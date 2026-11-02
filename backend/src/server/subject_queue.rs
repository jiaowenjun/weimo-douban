//! Durable enrichment controls, attempt history and reversible queue operations.
use anyhow::Result;
use rand::Rng;
use serde_json::{json, Value};
use sqlx::{Row, SqliteConnection, SqlitePool};

use super::{
    config::ServerConfig,
    error::{AppError, AppResult},
    state::AppState,
};

pub fn operation_id() -> String {
    format!(
        "enrich_{:x}_{:016x}",
        ServerConfig::now_ms(),
        rand::thread_rng().gen::<u64>()
    )
}

pub async fn restore_pacing(state: &AppState, category: &str) -> Result<()> {
    let row =
        sqlx::query("SELECT interval_secs,pacing_updated_ms FROM subject_runtime WHERE category=?")
            .bind(category)
            .fetch_one(&state.pool)
            .await?;
    let saved: Option<f64> = row.get("interval_secs");
    let updated: i64 = row.get("pacing_updated_ms");
    let age = ServerConfig::now_ms() - updated;
    let value = saved
        .filter(|v| {
            v.is_finite()
                && age >= 0
                && age
                    <= state
                        .config
                        .subject_pacing_ttl_secs
                        .max(0)
                        .saturating_mul(1000)
        })
        .map(|v| v.clamp(state.config.subject_floor(category), 90.0))
        .unwrap_or_else(|| state.config.subject_initial(category));
    state.restore_subject_interval(category, value);
    Ok(())
}

/// Serializes with claims, so after this commits no new request may be claimed.
pub async fn set_paused(state: &AppState, category: &str, paused: bool) -> Result<Value> {
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("UPDATE subject_runtime SET paused=? WHERE category=?")
        .bind(paused)
        .bind(category)
        .execute(&mut *tx)
        .await?;
    let inflight: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM subject_attempts WHERE outcome='started' AND (?='*' OR category=?)",
    )
    .bind(category)
    .bind(category)
    .fetch_one(&mut *tx)
    .await?;
    let id = operation_id();
    let counts = json!({"in_flight":inflight});
    audit(
        &mut tx,
        &id,
        if paused { "pause" } else { "resume" },
        category,
        &json!({}),
        &counts,
    )
    .await?;
    tx.commit().await?;
    state.wake_worker();
    tracing::info!(operation_id=%id,category,paused,in_flight=inflight,"enrichment control updated");
    Ok(json!({"operation_id":id,"category":category,"paused":paused,"in_flight":inflight}))
}

pub async fn audit(
    conn: &mut SqliteConnection,
    id: &str,
    kind: &str,
    category: &str,
    filters: &Value,
    counts: &Value,
) -> Result<()> {
    sqlx::query("INSERT INTO subject_operations(id,kind,category,filters_json,counts_json,created_ms) VALUES(?,?,?,?,?,?)")
        .bind(id).bind(kind).bind(category).bind(filters.to_string()).bind(counts.to_string())
        .bind(ServerConfig::now_ms()).execute(conn).await?;
    Ok(())
}

