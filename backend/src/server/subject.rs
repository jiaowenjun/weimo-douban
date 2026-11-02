//! 条目元信息富集——rexxar 结构化详情 → subjects 跨用户缓存。
//! 设计与验收场景见《条目元信息富集方案.md》。

use anyhow::{Context, Result};
use rand::Rng;
use serde_json::{json, Map, Value};
use sqlx::{Row, SqlitePool};

use crate::crawler::Category;

use super::config::ServerConfig;
use super::covers::cover_proxy_url;
use super::error::{AppError, AppResult};
use super::fetcher::{DoubanFetcher, FetchError};
use super::service::ms_to_rfc3339;
use super::state::AppState;

/// rexxar 校验请求来源：Referer 必须指向 m.douban.com，UA 需为移动端
pub const REXXAR_REFERER: &str = "https://m.douban.com/";
pub const MOBILE_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) \
     AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.0 Mobile/15E148 Safari/604.1";

/// 单条目富集最大尝试次数（含首次），对齐维度抓取的 MAX_ATTEMPTS
pub const MAX_ATTEMPTS: i64 = 5;

pub fn rexxar_url(category: &str, subject_id: &str) -> String {
    format!("https://m.douban.com/rexxar/api/v2/{category}/{subject_id}")
}

// --------------------------------------------------------------------------- //
// rexxar 响应 → subjects 行（映射契约见方案 §2.2）
// --------------------------------------------------------------------------- //

