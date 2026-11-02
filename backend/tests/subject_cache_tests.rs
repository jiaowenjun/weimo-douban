use axum::{
    body::{to_bytes, Body},
    extract::{ConnectInfo, State},
    http::{Method, Request, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use clap::Parser;
use serde_json::{json, Value};
use sqlx::Row;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tempfile::TempDir;
use tower::ServiceExt;
use weimo_douban::server::{
    config::ServerConfig,
    db,
    fetcher::{DoubanFetcher, FetchError},
    routes::build_app,
    state::AppState,
    subject,
    subject_cache::{BatchRequest, BatchResponse, CachedSubject, SubjectKey},
    subject_cache_client::{self, sync_once},
};

fn key(id: &str) -> SubjectKey {
    SubjectKey {
        category: "movie".into(),
        subject_id: id.into(),
    }
}
fn item(id: &str) -> CachedSubject {
    CachedSubject {
        key: key(id),
        title: format!("中心 {id}"),
        original_title: String::new(),
        year: Some(2020),
        aka: vec![],
        url: format!("https://movie.douban.com/subject/{id}/"),
        cover_url: "https://img1.doubanio.com/view/photo/s_ratio_poster/public/p1.webp".into(),
        rating_value: Some(8.0),
        rating_count: Some(10),
        meta: json!({"directors":["导演"],"intro":"简介"}),
        fetched_ms: ServerConfig::now_ms() - 60_000,
        status: "ok".into(),
        source: "rexxar".into(),
    }
}

async fn setup(url: Option<&str>, serve: bool) -> (TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = ServerConfig::try_parse_from(["test"]).unwrap();
    config.data_dir = dir.path().to_path_buf();
    config.subject_cache_url = url.map(str::to_owned);
    config.subject_cache_serve = serve;
    config.desktop_app = url.is_some();
    config.crawl_min_interval_secs = 0.0;
    config.subject_min_interval_secs = 0.0;
    config.rate_per_minute = 100_000;
    config.token = "admin-only".into();
    let pool = db::connect(&config.db_path()).await.unwrap();
    let state = AppState::new(config, pool);
    subject_cache_client::load_preferences(&state)
        .await
        .unwrap();
    (dir, state)
}

async fn seed(state: &AppState, id: &str) {
    sqlx::query("INSERT INTO records(uid,category,dim_status,subject_id,title) VALUES('user','movie','collect',?,?)")
        .bind(id).bind(format!("本地 {id}")).execute(&state.pool).await.unwrap();
    subject::enqueue_missing_subjects(&state.pool, "user", "movie")
        .await
        .unwrap();
}
async fn store(state: &AppState, id: &str, source: &str, time: i64) {
    sqlx::query("INSERT INTO subjects(category,subject_id,title,meta_json,source,fetched_ms) VALUES('movie',?,'已有条目','{}',?,?)
        ON CONFLICT(category,subject_id) DO UPDATE SET title=excluded.title,source=excluded.source,fetched_ms=excluded.fetched_ms")
        .bind(id).bind(source).bind(time).execute(&state.pool).await.unwrap();
}

fn request(method: Method, path: &str, body: Value, auth: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json");
    if auth {
        builder = builder.header("Authorization", "Bearer admin-only");
    }
    let mut req = builder.body(Body::from(body.to_string())).unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "192.0.2.77:1234".parse::<std::net::SocketAddr>().unwrap(),
    ));
    req
}
async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1_000_000).await.unwrap()).unwrap()
}