/// Snapshot and deletion use the same write transaction and exactly the same keys.
pub async fn clear(
    pool: &SqlitePool,
    states: &[String],
    category: Option<&str>,
    subject_id: Option<&str>,
) -> Result<Value> {
    let cat = category
        .filter(|c| !c.is_empty() && *c != "all")
        .unwrap_or("*");
    let id = operation_id();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let slots = vec!["?"; states.len()].join(",");
    let query=format!("SELECT category,state,attempts,COUNT(*) AS n FROM subject_jobs WHERE state IN ({slots}) AND (?='*' OR category=?) AND (? IS NULL OR subject_id=?) GROUP BY category,state,attempts");
    let mut q = sqlx::query(&query);
    for s in states {
        q = q.bind(s);
    }
    let rows = q
        .bind(cat)
        .bind(cat)
        .bind(subject_id)
        .bind(subject_id)
        .fetch_all(&mut *tx)
        .await?;
    let (mut deleted, mut retrying, mut running) = (0i64, 0i64, 0i64);
    let mut by_category = std::collections::BTreeMap::<String, i64>::new();
    for r in rows {
        let n: i64 = r.get("n");
        let st: String = r.get("state");
        let attempts: i64 = r.get("attempts");
        deleted += n;
        if st == "queued" && attempts > 0 {
            retrying += n;
        }
        if st == "running" {
            running += n;
        }
        *by_category.entry(r.get("category")).or_default() += n;
    }
    let inflight_query=format!("SELECT COUNT(*) FROM subject_attempts a WHERE a.outcome='started' AND EXISTS(SELECT 1 FROM subject_jobs j WHERE j.category=a.category AND j.subject_id=a.subject_id AND j.state IN ({slots}) AND (?='*' OR j.category=?) AND (? IS NULL OR j.subject_id=?))");
    let mut q = sqlx::query_scalar(&inflight_query);
    for st in states {
        q = q.bind(st);
    }
    let in_flight: i64 = q
        .bind(cat)
        .bind(cat)
        .bind(subject_id)
        .bind(subject_id)
        .fetch_one(&mut *tx)
        .await?;
    let counts = json!({"deleted":deleted,"retrying":retrying,"running":running,"in_flight":in_flight,"by_category":by_category});
    audit(
        &mut tx,
        &id,
        "clear",
        cat,
        &json!({"states":states,"subject_id":subject_id}),
        &counts,
    )
    .await?;
    let query=format!("INSERT INTO subject_job_snapshots SELECT ?,category,subject_id,state,attempts,next_run_ms,priority,created_ms,updated_ms,last_error,retry_since_ms FROM subject_jobs WHERE state IN ({slots}) AND (?='*' OR category=?) AND (? IS NULL OR subject_id=?)");
    let mut q = sqlx::query(&query).bind(&id);
    for s in states {
        q = q.bind(s);
    }
    q.bind(cat)
        .bind(cat)
        .bind(subject_id)
        .bind(subject_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM subject_jobs WHERE EXISTS(SELECT 1 FROM subject_job_snapshots s WHERE s.operation_id=? AND s.category=subject_jobs.category AND s.subject_id=subject_jobs.subject_id)")
        .bind(&id).execute(&mut *tx).await?;
    tx.commit().await?;
    tracing::info!(operation_id=%id,states=?states,category=cat,deleted,retrying,running,in_flight,by_category=?by_category,"enrichment jobs cleared");
    Ok(
        json!({"operation_id":id,"deleted":deleted,"retrying":retrying,"in_flight":in_flight,"by_category":by_category}),
    )
}

pub async fn restore(state: &AppState, id: &str) -> AppResult<Value> {
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let operation = sqlx::query(
        "SELECT created_ms,restored_ms FROM subject_operations WHERE id=? AND kind='clear'",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::InvalidArgument("找不到可恢复的清空快照".into()))?;
    let restored: Option<i64> = operation.get("restored_ms");
    if restored.is_some() {
        return Ok(json!({"restored":0,"already_restored":true}));
    }
    // Never overwrite a newer job, resurrect an in-flight attempt, or replace a newer result.
    let n=sqlx::query("INSERT OR IGNORE INTO subject_jobs(category,subject_id,state,attempts,next_run_ms,priority,created_ms,updated_ms,last_error,retry_since_ms)
        SELECT s.category,s.subject_id,CASE WHEN s.state='running' THEN 'queued' ELSE s.state END,s.attempts,s.next_run_ms,s.priority,s.created_ms,s.updated_ms,s.last_error,s.retry_since_ms
        FROM subject_job_snapshots s WHERE s.operation_id=?
        AND NOT EXISTS(SELECT 1 FROM subject_attempts a WHERE a.category=s.category AND a.subject_id=s.subject_id AND a.outcome='started')
        AND NOT EXISTS(SELECT 1 FROM subjects v WHERE v.category=s.category AND v.subject_id=s.subject_id AND (s.priority!=0 OR v.fetched_ms>=?))")
        .bind(id).bind(operation.get::<i64,_>("created_ms")).execute(&mut *tx).await?.rows_affected();
    let in_flight:i64=sqlx::query_scalar("SELECT COUNT(*) FROM subject_job_snapshots s WHERE operation_id=? AND EXISTS(SELECT 1 FROM subject_attempts a WHERE a.category=s.category AND a.subject_id=s.subject_id AND a.outcome='started')")
        .bind(id).fetch_one(&mut *tx).await?;
    // Keep the snapshot available if an attempt still has an unknown outcome.
    if in_flight == 0 {
        sqlx::query("UPDATE subject_operations SET restored_ms=? WHERE id=?")
            .bind(ServerConfig::now_ms())
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    let restore_id = operation_id();
    audit(
        &mut tx,
        &restore_id,
        "restore",
        "*",
        &json!({"snapshot_id":id}),
        &json!({"restored":n,"in_flight":in_flight}),
    )
    .await?;
    tx.commit().await?;
    state.wake_worker();
    tracing::info!(operation_id=%restore_id,snapshot_id=id,restored=n,in_flight,"enrichment snapshot restored");
    Ok(json!({"restored":n,"in_flight":in_flight,"operation_id":restore_id}))
}

pub async fn overview(state: &AppState) -> Result<Value> {
    let now = ServerConfig::now_ms();
    let mut categories = Vec::new();
    for category in ["book", "movie", "music"] {
        let r=sqlx::query("SELECT c.*,g.paused AS global_paused,MAX(c.next_request_ms,c.blocked_until_ms,g.next_request_ms) AS allowed_ms FROM subject_runtime c CROSS JOIN subject_runtime g WHERE c.category=? AND g.category='*'")
            .bind(category).fetch_one(&state.pool).await?;
        let paused = r.get::<i64, _>("paused") != 0
            || r.get::<i64, _>("global_paused") != 0
            || !state.config.subject_enrich;
        let allowed: i64 = r.get("allowed_ms");
        let q=sqlx::query("SELECT COUNT(*) AS pending,COALESCE(SUM(state='running'),0) AS running,COALESCE(SUM(state='queued' AND attempts=0),0) AS first_waiting,
          COALESCE(SUM(state='queued' AND attempts>0),0) AS retry_waiting,
          COALESCE(SUM(state='queued' AND attempts>0 AND MAX(COALESCE(next_run_ms,0),?,CASE WHEN j.priority=0 THEN 0 ELSE COALESCE(x.hold_until_ms,0) END)<=?),0) AS retry_due,
          MIN(CASE WHEN attempts>0 THEN retry_since_ms END) AS oldest_retry,
          MIN(CASE WHEN state='queued' AND attempts>0 THEN MAX(COALESCE(next_run_ms,0),?,CASE WHEN j.priority=0 THEN 0 ELSE COALESCE(x.hold_until_ms,0) END) END) AS oldest_due
          FROM subject_jobs j LEFT JOIN subject_cache_checks x ON x.category=j.category AND x.subject_id=j.subject_id WHERE j.category=? AND state IN ('queued','running')")
            .bind(allowed).bind(now).bind(allowed).bind(category).fetch_one(&state.pool).await?;
        let history=sqlx::query("SELECT outcome,http_status,recovered,COUNT(*) AS n FROM subject_attempts WHERE category=? GROUP BY outcome,http_status,recovered")
            .bind(category).fetch_all(&state.pool).await?;
        let history:Vec<Value>=history.into_iter().map(|h|json!({"outcome":h.get::<String,_>("outcome"),"http_status":h.get::<Option<i64>,_>("http_status"),"recovered":h.get::<i64,_>("recovered")!=0,"count":h.get::<i64,_>("n")})).collect();
        let window=sqlx::query("SELECT COUNT(*) AS attempts,COALESCE(SUM(outcome='success'),0) AS successes,COALESCE(SUM(http_status=400),0) AS http_400 FROM subject_attempts WHERE category=? AND started_ms>=? AND outcome!='cached'")
            .bind(category).bind(now-3_600_000).fetch_one(&state.pool).await?;
        let terminal=sqlx::query("SELECT COALESCE(SUM(state='error'),0) AS errors,COALESCE(SUM(state='not_found'),0) AS not_found FROM subject_jobs WHERE category=?")
            .bind(category).fetch_one(&state.pool).await?;
        categories.push(json!({"category":category,"paused":paused,"category_paused":r.get::<i64,_>("paused")!=0,
            "global_paused":r.get::<i64,_>("global_paused")!=0,"blocked_until_ms":r.get::<i64,_>("blocked_until_ms"),"next_request_ms":allowed,
            "interval_secs":r.get::<Option<f64>,_>("interval_secs"),"floor_secs":state.config.subject_floor(category),
            "pending":q.get::<i64,_>("pending"),"running":q.get::<i64,_>("running"),"first_waiting":q.get::<i64,_>("first_waiting"),
            "retry_waiting":q.get::<i64,_>("retry_waiting"),"retry_due":if paused {0} else {q.get::<i64,_>("retry_due")},
            "retry_wait_ms":q.get::<Option<i64>,_>("oldest_retry").map(|t|(now-t).max(0)),
            "dispatch_delay_ms":if paused {None} else {q.get::<Option<i64>,_>("oldest_due").map(|t|(now-t).max(0))},
            "errors":terminal.get::<i64,_>("errors"),"not_found":terminal.get::<i64,_>("not_found"),"history":history,
            "last_hour":{"attempts":window.get::<i64,_>("attempts"),"successes":window.get::<i64,_>("successes"),"http_400":window.get::<i64,_>("http_400")}}));
    }
    let coverage=sqlx::query("SELECT r.uid,r.category,COUNT(DISTINCT r.subject_id) AS total,
        COUNT(DISTINCT CASE WHEN s.source='rexxar' THEN r.subject_id END) AS enriched,
        COUNT(DISTINCT CASE WHEN s.source='intro' THEN r.subject_id END) AS fallback,
        COUNT(DISTINCT CASE WHEN s.subject_id IS NULL THEN r.subject_id END) AS missing,
        COUNT(DISTINCT CASE WHEN s.subject_id IS NULL AND j.state IN ('queued','running') THEN r.subject_id END) AS queued,
        COUNT(DISTINCT CASE WHEN s.subject_id IS NULL AND j.subject_id IS NULL THEN r.subject_id END) AS unqueued,
        COALESCE(sub.enabled,0) AS subscribed,sub.next_full_ms
        FROM records r LEFT JOIN subjects s USING(category,subject_id) LEFT JOIN subject_jobs j USING(category,subject_id)
        LEFT JOIN subscriptions sub ON sub.uid=r.uid GROUP BY r.uid,r.category ORDER BY missing DESC,r.uid,r.category")
        .fetch_all(&state.pool).await?;
    let coverage:Vec<Value>=coverage.into_iter().map(|r|json!({"uid":r.get::<String,_>("uid"),"category":r.get::<String,_>("category"),"total":r.get::<i64,_>("total"),"enriched":r.get::<i64,_>("enriched"),"fallback":r.get::<i64,_>("fallback"),"missing":r.get::<i64,_>("missing"),"queued":r.get::<i64,_>("queued"),"unqueued":r.get::<i64,_>("unqueued"),"subscribed":r.get::<i64,_>("subscribed")!=0,"next_full_ms":r.get::<Option<i64>,_>("next_full_ms")})).collect();
    let operations = sqlx::query(
        "SELECT * FROM subject_operations ORDER BY created_ms DESC,rowid DESC LIMIT 30",
    )
    .fetch_all(&state.pool)
    .await?;
    let operations:Vec<Value>=operations.into_iter().map(|r|json!({"id":r.get::<String,_>("id"),"kind":r.get::<String,_>("kind"),"category":r.get::<String,_>("category"),"created_ms":r.get::<i64,_>("created_ms"),"restored_ms":r.get::<Option<i64>,_>("restored_ms"),"counts":serde_json::from_str::<Value>(&r.get::<String,_>("counts_json")).unwrap_or(json!({}))})).collect();
    let since: Option<i64> = sqlx::query_scalar("SELECT MIN(started_ms) FROM subject_attempts")
        .fetch_one(&state.pool)
        .await?;
    Ok(
        json!({"enabled":state.config.subject_enrich,"now_ms":now,"history_since_ms":since,"categories":categories,"coverage":coverage,"operations":operations}),
    )
}

/// Called before starting the single worker, never while requests are running.
pub async fn recover_interrupted(pool: &SqlitePool) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query(
        "UPDATE subject_attempts SET outcome='interrupted',finished_ms=? WHERE outcome='started'",
    )
    .bind(ServerConfig::now_ms())
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE subject_jobs SET state='queued',run_token=NULL WHERE state='running'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn global_delay_ms(pool: &SqlitePool) -> Result<i64> {
    let until: i64 =
        sqlx::query_scalar("SELECT next_request_ms FROM subject_runtime WHERE category='*'")
            .fetch_one(pool)
            .await?;
    Ok((until - ServerConfig::now_ms()).max(0))
}

pub async fn next_delay_ms(state: &AppState) -> Result<Option<i64>> {
    if !state.config.subject_enrich {
        return Ok(None);
    }
    let until:Option<i64>=sqlx::query_scalar("SELECT MIN(MAX(COALESCE(j.next_run_ms,0),c.next_request_ms,c.blocked_until_ms,g.next_request_ms,CASE WHEN j.priority=0 THEN 0 ELSE COALESCE(x.hold_until_ms,0) END))
        FROM subject_jobs j JOIN subject_runtime c ON c.category=j.category CROSS JOIN subject_runtime g
        LEFT JOIN subject_cache_checks x ON x.category=j.category AND x.subject_id=j.subject_id
        WHERE g.category='*' AND c.paused=0 AND g.paused=0 AND j.state IN ('queued','running')")
        .fetch_one(&state.pool).await?;
    Ok(until.map(|t| (t - ServerConfig::now_ms()).max(1)))
}

pub async fn history(
    pool: &SqlitePool,
    category: Option<&str>,
    subject_id: Option<&str>,
    page: i64,
    page_size: i64,
) -> Result<Value> {
    let category = category.filter(|c| !c.is_empty() && *c != "all");
    let subject_id = subject_id.filter(|s| !s.is_empty());
    let page = page.clamp(1, 1_000_000);
    let page_size = page_size.clamp(1, 100);
    let total:i64=sqlx::query_scalar("SELECT COUNT(*) FROM subject_attempts WHERE (? IS NULL OR category=?) AND (? IS NULL OR subject_id=?)")
        .bind(category).bind(category).bind(subject_id).bind(subject_id).fetch_one(pool).await?;
    let rows=sqlx::query("SELECT * FROM subject_attempts WHERE (? IS NULL OR category=?) AND (? IS NULL OR subject_id=?) ORDER BY id DESC LIMIT ? OFFSET ?")
        .bind(category).bind(category).bind(subject_id).bind(subject_id).bind(page_size).bind((page-1)*page_size).fetch_all(pool).await?;
    let items:Vec<Value>=rows.into_iter().map(|r|json!({"id":r.get::<i64,_>("id"),"category":r.get::<String,_>("category"),"subject_id":r.get::<String,_>("subject_id"),"attempt":r.get::<i64,_>("attempt"),"started_ms":r.get::<i64,_>("started_ms"),"finished_ms":r.get::<Option<i64>,_>("finished_ms"),"outcome":r.get::<String,_>("outcome"),"error_code":r.get::<Option<String>,_>("error_code"),"http_status":r.get::<Option<i64>,_>("http_status"),"error":r.get::<Option<String>,_>("error"),"recovered":r.get::<i64,_>("recovered")!=0})).collect();
    Ok(json!({"items":items,"total":total,"page":page,"page_size":page_size}))
}
