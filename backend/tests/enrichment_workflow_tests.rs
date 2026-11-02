//! Behavioral regression tests: lifecycle, destructive-operation races and category scheduling.
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use clap::Parser;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tower::ServiceExt;
use weimo_douban::server::{
    config::ServerConfig,
    db,
    fetcher::{DoubanFetcher, FetchError},
    routes::build_app,
    state::AppState,
    subject, subject_queue,
};

async fn setup() -> (TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = ServerConfig::try_parse_from(["test"]).unwrap();
    config.data_dir = dir.path().to_owned();
    config.crawl_min_interval_secs = 0.0;
    config.subject_min_interval_secs = 0.5;
    config.rate_per_minute = 10_000_000;
    let pool = db::connect(&config.db_path()).await.unwrap();
    (dir, AppState::new(config, pool))
}
async fn enqueue(s: &AppState, category: &str, id: &str, attempts: i64, created: i64) {
    sqlx::query("INSERT INTO subject_jobs(category,subject_id,state,attempts,created_ms,updated_ms,retry_since_ms) VALUES(?,?,'queued',?,?,?,?)")
        .bind(category).bind(id).bind(attempts).bind(created).bind(created).bind(if attempts>0 {Some(created)} else {None}).execute(&s.pool).await.unwrap();
}
async fn advance(s: &AppState) {
    sqlx::query("UPDATE subject_runtime SET next_request_ms=0,blocked_until_ms=0")
        .execute(&s.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE subject_jobs SET next_run_ms=NULL")
        .execute(&s.pool)
        .await
        .unwrap();
}
async fn scalar(s: &AppState, q: &str) -> i64 {
    sqlx::query_scalar(q).fetch_one(&s.pool).await.unwrap()
}
struct Fetch {
    calls: Arc<Mutex<Vec<String>>>,
    blocked: bool,
}
impl Fetch {
    fn ok() -> Self {
        Self {
            calls: Default::default(),
            blocked: false,
        }
    }
}
impl DoubanFetcher for Fetch {
    async fn fetch_page(&self, _: &str, _: &str) -> Result<String, FetchError> {
        unreachable!()
    }
    async fn fetch_subject(&self, c: &str, id: &str) -> Result<Value, FetchError> {
        self.calls.lock().unwrap().push(format!("{c}/{id}"));
        if self.blocked {
            Err(FetchError::Blocked(400))
        } else {
            Ok(json!({"title":id}))
        }
    }
}

#[tokio::test]
async fn pause_survives_restart_and_covers_new_forced_jobs() {
    let (_dir, s) = setup().await;
    subject_queue::set_paused(&s, "movie", true).await.unwrap();
    subject::force_refresh(&s.pool, "movie", "1").await.unwrap();
    let restarted = AppState::new(s.config.clone(), s.pool.clone());
    let fetch = Fetch::ok();
    assert!(!subject::process_next_job(&restarted, &fetch).await.unwrap());
    enqueue(&s, "book", "2", 0, 0).await;
    assert!(subject::process_next_job(&restarted, &fetch).await.unwrap());
    assert_eq!(*fetch.calls.lock().unwrap(), vec!["book/2"]);
    subject_queue::set_paused(&s, "*", true).await.unwrap();
    subject_queue::set_paused(&s, "movie", false).await.unwrap();
    advance(&s).await;
    assert!(!subject::process_next_job(&s, &fetch).await.unwrap());
    subject_queue::set_paused(&s, "*", false).await.unwrap();
    assert!(subject::process_next_job(&s, &fetch).await.unwrap());
}

#[tokio::test]
async fn cooldown_covers_new_jobs_and_force_refresh_after_restart() {
    let (_dir, s) = setup().await;
    enqueue(&s, "movie", "1", 0, 0).await;
    let fetch = Fetch {
        blocked: true,
        ..Fetch::ok()
    };
    assert!(subject::process_next_job(&s, &fetch).await.unwrap());
    subject::force_refresh(&s.pool, "movie", "2").await.unwrap();
    sqlx::query("UPDATE subject_runtime SET next_request_ms=0 WHERE category='*'")
        .execute(&s.pool)
        .await
        .unwrap();
    let restarted = AppState::new(s.config.clone(), s.pool.clone());
    assert!(!subject::process_next_job(&restarted, &fetch).await.unwrap());
    enqueue(&s, "book", "3", 0, 0).await;
    let ok = Fetch::ok();
    assert!(subject::process_next_job(&restarted, &ok).await.unwrap());
    assert_eq!(*ok.calls.lock().unwrap(), vec!["book/3"]);
    assert!(subject_queue::next_delay_ms(&s).await.unwrap().unwrap() > 0);
}

#[tokio::test]
async fn retries_get_bounded_priority_without_starving_first_attempts() {
    let (_dir, s) = setup().await;
    enqueue(&s, "movie", "new", 0, 1).await;
    for n in 0..5 {
        enqueue(&s, "movie", &format!("retry{n}"), 1, 100 + n).await;
    }
    let fetch = Fetch::ok();
    for _ in 0..4 {
        advance(&s).await;
        assert!(subject::process_next_job(&s, &fetch).await.unwrap());
    }
    let calls = fetch.calls.lock().unwrap();
    assert_eq!(calls[0], "movie/retry0");
    assert_eq!(calls[1], "movie/retry1");
    assert_eq!(calls[2], "movie/new");
    assert_eq!(calls[3], "movie/retry2");
}

#[tokio::test]
async fn clear_snapshot_restores_retry_state_and_preserves_newer_work() {
    let (_dir, s) = setup().await;
    enqueue(&s, "movie", "retry", 2, 100).await;
    enqueue(&s, "movie", "fresh", 0, 1).await;
    sqlx::query("UPDATE subject_jobs SET next_run_ms=9999999999999,last_error='HTTP 400' WHERE subject_id='retry'").execute(&s.pool).await.unwrap();
    let result = subject_queue::clear(&s.pool, &["queued".into()], None, None)
        .await
        .unwrap();
    assert_eq!(result["deleted"], 2);
    assert_eq!(result["retrying"], 1);
    assert_eq!(scalar(&s, "SELECT COUNT(*) FROM subject_jobs").await, 0);
    subject::force_refresh(&s.pool, "movie", "fresh")
        .await
        .unwrap();
    let id = result["operation_id"].as_str().unwrap();
    let result = subject_queue::restore(&s, id).await.unwrap();
    assert_eq!(result["restored"], 1);
    let row:(i64,i64,i64,String)=sqlx::query_as("SELECT attempts,retry_since_ms,next_run_ms,last_error FROM subject_jobs WHERE subject_id='retry'").fetch_one(&s.pool).await.unwrap();
    assert_eq!(row, (2, 100, 9999999999999, "HTTP 400".into()));
    assert_eq!(
        scalar(
            &s,
            "SELECT priority FROM subject_jobs WHERE subject_id='fresh'"
        )
        .await,
        0
    );
    assert_eq!(subject_queue::restore(&s, id).await.unwrap()["restored"], 0);
}

struct InFlight {
    started: Arc<tokio::sync::Notify>,
    finish: Arc<tokio::sync::Notify>,
}
impl DoubanFetcher for InFlight {
    async fn fetch_page(&self, _: &str, _: &str) -> Result<String, FetchError> {
        unreachable!()
    }
    async fn fetch_subject(&self, _: &str, _: &str) -> Result<Value, FetchError> {
        self.started.notify_one();
        self.finish.notified().await;
        Ok(json!({"title":"completed in flight"}))
    }
}

#[tokio::test]
async fn in_flight_result_commits_but_cannot_complete_a_replacement_job() {
    let (_dir, s) = setup().await;
    enqueue(&s, "movie", "1", 0, 0).await;
    let started = Arc::new(tokio::sync::Notify::new());
    let finish = Arc::new(tokio::sync::Notify::new());
    let fetch = InFlight {
        started: started.clone(),
        finish: finish.clone(),
    };
    let worker = s.clone();
    let task =
        tokio::spawn(async move { subject::process_next_job(&worker, &fetch).await.unwrap() });
    started.notified().await;
    // A force refresh may replace the running row while its old HTTP request is still active.
    subject::force_refresh(&s.pool, "movie", "1").await.unwrap();
    let clear = subject_queue::clear(&s.pool, &["queued".into(), "running".into()], None, None)
        .await
        .unwrap();
    assert_eq!(clear["in_flight"], 1);
    let id = clear["operation_id"].as_str().unwrap();
    assert_eq!(
        subject_queue::restore(&s, id).await.unwrap()["in_flight"],
        1
    );
    subject::force_refresh(&s.pool, "movie", "1").await.unwrap();
    finish.notify_one();
    assert!(task.await.unwrap());
    assert_eq!(scalar(&s, "SELECT COUNT(*) FROM subjects").await, 1);
    let status: String = sqlx::query_scalar("SELECT state FROM subject_jobs")
        .fetch_one(&s.pool)
        .await
        .unwrap();
    assert_eq!(status, "queued", "old run token must not finish new job");
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM subject_attempts WHERE outcome='success'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn success_is_durable_before_pacing_and_history_survives_clear() {
    let (_dir, mut s) = setup().await;
    s.config.subject_movie_min_interval_secs = Some(60.0);
    enqueue(&s, "movie", "1", 1, 1).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        subject::process_next_job(&s, &Fetch::ok()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(scalar(&s, "SELECT COUNT(*) FROM subjects").await, 1);
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM subject_jobs WHERE state='ok' AND attempts=0"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(&s, "SELECT recovered FROM subject_attempts").await,
        1
    );
    subject_queue::clear(&s.pool, &["ok".into()], None, None)
        .await
        .unwrap();
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM subject_attempts WHERE outcome='success'"
        )
        .await,
        1
    );
    assert!(
        scalar(
            &s,
            "SELECT next_request_ms FROM subject_runtime WHERE category='movie'"
        )
        .await
            > ServerConfig::now_ms() + 50_000
    );
}