#[derive(Clone)]
enum Reply {
    Hits,
    Miss,
    Limited,
    Broken,
    Oversized,
}
#[derive(Clone)]
struct MockState {
    reply: Reply,
    delay: Duration,
    calls: Arc<AtomicUsize>,
}
struct Mock {
    url: String,
    calls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn mock(reply: Reply, delay: Duration) -> Mock {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = MockState {
        reply,
        delay,
        calls: calls.clone(),
    };
    async fn handler(
        State(state): State<MockState>,
        headers: axum::http::HeaderMap,
        Json(input): Json<BatchRequest>,
    ) -> axum::response::Response {
        assert!(!headers.contains_key("Cookie"));
        assert!(!headers.contains_key("Authorization"));
        state.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(state.delay).await;
        match state.reply {
            Reply::Hits => Json(BatchResponse {
                schema_version: 1,
                items: input.items.iter().map(|k| item(&k.subject_id)).collect(),
                missing: vec![],
            })
            .into_response(),
            Reply::Miss => Json(BatchResponse {
                schema_version: 1,
                items: vec![],
                missing: input.items,
            })
            .into_response(),
            Reply::Limited => (
                StatusCode::TOO_MANY_REQUESTS,
                [("Retry-After", "120")],
                "busy",
            )
                .into_response(),
            Reply::Broken => {
                Json(json!({"schema_version":1,"items":[],"missing":[]})).into_response()
            }
            Reply::Oversized => "x".repeat(300 * 1024).into_response(),
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/batch", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/batch", post(handler))
        .with_state(state);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Mock { url, calls, task }
}
async fn entered(mock: &Mock) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while mock.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn export_is_public_read_only_and_does_not_synthesize_or_enqueue() {
    let (_dir, state) = setup(None, true).await;
    store(&state, "1", "rexxar", ServerConfig::now_ms() - 1000).await;
    store(&state, "2", "intro", ServerConfig::now_ms() - 1000).await;
    let router = build_app(state.clone());
    let response = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/subjects/batch",
            json!({"items":[key("1"),key("2"),key("3"),key("1")]}),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["missing"].as_array().unwrap().len(), 2);
    assert!(body["items"][0].get("raw_json").is_none());
    assert!(body["items"][0].get("cover_url").is_some());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM subject_jobs")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    let write = sqlx::query("DELETE FROM subjects")
        .execute(&state.subject_cache.as_ref().unwrap().pool)
        .await;
    assert!(
        write.is_err(),
        "cache connection must be physically read-only"
    );
    let admin = router
        .oneshot(request(
            Method::POST,
            "/api/v1/subjects/backfill",
            json!({}),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(admin.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn export_rejects_invalid_and_oversized_inputs_and_is_off_by_default() {
    let (_dir, state) = setup(None, true).await;
    let router = build_app(state);
    for items in [vec![], vec![key("not-an-id")], vec![key("1"); 101]] {
        let response = router
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/v1/subjects/batch",
                json!({"items":items}),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let response = router
        .oneshot(request(
            Method::POST,
            "/api/v1/subjects/batch",
            json!({"items":[],"padding":"x".repeat(20_000)}),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let (_dir, state) = setup(None, false).await;
    let response = build_app(state)
        .oneshot(request(
            Method::POST,
            "/api/v1/subjects/batch",
            json!({"items":[key("1")]}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn center_hits_upgrade_intro_and_work_during_douban_cooldown() {
    let mock = mock(Reply::Hits, Duration::ZERO).await;
    let (_dir, state) = setup(Some(&mock.url), false).await;
    seed(&state, "1").await;
    seed(&state, "2").await;
    store(&state, "2", "intro", ServerConfig::now_ms()).await;
    let before = weimo_douban::server::cache::versions(&state, "user", "collect")
        .await
        .unwrap();
    sqlx::query(
        "UPDATE subject_jobs SET next_run_ms=?,attempts=1,retry_since_ms=1,last_error='HTTP 400'",
    )
    .bind(ServerConfig::now_ms() + 600_000)
    .execute(&state.pool)
    .await
    .unwrap();
    for _ in 0..5 {
        state.record_crawl_failure();
    }
    assert!(state.circuit_remaining_ms() > 0);
    assert_eq!(sync_once(&state).await.unwrap(), 2);
    let after = weimo_douban::server::cache::versions(&state, "user", "collect")
        .await
        .unwrap();
    assert_ne!(
        before.movie, after.movie,
        "import must invalidate the local UI projection"
    );
    assert!(
        state.circuit_remaining_ms() > 0,
        "center success must not reset Douban circuit"
    );
    let row = sqlx::query(
        "SELECT source,fetched_ms,cover_url,raw_json FROM subjects WHERE subject_id='2'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("source"), "rexxar");
    assert!(row.get::<i64, _>("fetched_ms") < ServerConfig::now_ms() - 50_000);
    assert!(row
        .get::<String, _>("cover_url")
        .starts_with("https://img1.doubanio.com/"));
    assert_eq!(row.get::<String, _>("raw_json"), "");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM subject_jobs WHERE state='ok'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(sync_once(&state).await.unwrap(), 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM subject_jobs WHERE attempts!=0 OR retry_since_ms IS NOT NULL OR run_token IS NOT NULL OR last_error IS NOT NULL")
            .fetch_one(&state.pool).await.unwrap(),
        0,
        "center cache completion clears the retry lifecycle too"
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
}

struct SourceFetcher(Arc<AtomicUsize>);
impl DoubanFetcher for SourceFetcher {
    async fn fetch_page(&self, _url: &str, _referer: &str) -> Result<String, FetchError> {
        Err(FetchError::NotFound)
    }
    async fn fetch_subject(&self, _category: &str, _id: &str) -> Result<Value, FetchError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"title":"豆瓣直抓"}))
    }
}

#[tokio::test]
async fn misses_and_center_faults_release_local_jobs_without_retrying_every_item() {
    for reply in [Reply::Miss, Reply::Limited, Reply::Broken, Reply::Oversized] {
        let mock = mock(reply, Duration::ZERO).await;
        let (_dir, state) = setup(Some(&mock.url), false).await;
        seed(&state, "1").await;
        assert_eq!(sync_once(&state).await.unwrap(), 1);
        assert_eq!(sync_once(&state).await.unwrap(), 0);
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        let row = sqlx::query("SELECT state,attempts FROM subject_jobs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("state"), "queued");
        assert_eq!(row.get::<i64, _>("attempts"), 0);
        assert_eq!(state.circuit_remaining_ms(), 0);
        let calls = Arc::new(AtomicUsize::new(0));
        assert!(
            subject::process_next_job(&state, &SourceFetcher(calls.clone()))
                .await
                .unwrap()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn center_offline_and_slow_response_do_not_stop_source_fetching() {
    let mock = mock(Reply::Hits, Duration::from_secs(10)).await;
    let (_dir, state) = setup(Some(&mock.url), false).await;
    seed(&state, "1").await;
    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(4), sync_once(&state))
        .await
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(4));
    let calls = Arc::new(AtomicUsize::new(0));
    assert!(
        subject::process_next_job(&state, &SourceFetcher(calls.clone()))
            .await
            .unwrap()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let offline_url = mock.url.clone();
    drop(mock);
    let (_dir, offline) = setup(Some(&offline_url), false).await;
    seed(&offline, "2").await;
    sync_once(&offline).await.unwrap();
    assert!(
        subject::process_next_job(&offline, &SourceFetcher(calls.clone()))
            .await
            .unwrap()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn disabling_cancels_inflight_query_and_persists_across_restart() {
    let mock = mock(Reply::Hits, Duration::from_secs(10)).await;
    let (_dir, state) = setup(Some(&mock.url), false).await;
    seed(&state, "1").await;
    let run_state = state.clone();
    let task = tokio::spawn(async move { sync_once(&run_state).await.unwrap() });
    entered(&mock).await;
    let response = build_app(state.clone())
        .oneshot(request(
            Method::PATCH,
            "/api/v1/app/subject-cache",
            json!({"enabled":false}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM subjects")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    let restarted = AppState::new(state.config.clone(), state.pool.clone());
    subject_cache_client::load_preferences(&restarted)
        .await
        .unwrap();
    assert!(!restarted.subject_cache_client.as_ref().unwrap().enabled());
    assert_eq!(sync_once(&restarted).await.unwrap(), 0);
    assert!(
        subject::process_next_job(&state, &SourceFetcher(Arc::new(AtomicUsize::new(0))))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn force_refresh_and_newer_local_data_win_over_inflight_import() {
    let mock = mock(Reply::Hits, Duration::from_millis(150)).await;
    let (_dir, state) = setup(Some(&mock.url), false).await;
    seed(&state, "1").await;
    seed(&state, "2").await;
    let run_state = state.clone();
    let task = tokio::spawn(async move { sync_once(&run_state).await.unwrap() });
    entered(&mock).await;
    subject::force_refresh(&state.pool, "movie", "1")
        .await
        .unwrap();
    store(&state, "2", "rexxar", ServerConfig::now_ms()).await;
    task.await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM subjects WHERE subject_id='1'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM subject_jobs WHERE subject_id='1'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "queued"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT title FROM subjects WHERE subject_id='2'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "已有条目"
    );
}

#[tokio::test]
async fn active_center_lease_prevents_duplicate_source_fetch_and_crash_lease_expires() {
    let mock = mock(Reply::Hits, Duration::from_millis(150)).await;
    let (_dir, state) = setup(Some(&mock.url), false).await;
    seed(&state, "1").await;
    let run_state = state.clone();
    let task = tokio::spawn(async move { sync_once(&run_state).await.unwrap() });
    entered(&mock).await;
    let calls = Arc::new(AtomicUsize::new(0));
    assert!(
        !subject::process_next_job(&state, &SourceFetcher(calls.clone()))
            .await
            .unwrap()
    );
    task.await.unwrap();
    assert!(
        !subject::process_next_job(&state, &SourceFetcher(calls.clone()))
            .await
            .unwrap()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    seed(&state, "2").await;
    sqlx::query("INSERT INTO subject_cache_checks VALUES('movie','2',0,?)")
        .bind(ServerConfig::now_ms() - 1)
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(
        subject::process_next_job(&state, &SourceFetcher(calls.clone()))
            .await
            .unwrap()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn thirty_clients_are_shed_without_consuming_web_rate_budget() {
    let (_dir, mut state) = setup(None, true).await;
    state.config.rate_per_minute = 60;
    store(&state, "1", "rexxar", ServerConfig::now_ms() - 1000).await;
    let router = build_app(state);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..30 {
        let router = router.clone();
        tasks.spawn(async move {
            let mut limited = 0;
            for _ in 0..20 {
                let response = router
                    .clone()
                    .oneshot(request(
                        Method::POST,
                        "/api/v1/subjects/batch",
                        json!({"items":[key("1")]}),
                        false,
                    ))
                    .await
                    .unwrap();
                assert!(matches!(
                    response.status(),
                    StatusCode::OK
                        | StatusCode::TOO_MANY_REQUESTS
                        | StatusCode::SERVICE_UNAVAILABLE
                ));
                if response.status() != StatusCode::OK {
                    limited += 1;
                }
            }
            limited
        });
    }
    for _ in 0..30 {
        let response = router
            .clone()
            .oneshot(request(
                Method::GET,
                "/api/v1/subjects/movie/1",
                json!(null),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "App cache must not consume website rate budget"
        );
    }
    let mut limited = 0;
    while let Some(result) = tasks.join_next().await {
        limited += result.unwrap();
    }
    assert!(limited > 500);
}

#[test]
fn desktop_defaults_respect_explicit_disable_and_validate_wire_data() {
    let mut config = ServerConfig::try_parse_from(["test", "--subject-cache-url", ""]).unwrap();
    config.configure_desktop();
    assert!(config.desktop_app);
    assert!(!config.subject_cache_serve);
    assert_eq!(config.subject_cache_url.as_deref(), Some(""));
    let mut value = item("1");
    value.meta = json!({"directors":"bad"});
    assert!(!value.valid(ServerConfig::now_ms()));
    value = item("1");
    value.cover_url = "http://127.0.0.1/private".into();
    assert!(!value.valid(ServerConfig::now_ms()));
}

#[tokio::test]
async fn scheduler_keeps_batch_pacing_even_when_source_worker_is_woken() {
    let mock = mock(Reply::Hits, Duration::ZERO).await;
    let (_dir, state) = setup(Some(&mock.url), false).await;
    for id in 1..=205 {
        seed(&state, &id.to_string()).await;
    }
    let task = tokio::spawn(subject_cache_client::run(state.clone()));
    entered(&mock).await;
    for _ in 0..30 {
        state.wake_worker();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        mock.calls.load(Ordering::SeqCst),
        1,
        "source wakeups must not defeat batch pacing"
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while mock.calls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
}

#[tokio::test]
async fn restart_with_center_disabled_resumes_at_expired_lease_without_waiting_a_minute() {
    let (_dir, state) = setup(None, false).await;
    seed(&state, "1").await;
    sqlx::query("INSERT INTO subject_cache_checks VALUES('movie','1',0,?)")
        .bind(ServerConfig::now_ms() + 150)
        .execute(&state.pool)
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(weimo_douban::server::worker::run_worker(
        state,
        SourceFetcher(calls.clone()),
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
}