/// 映射产物：subjects 表除主键与状态列之外的完整载荷
pub struct SubjectRow {
    pub title: String,
    pub original_title: String,
    pub year: Option<i64>,
    pub aka_json: String,
    pub url: String,
    pub cover_url: String,
    pub rating_value: Option<f64>,
    pub rating_count: Option<i64>,
    pub meta_json: String,
    pub raw_json: String,
}

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 取数组中的字符串元素（豆瓣数组偶含非字符串项，逐个过滤）
fn arr(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// 对象数组的 name 字段（directors/actors/singer 均为 [{name: …}]）
fn arr_name(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// book/music 无独立 year 字段，取 pubdate[0] 前四位
fn year_from_pubdate(v: &Value) -> Option<i64> {
    arr(v, "pubdate")
        .first()
        .and_then(|d| d.get(..4)?.parse().ok())
}

fn opt_i64(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_i64)
}

pub fn map_rexxar(category: Category, v: &Value) -> SubjectRow {
    let mut meta = Map::new();
    match category {
        Category::Movie => {
            meta.insert("directors".into(), json!(arr_name(v, "directors")));
            meta.insert("actors".into(), json!(arr_name(v, "actors")));
            meta.insert("genres".into(), json!(arr(v, "genres")));
            meta.insert("countries".into(), json!(arr(v, "countries")));
            meta.insert("languages".into(), json!(arr(v, "languages")));
            meta.insert("durations".into(), json!(arr(v, "durations")));
            meta.insert("pubdate".into(), json!(arr(v, "pubdate")));
            meta.insert(
                "is_tv".into(),
                json!(v.get("is_tv").and_then(Value::as_bool)),
            );
            meta.insert("episodes_count".into(), json!(opt_i64(v, "episodes_count")));
            meta.insert("intro".into(), json!(s(v, "intro")));
        }
        Category::Book => {
            // rexxar 语义权威：author 与 translator 分列（intro 文本无法区分）
            meta.insert("authors".into(), json!(arr(v, "author")));
            meta.insert("translators".into(), json!(arr(v, "translator")));
            meta.insert("press".into(), json!(arr(v, "press")));
            meta.insert("pubdate".into(), json!(arr(v, "pubdate")));
            meta.insert("pages".into(), json!(arr(v, "pages")));
            meta.insert("price".into(), json!(arr(v, "price")));
            meta.insert("subtitle".into(), json!(s(v, "subtitle")));
            meta.insert(
                "series".into(),
                json!(v
                    .pointer("/book_series/title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()),
            );
            meta.insert("intro".into(), json!(s(v, "intro")));
        }
        Category::Music => {
            meta.insert("singers".into(), json!(arr_name(v, "singer")));
            meta.insert("genres".into(), json!(arr(v, "genres")));
            meta.insert("media".into(), json!(arr(v, "media")));
            meta.insert("discs".into(), json!(arr(v, "discs")));
            meta.insert("pubdate".into(), json!(arr(v, "pubdate")));
            meta.insert("publisher".into(), json!(arr(v, "publisher")));
            meta.insert("version".into(), json!(arr(v, "version")));
            meta.insert("songs".into(), json!(arr_name(v, "songs")));
            meta.insert("intro".into(), json!(s(v, "intro")));
        }
    }

    // movie 的 year 是字符串（"1994"）或数字，两种都收
    let year = match category {
        Category::Movie => v.get("year").and_then(|y| {
            y.as_i64()
                .or_else(|| y.as_str().and_then(|s| s.parse().ok()))
        }),
        _ => year_from_pubdate(v),
    };

    SubjectRow {
        title: s(v, "title"),
        original_title: s(v, "original_title"),
        year,
        aka_json: json!(arr(v, "aka")).to_string(),
        url: s(v, "url"),
        cover_url: s(v, "cover_url"),
        rating_value: v.pointer("/rating/value").and_then(Value::as_f64),
        rating_count: v.pointer("/rating/count").and_then(Value::as_i64),
        meta_json: json!(meta).to_string(),
        raw_json: v.to_string(),
    }
}

// --------------------------------------------------------------------------- //
// 入队：集合差 + INSERT OR IGNORE（跨用户去重的核心，方案 §5.1）
// --------------------------------------------------------------------------- //

/// 把该用户该类别下"从未富集"的条目排入富集队列。
/// 返回本次新排入的 job 数（已在库或已被其他用户排过的条目为 0 增量）。
///
/// 只入缺失条目：已在 subjects 中的数据不再自动重抓——rexxar 数据基本不变，
/// 重抓属显式操作（单条 refresh 或批量 requeue scope=stale，均 priority=0）。
pub async fn enqueue_missing_subjects(pool: &SqlitePool, uid: &str, category: &str) -> Result<u64> {
    let now = ServerConfig::now_ms();
    let res = sqlx::query(
        "INSERT OR IGNORE INTO subject_jobs(category, subject_id, created_ms, updated_ms)
         SELECT DISTINCT r.category, r.subject_id, ?, ?
         FROM records r
         LEFT JOIN subjects s
                ON s.category = r.category AND s.subject_id = r.subject_id
         WHERE r.uid = ? AND r.category = ?
           AND s.category IS NULL",
    )
    .bind(now)
    .bind(now)
    .bind(uid)
    .bind(category)
    .execute(pool)
    .await
    .context("富集队列入队失败")?;
    Ok(res.rows_affected())
}

/// 存量回填：对 records 里出现的每个 (uid, category) 逐个跑集合差（幂等）。
/// 返回 (新排入 job 数, 覆盖的 uid×类别对数)。
pub async fn backfill(pool: &SqlitePool) -> Result<(u64, usize)> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let pairs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM (SELECT DISTINCT uid,category FROM records)")
            .fetch_one(&mut *tx)
            .await?;
    let now = ServerConfig::now_ms();
    let queued=sqlx::query("INSERT OR IGNORE INTO subject_jobs(category,subject_id,created_ms,updated_ms)
        SELECT DISTINCT r.category,r.subject_id,?,? FROM records r LEFT JOIN subjects s USING(category,subject_id) WHERE s.subject_id IS NULL")
        .bind(now).bind(now).execute(&mut *tx).await?.rows_affected();
    let id = super::subject_queue::operation_id();
    super::subject_queue::audit(
        &mut tx,
        &id,
        "backfill",
        "*",
        &json!({}),
        &json!({"queued":queued}),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(operation_id=%id,queued,"enrichment backfill queued");
    Ok((queued, pairs as usize))
}

/// 强制刷新单个条目：无视 TTL 重新排队（priority=0 优先执行）
pub async fn force_refresh(pool: &SqlitePool, category: &str, subject_id: &str) -> Result<()> {
    let now = ServerConfig::now_ms();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query(
        "INSERT INTO subject_jobs (category, subject_id, state, priority, created_ms, updated_ms)
         VALUES (?, ?, 'queued', 0, ?, ?)
         ON CONFLICT(category, subject_id) DO UPDATE SET
            state = 'queued', attempts = 0, next_run_ms = NULL,
            last_error=NULL, retry_since_ms=NULL, run_token=NULL, priority = 0, updated_ms = excluded.updated_ms",
    )
    .bind(category)
    .bind(subject_id)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await
    .context("条目刷新入队失败")?;
    let id = super::subject_queue::operation_id();
    super::subject_queue::audit(
        &mut tx,
        &id,
        "refresh",
        category,
        &json!({"subject_id":subject_id}),
        &json!({"queued":1}),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(operation_id=%id,category,subject_id,"enrichment refresh queued");
    Ok(())
}

// --------------------------------------------------------------------------- //
// worker 执行（与 dimensions 同构的取任务-执行-退避，方案 §6）
// --------------------------------------------------------------------------- //

struct SubjectJob {
    category: String,
    subject_id: String,
    attempts: i64,
    priority: i64,
    run_token: String,
}

/// Claim under a write transaction: pause, cooldown and clear serialize with this decision.
async fn pick_due_job(state: &AppState) -> Result<Option<SubjectJob>> {
    let now = ServerConfig::now_ms();
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query(
        "SELECT j.category,j.subject_id,j.attempts,j.priority,j.run_token FROM subject_jobs j
         JOIN subject_runtime c ON c.category=j.category CROSS JOIN subject_runtime g
         WHERE g.category='*' AND g.paused=0 AND c.paused=0
           AND MAX(g.next_request_ms,c.next_request_ms,c.blocked_until_ms)<=?
           AND j.state IN ('queued','running') AND COALESCE(j.next_run_ms,0)<=?
           AND NOT EXISTS(SELECT 1 FROM subject_cache_checks x WHERE x.category=j.category AND x.subject_id=j.subject_id AND x.hold_until_ms>? AND j.priority!=0)
         ORDER BY CASE WHEN j.attempts>0 AND c.retry_streak<2 THEN 0 WHEN j.attempts=0 AND c.retry_streak>=2 THEN 0 ELSE 1 END,
           j.priority ASC, COALESCE(j.retry_since_ms,j.created_ms) ASC,j.rowid ASC LIMIT 1")
        .bind(now).bind(now).bind(now).fetch_optional(&mut *tx).await?;
    let Some(r) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    let category: String = r.get("category");
    let subject_id: String = r.get("subject_id");
    let attempts: i64 = r.get("attempts");
    let run_token = super::subject_queue::operation_id();
    // A prior process may have died after claiming. Preserve that unknown outcome explicitly.
    if let Some(old) = r.get::<Option<String>, _>("run_token") {
        sqlx::query("UPDATE subject_attempts SET outcome='interrupted',finished_ms=? WHERE run_token=? AND outcome='started'")
            .bind(now).bind(old).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE subject_jobs SET state='running',run_token=?,updated_ms=? WHERE category=? AND subject_id=?")
        .bind(&run_token).bind(now).bind(&category).bind(&subject_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE subject_runtime SET retry_streak=CASE WHEN ? THEN retry_streak+1 ELSE 0 END WHERE category=?")
        .bind(attempts>0).bind(&category).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO subject_attempts(run_token,category,subject_id,attempt,started_ms) VALUES(?,?,?,?,?)")
        .bind(&run_token).bind(&category).bind(&subject_id).bind(attempts+1).bind(now).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(SubjectJob {
        category,
        subject_id,
        attempts,
        priority: r.get("priority"),
        run_token,
    }))
}

/// Fetch and commit immediately. Request deadlines replace a sleep that blocked other categories.
pub async fn process_next_job<F: DoubanFetcher>(state: &AppState, fetcher: &F) -> Result<bool> {
    if !state.config.subject_enrich {
        return Ok(false);
    }
    let Some(job) = pick_due_job(state).await? else {
        return Ok(false);
    };
    if job.priority != 0 {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM subjects WHERE category=? AND subject_id=?)",
        )
        .bind(&job.category)
        .bind(&job.subject_id)
        .fetch_one(&state.pool)
        .await?;
        if exists {
            let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
            finish_job(&mut tx, &job, "ok", 0, None, None).await?;
            finish_attempt(&mut tx, &job, "cached", None, None).await?;
            tx.commit().await?;
            return Ok(true);
        }
    }
    super::subject_queue::restore_pacing(state, &job.category).await?;
    // Reserve before I/O so a crash cannot discard the next request deadline.
    // Cache hits above consume no request budget.
    let now = ServerConfig::now_ms();
    let mut reservation = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query(
        "UPDATE subject_runtime SET next_request_ms=MAX(next_request_ms,?) WHERE category=?",
    )
    .bind(now + (state.subject_interval(&job.category) * 1000.0) as i64)
    .bind(&job.category)
    .execute(&mut *reservation)
    .await?;
    sqlx::query(
        "UPDATE subject_runtime SET next_request_ms=MAX(next_request_ms,?) WHERE category='*'",
    )
    .bind(now + (state.config.crawl_min_interval_secs.max(0.0) * 1000.0) as i64)
    .execute(&mut *reservation)
    .await?;
    reservation.commit().await?;

    match fetcher.fetch_subject(&job.category, &job.subject_id).await {
        Ok(value) => {
            let category = Category::from_key(&job.category)
                .with_context(|| format!("未知 category: {}", job.category))?;
            let row = map_rexxar(category, &value);
            let interval = state.subject_interval_after_success(&job.category);
            let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
            upsert_subject_row(&mut tx, &job.category, &job.subject_id, &row, "rexxar").await?;
            finish_job(&mut tx, &job, "ok", 0, None, None).await?;
            finish_attempt(&mut tx, &job, "success", None, None).await?;
            persist_pacing(state, &mut tx, &job, interval, true).await?;
            tx.commit().await?;
            state.record_crawl_success();
            tracing::info!(subject=format!("{}/{}",job.category,job.subject_id),title=%row.title,recovered=job.attempts>0,"条目元信息富集完成");
        }
        Err(e) => handle_job_error(state, &job, e).await?,
    }
    Ok(true)
}

async fn finish_job(
    conn: &mut sqlx::SqliteConnection,
    job: &SubjectJob,
    status: &str,
    attempts: i64,
    next: Option<i64>,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query("UPDATE subject_jobs SET state=?,attempts=?,next_run_ms=?,last_error=?,updated_ms=?,run_token=NULL,
      retry_since_ms=CASE WHEN ?='queued' THEN COALESCE(retry_since_ms,?) ELSE NULL END
      WHERE category=? AND subject_id=? AND run_token=?")
        .bind(status).bind(attempts).bind(next).bind(error).bind(ServerConfig::now_ms()).bind(status).bind(ServerConfig::now_ms())
        .bind(&job.category).bind(&job.subject_id).bind(&job.run_token).execute(conn).await?;
    Ok(())
}

async fn finish_attempt(
    conn: &mut sqlx::SqliteConnection,
    job: &SubjectJob,
    outcome: &str,
    error: Option<&FetchError>,
    http: Option<i64>,
) -> Result<()> {
    sqlx::query("UPDATE subject_attempts SET finished_ms=?,outcome=?,error_code=?,http_status=?,error=?,recovered=? WHERE run_token=?")
        .bind(ServerConfig::now_ms()).bind(outcome).bind(error.map(|e|e.code())).bind(http).bind(error.map(|e|e.to_string()))
        .bind(outcome=="success" && job.attempts>0).bind(&job.run_token).execute(conn).await?;
    Ok(())
}

async fn persist_pacing(
    state: &AppState,
    conn: &mut sqlx::SqliteConnection,
    job: &SubjectJob,
    interval: f64,
    success: bool,
) -> Result<()> {
    let now = ServerConfig::now_ms();
    let jitter = rand::thread_rng().gen_range(0.0..1.0);
    sqlx::query("UPDATE subject_runtime SET interval_secs=?,pacing_updated_ms=?,next_request_ms=?,block_streak=CASE WHEN ? THEN 0 ELSE block_streak END WHERE category=?")
        .bind(interval).bind(now).bind(now+((interval+jitter)*1000.0) as i64).bind(success).bind(&job.category)
        .execute(&mut *conn).await?;
    sqlx::query(
        "UPDATE subject_runtime SET next_request_ms=MAX(next_request_ms,?) WHERE category='*'",
    )
    .bind(now + ((state.config.crawl_min_interval_secs.max(0.0) + jitter) * 1000.0) as i64)
    .execute(conn)
    .await?;
    Ok(())
}

async fn handle_job_error(state: &AppState, job: &SubjectJob, e: FetchError) -> Result<()> {
    let now = ServerConfig::now_ms();
    let attempts = job.attempts + 1;
    let blocked = matches!(e, FetchError::Blocked(_));
    let permanent = !e.is_transient();
    let http = match &e {
        FetchError::Blocked(code) => Some(*code as i64),
        FetchError::NotFound => Some(404),
        _ => None,
    };
    let interval = if blocked {
        state.subject_interval_after_block(&job.category)
    } else {
        state.subject_interval(&job.category)
    };
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    persist_pacing(state, &mut tx, job, interval, false).await?;
    if !blocked {
        sqlx::query("UPDATE subject_runtime SET block_streak=0 WHERE category=?")
            .bind(&job.category)
            .execute(&mut *tx)
            .await?;
    }
    let mut until = now;
    if blocked || permanent {
        let streak: i64 =
            sqlx::query_scalar("SELECT block_streak FROM subject_runtime WHERE category=?")
                .bind(&job.category)
                .fetch_one(&mut *tx)
                .await?;
        let factor = if blocked && state.config.subject_adaptive_cooldown {
            1i64 << streak.clamp(0, 4)
        } else {
            1
        };
        // Only uninterrupted blocked probes increase the experimental cooldown; success resets it.
        let base = state.config.subject_category_cooldown_secs.max(0);
        let seconds = (base.saturating_mul(factor)).min(base.max(1800));
        until = now + seconds * 1000 + rand::thread_rng().gen_range(0..30_000);
        sqlx::query("UPDATE subject_runtime SET blocked_until_ms=MAX(blocked_until_ms,?),block_streak=CASE WHEN ? THEN block_streak+1 ELSE 0 END WHERE category=?")
            .bind(until).bind(blocked).bind(&job.category).execute(&mut *tx).await?;
        // Preserve next_run visibility for existing jobs; the category gate also covers future jobs.
        sqlx::query("UPDATE subject_jobs SET next_run_ms=MAX(COALESCE(next_run_ms,0),?) WHERE category=? AND state IN ('queued','running')")
            .bind(until).bind(&job.category).execute(&mut *tx).await?;
    }
    let mut error = format!("{}: {e}", e.code());
    let (status, outcome, next) = if permanent {
        ("not_found", "not_found", None)
    } else if attempts >= MAX_ATTEMPTS {
        let row=sqlx::query("SELECT title,alt_title,subject_url,cover,intro FROM records WHERE category=? AND subject_id=? LIMIT 1")
            .bind(&job.category).bind(&job.subject_id).fetch_optional(&mut *tx).await?;
        if let Some(r) = row {
            let record = crate::crawler::Record {
                category: job.category.clone(),
                subject_id: job.subject_id.clone(),
                title: r.get("title"),
                alt_title: r.get("alt_title"),
                subject_url: r.get("subject_url"),
                cover: r.get("cover"),
                intro: r.get("intro"),
                ..Default::default()
            };
            // An in-flight failure must not downgrade an already available authoritative result.
            let authoritative:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM subjects WHERE category=? AND subject_id=? AND source='rexxar')")
                .bind(&job.category).bind(&job.subject_id).fetch_one(&mut *tx).await?;
            if !authoritative {
                upsert_subject_row(
                    &mut tx,
                    &job.category,
                    &job.subject_id,
                    &intro_fallback_row(&record),
                    "intro",
                )
                .await?;
            }
            error = if authoritative {
                format!("重试耗尽，保留已有 rexxar 缓存: {error}")
            } else {
                format!("已降级 intro 兜底（重试耗尽）: {error}")
            };
            (
                "ok",
                if authoritative {
                    "retained"
                } else {
                    "fallback"
                },
                None,
            )
        } else {
            ("error", "error", None)
        }
    } else {
        let backoff = 20_000 * 3_i64.pow((attempts as u32).saturating_sub(1))
            + rand::thread_rng().gen_range(0..5_000);
        ("queued", "retry", Some((now + backoff).max(until)))
    };
    finish_job(&mut tx, job, status, attempts, next, Some(&error)).await?;
    finish_attempt(&mut tx, job, outcome, Some(&e), http).await?;
    tx.commit().await?;
    tracing::warn!(subject=format!("{}/{}",job.category,job.subject_id),attempt=attempts,error=%e,outcome,"条目富集失败");
    if !permanent && state.record_crawl_failure() {
        tracing::info!("熔断已触发，worker 将暂停抓取直至冷却结束");
    }
    Ok(())
}

async fn upsert_subject_row(
    conn: &mut sqlx::SqliteConnection,
    category: &str,
    subject_id: &str,
    row: &SubjectRow,
    source: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO subjects (category, subject_id, title, original_title, year, aka_json,
                               url, cover_url, rating_value, rating_count, meta_json, raw_json,
                               status, source, attempts, next_retry_ms, fetched_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'ok', ?, 0, NULL, ?)
         ON CONFLICT(category, subject_id) DO UPDATE SET
            title = excluded.title, original_title = excluded.original_title,
            year = excluded.year, aka_json = excluded.aka_json,
            url = excluded.url, cover_url = excluded.cover_url,
            rating_value = excluded.rating_value, rating_count = excluded.rating_count,
            meta_json = excluded.meta_json, raw_json = excluded.raw_json,
            status = 'ok', source = excluded.source, attempts = 0, next_retry_ms = NULL,
            fetched_ms = excluded.fetched_ms",
    )
    .bind(category)
    .bind(subject_id)
    .bind(&row.title)
    .bind(&row.original_title)
    .bind(row.year)
    .bind(&row.aka_json)
    .bind(&row.url)
    .bind(&row.cover_url)
    .bind(row.rating_value)
    .bind(row.rating_count)
    .bind(&row.meta_json)
    .bind(&row.raw_json)
    .bind(source)
    .bind(ServerConfig::now_ms())
    .execute(conn)
    .await
    .context("条目元信息入库失败")?;
    Ok(())
}

// --------------------------------------------------------------------------- //
// 任务管理：队列视图与批量操作（admin「富集任务」页）
// --------------------------------------------------------------------------- //

/// GET /subjects/jobs：分页列出富集 job。标题/封面优先取 subjects，
/// 未富集条目回退 records（跨用户任一条标记行）；附各状态计数。
/// states 为空表示全部；活动状态（running/queued）排前。
pub async fn list_jobs(
    state: &AppState,
    states: &[String],
    category: Option<&str>,
    q: &str,
    page: i64,
    page_size: i64,
) -> AppResult<Value> {
    let page = page.max(1);
    let page_size = page_size.clamp(1, 200);
    let offset = (page - 1) * page_size;
    let has_category = category.is_some_and(|c| !c.is_empty() && c != "all");

    let mut where_clause = String::new();
    if !states.is_empty() {
        let placeholders = vec!["?"; states.len()].join(",");
        where_clause.push_str(&format!(" WHERE j.state IN ({placeholders})"));
    }
    if has_category {
        where_clause.push_str(if where_clause.is_empty() {
            " WHERE"
        } else {
            " AND"
        });
        where_clause.push_str(" j.category = ?");
    }
    // 搜索命中任一即可：条目 ID / 已富集标题 / 任一用户标记行的标题
    let like = q.replace('\\', "\\\\").replace('%', "\\%");
    let has_q = !q.is_empty();
    if has_q {
        where_clause.push_str(if where_clause.is_empty() {
            " WHERE"
        } else {
            " AND"
        });
        where_clause.push_str(
            " (j.subject_id LIKE ? ESCAPE '\\' OR s.title LIKE ? ESCAPE '\\' \
              OR EXISTS (SELECT 1 FROM records r WHERE r.category = j.category \
                         AND r.subject_id = j.subject_id AND r.title LIKE ? ESCAPE '\\'))",
        );
    }

    let total: i64 = {
        let sql = format!(
            "SELECT COUNT(*) FROM subject_jobs j LEFT JOIN subjects s ON s.category = j.category AND s.subject_id = j.subject_id{where_clause}"
        );
        let mut query = sqlx::query_scalar(&sql);
        for st in states {
            query = query.bind(st);
        }
        if has_category {
            query = query.bind(category.unwrap());
        }
        if has_q {
            for _ in 0..3 {
                query = query.bind(format!("%{like}%"));
            }
        }
        query.fetch_one(&state.pool).await?
    };

    let rows = {
        let sql = format!(
            "SELECT j.category, j.subject_id, j.state, j.attempts, j.priority,
                    j.next_run_ms, j.created_ms, j.updated_ms, j.last_error,
                    s.title AS enriched_title, s.cover_url, s.fetched_ms,
                    (SELECT r.title FROM records r
                      WHERE r.category = j.category AND r.subject_id = j.subject_id LIMIT 1
                    ) AS record_title,
                    (SELECT r.cover FROM records r
                      WHERE r.category = j.category AND r.subject_id = j.subject_id LIMIT 1
                    ) AS record_cover
             FROM subject_jobs j
             LEFT JOIN subjects s ON s.category = j.category AND s.subject_id = j.subject_id
             {where_clause}
             ORDER BY CASE j.state WHEN 'running' THEN 0 WHEN 'queued' THEN 1 ELSE 2 END,
                      j.updated_ms DESC, j.rowid DESC
             LIMIT ? OFFSET ?"
        );
        let mut query = sqlx::query(&sql);
        for st in states {
            query = query.bind(st);
        }
        if has_category {
            query = query.bind(category.unwrap());
        }
        if has_q {
            for _ in 0..3 {
                query = query.bind(format!("%{like}%"));
            }
        }
        let query = query.bind(page_size).bind(offset);
        query.fetch_all(&state.pool).await?
    };
    let stale_before = ServerConfig::now_ms() - state.config.subject_ttl_secs * 1000;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let fetched_ms: Option<i64> = r.try_get("fetched_ms").ok().flatten();
            let enriched_title: Option<String> = r.try_get("enriched_title").ok().flatten();
            let record_title: Option<String> = r.try_get("record_title").ok().flatten();
            let cover_url: Option<String> = r.try_get("cover_url").ok().flatten();
            let record_cover: Option<String> = r.try_get("record_cover").ok().flatten();
            let title = enriched_title
                .filter(|t| !t.is_empty())
                .or_else(|| record_title.filter(|t| !t.is_empty()));
            let cover = cover_url
                .filter(|c| !c.is_empty())
                .or_else(|| record_cover.filter(|c| !c.is_empty()))
                .map(|c| cover_proxy_url(&c))
                .unwrap_or_default();
            json!({
                "category": r.get::<String, _>("category"),
                "subject_id": r.get::<String, _>("subject_id"),
                "title": title,
                "cover": cover,
                "state": r.get::<String, _>("state"),
                "attempts": r.get::<i64, _>("attempts"),
                // priority=0 为显式强制重抓（单条 refresh / 批量 requeue），插队执行
                "forced": r.get::<i64, _>("priority") == 0,
                "next_run_at": r.try_get::<Option<i64>, _>("next_run_ms")
                    .ok().flatten().map(ms_to_rfc3339),
                "created_at": ms_to_rfc3339(r.get::<i64, _>("created_ms")),
                "updated_at": ms_to_rfc3339(r.get::<i64, _>("updated_ms")),
                "last_error": r.try_get::<Option<String>, _>("last_error").ok().flatten(),
                "fetched_at": fetched_ms.map(ms_to_rfc3339),
                // 库中数据已过 TTL：仅作展示与「重抓过期」范围的提示
                "stale": fetched_ms.is_some_and(|ms| ms < stale_before),
            })
        })
        .collect();

    let mut counts = std::collections::BTreeMap::new();
    for st in ["queued", "running", "ok", "error", "not_found"] {
        counts.insert(st.to_string(), 0i64);
    }
    for row in sqlx::query("SELECT state, COUNT(*) AS n FROM subject_jobs GROUP BY state")
        .fetch_all(&state.pool)
        .await?
    {
        let st: String = row.get("state");
        let n: i64 = row.get("n");
        counts.insert(st, n);
    }
    let total_jobs: i64 = counts.values().sum();
    Ok(json!({
        "items": items,
        "total": total,
        "page": page,
        "page_size": page_size,
        "counts": serde_json::to_value(&counts).unwrap_or(json!({})),
        "counts_total": total_jobs,
    }))
}

/// DELETE /subjects/jobs/{category}/{subject_id}：删除单个 job（任意状态）。
/// 删除 queued 即取消该条目；删除终态失败 job 后，下次同步可重新入队。
pub async fn delete_job(pool: &SqlitePool, category: &str, subject_id: &str) -> AppResult<()> {
    let states = ["queued", "running", "ok", "error", "not_found"].map(str::to_owned);
    let result =
        super::subject_queue::clear(pool, &states, Some(category), Some(subject_id)).await?;
    if result["deleted"].as_u64() == Some(0) {
        return Err(AppError::JobNotFound(format!("{category}/{subject_id}")));
    }
    Ok(())
}

/// POST /subjects/jobs/clear：按状态批量删除 job。
/// states 由路由层校验非空且均为合法状态；「中断队列」= ["queued","running"]。
/// 正在抓取中的 job（至多一个，单 worker）删除后本次结果仍会写库，仅 job 行消失。
pub async fn clear_jobs(
    pool: &SqlitePool,
    states: &[String],
    category: Option<&str>,
) -> Result<u64> {
    let result = super::subject_queue::clear(pool, states, category, None).await?;
    Ok(result["deleted"].as_u64().unwrap_or(0))
}

/// POST /subjects/jobs/requeue：批量重排。
/// - failed / not_found：对应终态 job 重新排队（priority=0 插队；not_found 多为
///   条目被豆瓣删除，重排仅在怀疑误判时使用）；
/// - stale：subjects 中已过 TTL 的条目强制重抓（priority=0 绕过缓存命中，仍遵守暂停/冷却）——
///   这是"重新抓取库中已有条目"唯一的批量入口。
pub async fn requeue_jobs(state: &AppState, scope: &str, category: Option<&str>) -> Result<u64> {
    let now = ServerConfig::now_ms();
    let category = category.filter(|c| !c.is_empty() && *c != "all");
    let has_category = category.is_some();
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let queued = match scope {
        "failed" | "not_found" => {
            let from = if scope == "failed" {
                "error"
            } else {
                "not_found"
            };
            let mut sql = format!(
                "UPDATE subject_jobs SET state='queued', attempts=0, next_run_ms=NULL,
                        last_error=NULL, retry_since_ms=NULL, run_token=NULL, priority=0, updated_ms=?
                 WHERE state = '{from}'"
            );
            if has_category {
                sql.push_str(" AND category = ?");
            }
            let mut query = sqlx::query(&sql).bind(now);
            if let Some(c) = category {
                query = query.bind(c);
            }
            query.execute(&mut *tx).await?.rows_affected()
        }
        "stale" => {
            let stale_before = now - state.config.subject_ttl_secs * 1000;
            let mut sql = String::from(
                "INSERT INTO subject_jobs (category, subject_id, state, priority, created_ms, updated_ms)
                 SELECT category, subject_id, 'queued', 0, ?, ? FROM subjects
                 WHERE status = 'ok' AND fetched_ms < ?",
            );
            if has_category {
                sql.push_str(" AND category = ?");
            }
            // 已在排队/执行中的 job 不动（避免重置退避计时）；终态行改为重排
            sql.push_str(
                " ON CONFLICT(category, subject_id) DO UPDATE SET \
                   state='queued', attempts=0, next_run_ms=NULL, last_error=NULL, \
                   priority=0, retry_since_ms=NULL, run_token=NULL, updated_ms=excluded.updated_ms \
                   WHERE subject_jobs.state NOT IN ('queued','running')",
            );
            let mut query = sqlx::query(&sql).bind(now).bind(now).bind(stale_before);
            if let Some(c) = category {
                query = query.bind(c);
            }
            query.execute(&mut *tx).await?.rows_affected()
        }
        _ => unreachable!("scope 已在路由层校验"),
    };
    let id = super::subject_queue::operation_id();
    super::subject_queue::audit(
        &mut tx,
        &id,
        "requeue",
        category.unwrap_or("all"),
        &json!({"scope":scope}),
        &json!({"queued":queued}),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(operation_id=%id,scope,category=?category,queued,"enrichment jobs requeued");
    Ok(queued)
}

// --------------------------------------------------------------------------- //
// 读侧：API 视图
// --------------------------------------------------------------------------- //

/// subjects 行 → 对外 JSON（结构见方案 §7.1）
pub(super) fn subject_view(category: &str, subject_id: &str, r: &sqlx::sqlite::SqliteRow) -> Value {
    let meta_json: String = r.get("meta_json");
    let aka_json: String = r.get("aka_json");
    let fetched_ms: i64 = r.get("fetched_ms");
    let rating = json!({
        "value": r.try_get::<Option<f64>, _>("rating_value").ok().flatten(),
        "count": r.try_get::<Option<i64>, _>("rating_count").ok().flatten(),
    });
    let mut view = json!({
        "category": category,
        "subject_id": subject_id,
        "title": r.get::<String, _>("title"),
        "original_title": r.get::<String, _>("original_title"),
        "aka": serde_json::from_str::<Value>(&aka_json).unwrap_or(json!([])),
        "year": r.try_get::<Option<i64>, _>("year").ok().flatten(),
        "rating": rating,
        // 与 records.cover 同理：豆瓣图域有 Referer 防盗链，出口改写走本地代理
        "cover": cover_proxy_url(&r.get::<String, _>("cover_url")),
        "url": r.get::<String, _>("url"),
        "status": r.get::<String, _>("status"),
        "fetched_at": ms_to_rfc3339(fetched_ms),
        // 获取方式随行走库：rexxar = 权威富集；intro = 重试耗尽的兜底行
        "source": r.get::<String, _>("source"),
    });
    // 类别专属节点：键随 category（movie/book/music）
    view[category] = serde_json::from_str::<Value>(&meta_json).unwrap_or(json!({}));
    view
}

pub async fn get_subject_view(
    state: &AppState,
    category: &str,
    subject_id: &str,
) -> AppResult<Value> {
    get_subject_view_from_pool(&state.pool, category, subject_id).await
}

pub(super) async fn get_subject_view_from_pool(
    pool: &SqlitePool,
    category: &str,
    subject_id: &str,
) -> AppResult<Value> {
    if let Some(row) = sqlx::query(
        "SELECT title, original_title, year, aka_json, cover_url, rating_value,
                rating_count, meta_json, status, fetched_ms, url, source
         FROM subjects WHERE category = ? AND subject_id = ?",
    )
    .bind(category)
    .bind(subject_id)
    .fetch_optional(pool)
    .await?
    {
        return Ok(subject_view(category, subject_id, &row));
    }
    // subjects 未富集：回退 records.intro 合成视图（跨用户任意一条标记即可）
    let record = sqlx::query(
        "SELECT subject_id, title, alt_title, subject_url, intro FROM records
         WHERE category = ? AND subject_id = ? LIMIT 1",
    )
    .bind(category)
    .bind(subject_id)
    .fetch_optional(pool)
    .await?;
    if let Some(r) = record {
        let rec = crate::crawler::Record {
            category: category.to_string(),
            status: String::new(),
            title: r.get("title"),
            alt_title: r.get("alt_title"),
            my_rating: None,
            date: String::new(),
            comment: String::new(),
            subject_url: r.get("subject_url"),
            subject_id: subject_id.to_string(),
            cover: String::new(),
            comment_id: String::new(),
            intro: r.get("intro"),
        };
        return Ok(intro_fallback_view(&rec));
    }
    Err(AppError::SubjectNotFound(format!(
        "{category}/{subject_id}"
    )))
}

/// intro 合成的年份与类别节点（读侧兜底视图与写侧兜底行共用同一解析）：
/// 返回 (year, meta 节点)，键随 category 为 movie/book/music
fn intro_fallback_parts(record: &crate::crawler::Record) -> (Option<i64>, Value) {
    let opt_arr = |v: &String| {
        if v.is_empty() {
            vec![]
        } else {
            vec![v.clone()]
        }
    };
    match record.category.as_str() {
        "movie" => {
            let m = crate::intro::parse_movie(&record.intro, &record.title, &record.alt_title);
            let year = m.years.iter().min().map(|&y| y as i64);
            let meta = json!({
                "directors": m.director_list,
                "actors": [],
                "genres": m.genres,
                "countries": m.countries,
                "languages": m.langs,
                "durations": m.duration.map(|d| vec![format!("{d}分钟")]).unwrap_or_default(),
                "pubdate": m.dates,
                "is_tv": Value::Null,
                "episodes_count": Value::Null,
                "intro": "",
            });
            (year, meta)
        }
        "book" => {
            let b = crate::intro::parse_book(&record.intro);
            let meta = json!({
                "authors": b.authors,
                "translators": [],
                "press": opt_arr(&b.publisher),
                "pubdate": opt_arr(&b.pubdate),
                "pages": [],
                "price": opt_arr(&b.price_raw),
                "subtitle": "",
                "series": "",
                "intro": "",
            });
            (b.pubyear.map(|y| y as i64), meta)
        }
        _ => {
            let m = crate::intro::parse_music(&record.intro);
            let split_plus = |v: &str| {
                v.split('+')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            };
            let meta = json!({
                "singers": opt_arr(&m.artist),
                "genres": split_plus(&m.genre),
                "media": split_plus(&m.medium),
                "discs": [],
                "pubdate": opt_arr(&m.date),
                "publisher": [],
                "version": [],
                "songs": [],
                "intro": "",
            });
            (m.year.map(|y| y as i64), meta)
        }
    }
}

/// intro 合成的 subjects 行（重试耗尽的兜底，source='intro'）：
/// 标题/链接/封面取 records（封面为收藏页小图 URL，出口走 covers 代理），
/// 无豆瓣评分、无原名与又名；raw_json 留空（intro 原文本就在 records 里）
fn intro_fallback_row(record: &crate::crawler::Record) -> SubjectRow {
    let (year, meta) = intro_fallback_parts(record);
    SubjectRow {
        title: record.title.clone(),
        original_title: String::new(),
        year,
        aka_json: "[]".to_string(),
        url: record.subject_url.clone(),
        cover_url: record.cover.clone(),
        rating_value: None,
        rating_count: None,
        meta_json: meta.to_string(),
        raw_json: String::new(),
    }
}

/// intro 兜底视图：subjects 未富集时按 records.intro（Rust 移植解析器）合成，
/// 形状与 rexxar 视图一致但无豆瓣评分、source="intro"（方案 §9 读侧兜底）。
pub(super) fn intro_fallback_view(record: &crate::crawler::Record) -> Value {
    let category = record.category.as_str();
    let (year, meta) = intro_fallback_parts(record);
    let mut view = json!({
        "category": category,
        "subject_id": record.subject_id,
        "title": record.title,
        "original_title": "",
        "aka": [],
        "year": year.map(|y| json!(y)).unwrap_or(Value::Null),
        "rating": {"value": Value::Null, "count": Value::Null},
        // intro 行无独立封面 URL（records.cover 由 marks 响应自身携带）
        "cover": "",
        "url": record.subject_url,
        "status": "ok",
        "fetched_at": Value::Null,
        "source": "intro",
    });
    view[category] = meta;
    view
}

/// marks?expand=subject 的批量读：按传入顺序对齐返回；subjects 已富集用 rexxar
/// 视图，否则回退 intro 解析合成（source="intro"，启发式字段、无豆瓣评分）。
pub async fn load_subject_views(
    state: &AppState,
    category: &str,
    records: &[crate::crawler::Record],
) -> Result<Vec<Value>> {
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; records.len()].join(",");
    let sql = format!(
        "SELECT subject_id, title, original_title, year, aka_json, cover_url,
                rating_value, rating_count, meta_json, status, fetched_ms, url, source
         FROM subjects WHERE category = ? AND subject_id IN ({placeholders})"
    );
    let mut q = sqlx::query(&sql).bind(category);
    for r in records {
        q = q.bind(&r.subject_id);
    }
    let rows = q
        .fetch_all(&state.pool)
        .await
        .context("批量读取条目元信息失败")?;
    let by_id: std::collections::HashMap<String, Value> = rows
        .iter()
        .map(|r| {
            let id: String = r.get("subject_id");
            (id.clone(), subject_view(category, &id, r))
        })
        .collect();
    Ok(records
        .iter()
        .map(|r| {
            by_id
                .get(&r.subject_id)
                .cloned()
                .unwrap_or_else(|| intro_fallback_view(r))
        })
        .collect())
}

/// 全局富集进度计数（sync result 汇总与 /subjects 列表共用）
pub struct EnrichmentCounts {
    pub enriched: i64,
    pub pending: i64,
    pub not_found: i64,
    pub failed: i64,
    /// intro 兜底行数（重试耗尽降级，可经 refresh / requeue stale 升级）
    pub intro_fallback: i64,
}

pub async fn enrichment_counts(pool: &SqlitePool) -> Result<EnrichmentCounts> {
    let enriched: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subjects WHERE status = 'ok'")
        .fetch_one(pool)
        .await
        .context("统计已富集条目失败")?;
    let count = |sql: &'static str| sqlx::query_scalar(sql).fetch_one(pool);
    let pending: i64 =
        count("SELECT COUNT(*) FROM subject_jobs WHERE state IN ('queued','running')")
            .await
            .context("统计待富集条目失败")?;
    let not_found: i64 = count("SELECT COUNT(*) FROM subject_jobs WHERE state = 'not_found'")
        .await
        .unwrap_or(0);
    let failed: i64 = count("SELECT COUNT(*) FROM subject_jobs WHERE state = 'error'")
        .await
        .unwrap_or(0);
    let intro_fallback: i64 = count("SELECT COUNT(*) FROM subjects WHERE source = 'intro'")
        .await
        .unwrap_or(0);
    Ok(EnrichmentCounts {
        enriched,
        pending,
        not_found,
        failed,
        intro_fallback,
    })
}

/// GET /subjects：分页列出已富集条目（admin 总览页数据源）。
/// category 为空/"all" 表示全部类别；q 按标题模糊过滤。
pub async fn list_subjects(
    state: &AppState,
    category: Option<&str>,
    q: &str,
    page: i64,
    page_size: i64,
) -> AppResult<Value> {
    let page = page.max(1);
    let page_size = page_size.clamp(1, 200);
    let offset = (page - 1) * page_size;

    let mut where_clause = String::new();
    let mut bind_category: Option<String> = None;
    let has_category = category.is_some_and(|c| !c.is_empty() && c != "all");
    if has_category {
        where_clause.push_str(" WHERE category = ?");
        bind_category = category.map(str::to_string);
    }
    if !q.is_empty() {
        where_clause.push_str(if where_clause.is_empty() {
            " WHERE"
        } else {
            " AND"
        });
        where_clause.push_str(" title LIKE ? ESCAPE '\\'");
    }

    let total: i64 = {
        let sql = format!("SELECT COUNT(*) FROM subjects{where_clause}");
        let mut query = sqlx::query_scalar(&sql);
        if let Some(c) = &bind_category {
            query = query.bind(c);
        }
        if !q.is_empty() {
            query = query.bind(format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%")));
        }
        query.fetch_one(&state.pool).await?
    };

    let rows = {
        let sql = format!(
            "SELECT subject_id, title, original_title, year, aka_json, cover_url,
                    rating_value, rating_count, meta_json, status, fetched_ms, url, category, source
             FROM subjects{where_clause}
             ORDER BY fetched_ms DESC, rowid DESC
             LIMIT ? OFFSET ?"
        );
        let mut query = sqlx::query(&sql);
        if let Some(c) = &bind_category {
            query = query.bind(c);
        }
        if !q.is_empty() {
            query = query.bind(format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%")));
        }
        let query = query.bind(page_size).bind(offset);
        query.fetch_all(&state.pool).await?
    };
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let cat: String = r.get("category");
            let id: String = r.get("subject_id");
            subject_view(&cat, &id, r)
        })
        .collect();

    let counts = enrichment_counts(&state.pool).await?;
    Ok(json!({
        "items": items,
        "total": total,
        "page": page,
        "page_size": page_size,
        "counts": {
            "enriched": counts.enriched,
            "pending": counts.pending,
            "not_found": counts.not_found,
            "failed": counts.failed,
            "intro_fallback": counts.intro_fallback,
        },
    }))
}

// --------------------------------------------------------------------------- //
// 映射黄金用例：锁定三类 rexxar 真实响应的字段映射（fixture 为 2026-10-28 实抓）
// --------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(name: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(format!("tests/fixtures/{name}")).unwrap())
            .unwrap()
    }

    #[test]
    fn map_rexxar_movie_fixture() {
        let row = map_rexxar(Category::Movie, &fixture("rexxar_movie.json"));
        assert_eq!(row.title, "肖申克的救赎");
        assert_eq!(row.original_title, "The Shawshank Redemption");
        assert_eq!(row.year, Some(1994));
        assert_eq!(row.rating_value, Some(9.7));
        assert!(row.rating_count.unwrap_or(0) > 3_000_000);
        let meta: Value = serde_json::from_str(&row.meta_json).unwrap();
        assert_eq!(meta["directors"], json!(["弗兰克·德拉邦特"]));
        assert_eq!(meta["actors"][0], json!("蒂姆·罗宾斯"));
        assert_eq!(meta["genres"], json!(["剧情", "犯罪"]));
        assert_eq!(meta["countries"], json!(["美国"]));
        assert_eq!(meta["languages"], json!(["英语"]));
        assert_eq!(meta["durations"], json!(["142分钟"]));
        assert_eq!(meta["is_tv"], json!(false));
        let aka: Value = serde_json::from_str(&row.aka_json).unwrap();
        assert!(aka.as_array().unwrap().iter().any(|v| v == "月黑高飞(港)"));
        assert!(row.raw_json.contains("Shawshank"));
    }

    #[test]
    fn map_rexxar_book_fixture() {
        let row = map_rexxar(Category::Book, &fixture("rexxar_book.json"));
        assert_eq!(row.title, "浮生六记");
        // book 无独立 year 字段，取 pubdate[0] 前四位
        assert_eq!(row.year, Some(2026));
        assert_eq!(row.rating_value, Some(9.1));
        let meta: Value = serde_json::from_str(&row.meta_json).unwrap();
        // author 与 translator 分列——intro 纯文本无法做到的语义区分
        assert_eq!(meta["authors"], json!(["沈复"]));
        assert_eq!(meta["translators"], json!(["羽戈"]));
        assert_eq!(meta["press"], json!(["广西师范大学出版社"]));
        assert_eq!(meta["pubdate"], json!(["2026-2"]));
        assert_eq!(meta["price"], json!(["56.00元"]));
        assert_eq!(meta["pages"], json!(["328"]));
        assert_eq!(meta["series"], json!("人间三梦：古典文库系列"));
    }

    #[test]
    fn map_rexxar_music_fixture() {
        let row = map_rexxar(Category::Music, &fixture("rexxar_music.json"));
        assert_eq!(row.title, "歌之女");
        assert_eq!(row.year, Some(1995));
        let meta: Value = serde_json::from_str(&row.meta_json).unwrap();
        assert_eq!(meta["singers"], json!(["梅艷芳", "Anita Mui"]));
        assert_eq!(meta["genres"], json!(["流行"]));
        assert_eq!(meta["media"], json!(["CD"]));
        assert_eq!(meta["discs"], json!(["1"]));
    }
}