#[tokio::test]
async fn failed_commit_rolls_back_subject_job_and_attempt_together() {
    let (_dir, s) = setup().await;
    enqueue(&s, "movie", "1", 0, 0).await;
    sqlx::query("CREATE TRIGGER fail_job_commit BEFORE UPDATE OF state ON subject_jobs WHEN NEW.state='ok' BEGIN SELECT RAISE(ABORT,'test failure'); END").execute(&s.pool).await.unwrap();
    assert!(subject::process_next_job(&s, &Fetch::ok()).await.is_err());
    assert_eq!(scalar(&s, "SELECT COUNT(*) FROM subjects").await, 0);
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM subject_attempts WHERE outcome='started'"
        )
        .await,
        1
    );
    subject_queue::recover_interrupted(&s.pool).await.unwrap();
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM subject_attempts WHERE outcome='interrupted'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn restored_pacing_has_ttl_and_respects_current_movie_floor() {
    let (_dir, mut s) = setup().await;
    s.config.subject_movie_min_interval_secs = Some(45.0);
    s.config.subject_movie_initial_interval_secs = Some(60.0);
    sqlx::query("UPDATE subject_runtime SET interval_secs=80,pacing_updated_ms=?,blocked_until_ms=? WHERE category='movie'")
        .bind(ServerConfig::now_ms()).bind(ServerConfig::now_ms()+300_000).execute(&s.pool).await.unwrap();
    subject_queue::restore_pacing(&s, "movie").await.unwrap();
    assert_eq!(s.subject_interval("movie"), 80.0);
    sqlx::query("UPDATE subject_runtime SET pacing_updated_ms=1 WHERE category='movie'")
        .execute(&s.pool)
        .await
        .unwrap();
    subject_queue::restore_pacing(&s, "movie").await.unwrap();
    assert_eq!(s.subject_interval("movie"), 60.0);
    assert!(
        scalar(
            &s,
            "SELECT blocked_until_ms FROM subject_runtime WHERE category='movie'"
        )
        .await
            > ServerConfig::now_ms()
    );
    assert_eq!(s.config.subject_floor("book"), 0.5);
}

#[tokio::test]
async fn adaptive_cooldown_only_grows_after_consecutive_blocked_probes() {
    let (_dir, mut s) = setup().await;
    s.config.subject_adaptive_cooldown = true;
    enqueue(&s, "movie", "1", 0, 0).await;
    let blocked = Fetch {
        blocked: true,
        ..Fetch::ok()
    };
    subject::process_next_job(&s, &blocked).await.unwrap();
    let first = scalar(
        &s,
        "SELECT blocked_until_ms FROM subject_runtime WHERE category='movie'",
    )
    .await
        - ServerConfig::now_ms();
    advance(&s).await;
    subject::process_next_job(&s, &blocked).await.unwrap();
    let second = scalar(
        &s,
        "SELECT blocked_until_ms FROM subject_runtime WHERE category='movie'",
    )
    .await
        - ServerConfig::now_ms();
    assert!((299_000..331_000).contains(&first));
    assert!((599_000..631_000).contains(&second));
    advance(&s).await;
    subject::process_next_job(&s, &Fetch::ok()).await.unwrap();
    assert_eq!(
        scalar(
            &s,
            "SELECT block_streak FROM subject_runtime WHERE category='movie'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn overview_reports_unsubscribed_gaps_and_persistent_recovery() {
    let (_dir, s) = setup().await;
    sqlx::query("INSERT INTO records(uid,category,dim_status,subject_id) VALUES('unsubscribed','movie','collect','1'),('unsubscribed','movie','collect','2')").execute(&s.pool).await.unwrap();
    enqueue(&s, "movie", "1", 1, 1).await;
    subject::process_next_job(&s, &Fetch::ok()).await.unwrap();
    let view = subject_queue::overview(&s).await.unwrap();
    assert_eq!(view["coverage"][0]["missing"], 1);
    assert_eq!(view["coverage"][0]["unqueued"], 1);
    assert_eq!(view["coverage"][0]["subscribed"], false);
    let movie = view["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["category"] == "movie")
        .unwrap();
    assert!(movie["history"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["recovered"] == true));
}

#[tokio::test]
async fn controls_validate_auth_and_category_and_overview_routes_resolve() {
    let (_dir, mut s) = setup().await;
    s.config.token = "secret".into();
    let app = build_app(s);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/subjects/jobs/overview")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/subjects/jobs/overview")
                .header("Authorization", "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["categories"].as_array().unwrap().len(), 3);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/subjects/jobs/control")
                .header("Authorization", "Bearer secret")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"category":"invalid","paused":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn requeue_all_aliases_preserve_pause_and_cooldown() {
    let (_dir, s) = setup().await;
    subject_queue::set_paused(&s, "*", true).await.unwrap();
    let until = ServerConfig::now_ms() + 300_000;
    sqlx::query("UPDATE subject_runtime SET blocked_until_ms=? WHERE category='movie'")
        .bind(until)
        .execute(&s.pool)
        .await
        .unwrap();
    for category in ["movie", "book"] {
        enqueue(&s, category, "1", 2, 1).await;
    }
    for filter in [None, Some("all"), Some("")] {
        sqlx::query("UPDATE subject_jobs SET state='error',attempts=2,retry_since_ms=1,last_error='HTTP 400'")
            .execute(&s.pool).await.unwrap();
        assert_eq!(
            subject::requeue_jobs(&s, "failed", filter).await.unwrap(),
            2
        );
        assert_eq!(scalar(&s, "SELECT COUNT(*) FROM subject_jobs WHERE state='queued' AND attempts=0 AND retry_since_ms IS NULL AND last_error IS NULL").await, 2);
        let fetch = Fetch::ok();
        assert!(!subject::process_next_job(&s, &fetch).await.unwrap());
        assert!(fetch.calls.lock().unwrap().is_empty());
    }
    sqlx::query("UPDATE subject_jobs SET state='error'")
        .execute(&s.pool)
        .await
        .unwrap();
    assert_eq!(
        subject::requeue_jobs(&s, "failed", Some("book"))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM subject_jobs WHERE category='movie' AND state='error'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT blocked_until_ms FROM subject_runtime WHERE category='movie'"
        )
        .await,
        until
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM subject_operations WHERE kind='requeue'"
        )
        .await,
        4
    );

    sqlx::query("INSERT INTO subjects(category,subject_id,title,fetched_ms) VALUES('music','stale','old',0)")
        .execute(&s.pool).await.unwrap();
    assert_eq!(
        subject::requeue_jobs(&s, "stale", Some("all"))
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn schema_upgrade_is_idempotent_and_keeps_existing_queue() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE subject_jobs(category TEXT,subject_id TEXT,state TEXT,attempts INTEGER,next_run_ms INTEGER,priority INTEGER,created_ms INTEGER,updated_ms INTEGER,last_error TEXT,PRIMARY KEY(category,subject_id))").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO subject_jobs VALUES('movie','1','queued',2,999,1,100,200,'HTTP 400')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    for _ in 0..2 {
        let pool = db::connect(&path).await.unwrap();
        let row: (i64, i64, String) =
            sqlx::query_as("SELECT attempts,retry_since_ms,last_error FROM subject_jobs")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row, (2, 200, "HTTP 400".into()));
        pool.close().await;
    }
}

#[tokio::test]
async fn cached_job_does_not_reserve_request_budget_or_count_as_http_attempt() {
    let (_dir, s) = setup().await;
    sqlx::query(
        "INSERT INTO subjects(category,subject_id,title,fetched_ms) VALUES('movie','1','cached',1)",
    )
    .execute(&s.pool)
    .await
    .unwrap();
    enqueue(&s, "movie", "1", 0, 0).await;
    let fetch = Fetch::ok();
    assert!(subject::process_next_job(&s, &fetch).await.unwrap());
    assert!(fetch.calls.lock().unwrap().is_empty());
    assert_eq!(
        scalar(&s, "SELECT SUM(next_request_ms) FROM subject_runtime").await,
        0
    );
    let view = subject_queue::overview(&s).await.unwrap();
    let movie = view["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["category"] == "movie")
        .unwrap();
    assert_eq!(movie["last_hour"]["attempts"], 0);
}

#[tokio::test]
async fn snapshot_failure_keeps_queue_and_rolls_back_audit() {
    let (_dir, s) = setup().await;
    enqueue(&s, "movie", "1", 1, 0).await;
    sqlx::query("CREATE TRIGGER fail_snapshot BEFORE INSERT ON subject_job_snapshots BEGIN SELECT RAISE(ABORT,'snapshot failure'); END").execute(&s.pool).await.unwrap();
    assert!(
        subject_queue::clear(&s.pool, &["queued".into()], None, None)
            .await
            .is_err()
    );
    assert_eq!(scalar(&s, "SELECT COUNT(*) FROM subject_jobs").await, 1);
    assert_eq!(
        scalar(&s, "SELECT COUNT(*) FROM subject_operations").await,
        0
    );
}

#[tokio::test]
async fn exhausted_force_refresh_preserves_authoritative_cache_and_honest_history() {
    let (_dir, s) = setup().await;
    sqlx::query("INSERT INTO records(uid,category,dim_status,subject_id,title) VALUES('u','movie','collect','1','old')").execute(&s.pool).await.unwrap();
    sqlx::query("INSERT INTO subjects(category,subject_id,title,fetched_ms) VALUES('movie','1','authoritative',1)").execute(&s.pool).await.unwrap();
    subject::force_refresh(&s.pool, "movie", "1").await.unwrap();
    sqlx::query("UPDATE subject_jobs SET attempts=4")
        .execute(&s.pool)
        .await
        .unwrap();
    subject::process_next_job(
        &s,
        &Fetch {
            blocked: true,
            ..Fetch::ok()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM subjects WHERE source='rexxar' AND title='authoritative'"
        )
        .await,
        1
    );
    let history = subject_queue::history(&s.pool, Some("movie"), Some("1"), 1, 10)
        .await
        .unwrap();
    assert_eq!(history["items"][0]["outcome"], "retained");
    assert_eq!(history["items"][0]["http_status"], 400);
    assert_eq!(
        subject_queue::history(&s.pool, Some("book"), Some("1"), 1, 10)
            .await
            .unwrap()["total"],
        0
    );
}
