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
    let pairs = sqlx::query("SELECT DISTINCT uid, category FROM records ORDER BY uid, category")
        .fetch_all(pool)
        .await
        .context("回填读取 records 失败")?;
    let mut queued = 0u64;
    for row in &pairs {
        let uid: String = row.get("uid");
        let category: String = row.get("category");
        queued += enqueue_missing_subjects(pool, &uid, &category).await?;
    }
    Ok((queued, pairs.len()))
}

/// 强制刷新单个条目：无视 TTL 重新排队（priority=0 优先执行）
pub async fn force_refresh(pool: &SqlitePool, category: &str, subject_id: &str) -> Result<()> {
    let now = ServerConfig::now_ms();
    sqlx::query(
        "INSERT INTO subject_jobs (category, subject_id, state, priority, created_ms, updated_ms)
         VALUES (?, ?, 'queued', 0, ?, ?)
         ON CONFLICT(category, subject_id) DO UPDATE SET
            state = 'queued', attempts = 0, next_run_ms = NULL,
            priority = 0, updated_ms = excluded.updated_ms",
    )
    .bind(category)
    .bind(subject_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .context("条目刷新入队失败")?;
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
}

/// 处理一个到期富集 job；返回是否处理了（供 worker 循环与测试复用）。
/// 维度队列优先，本方法只在 worker 空闲时段被调用。
pub async fn process_next_job<F: DoubanFetcher>(state: &AppState, fetcher: &F) -> Result<bool> {
    if !state.config.subject_enrich {
        return Ok(false);
    }
    let Some(job) = pick_due_job(&state.pool).await? else {
        return Ok(false);
    };

    // 执行守卫：库中已有该条目且非显式强制重抓（priority=0）→ 跳过，不请求豆瓣。
    // 入队侧虽已收紧为只入缺失条目，守卫保证"已在库 → 零请求"在任何入队路径下都成立
    //（如清理 job 后被旧路径重排、未来入队逻辑演化）。
    if job.priority != 0 {
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM subjects WHERE category = ? AND subject_id = ?")
                .bind(&job.category)
                .bind(&job.subject_id)
                .fetch_optional(&state.pool)
                .await?;
        if exists.is_some() {
            sqlx::query(
                "UPDATE subject_jobs SET state='ok', attempts=0, next_run_ms=NULL,
                        last_error=NULL, updated_ms=?
                 WHERE category=? AND subject_id=?",
            )
            .bind(ServerConfig::now_ms())
            .bind(&job.category)
            .bind(&job.subject_id)
            .execute(&state.pool)
            .await?;
            tracing::info!(
                subject = format!("{}/{}", job.category, job.subject_id),
                "跳过富集：subjects 已有该条目（非强制重抓）"
            );
            return Ok(true);
        }
    }

    let now = ServerConfig::now_ms();
    // 标记 running：崩溃后残留的 running 行也会被 pick_due_job 重新拾起
    let claimed = sqlx::query(
        "UPDATE subject_jobs SET state='running', updated_ms=? WHERE category=? AND subject_id=?
         AND state IN ('queued','running')
         AND (priority=0 OR NOT EXISTS (SELECT 1 FROM subject_cache_checks c
              WHERE c.category=subject_jobs.category AND c.subject_id=subject_jobs.subject_id AND c.hold_until_ms>?))",
    )
    .bind(now)
    .bind(&job.category)
    .bind(&job.subject_id)
    .bind(now)
    .execute(&state.pool)
    .await?;
    if claimed.rows_affected() == 0 {
        return Ok(true);
    }

    match fetcher.fetch_subject(&job.category, &job.subject_id).await {
        Ok(value) => {
            state.record_crawl_success();
            // 全局节流：与页面抓取共用令牌桶语义；rexxar 另按类别 AIMD 自适应
            //（movie 端点限流远严于 music/book，成功后间隔缓慢收缩）
            let interval = state
                .subject_interval_after_success(&job.category)
                .max(state.config.crawl_min_interval_secs)
                + rand::thread_rng().gen_range(0.0..1.0);
            tokio::time::sleep(std::time::Duration::from_secs_f64(interval)).await;

            let category = Category::from_key(&job.category)
                .with_context(|| format!("未知 category: {}", job.category))?;
            let row = map_rexxar(category, &value);
            upsert_subject_row(&state.pool, &job.category, &job.subject_id, &row, "rexxar").await?;
            sqlx::query(
                "UPDATE subject_jobs SET state='ok', attempts=0, next_run_ms=NULL,
                        last_error=NULL, updated_ms=?
                 WHERE category=? AND subject_id=?",
            )
            .bind(ServerConfig::now_ms())
            .bind(&job.category)
            .bind(&job.subject_id)
            .execute(&state.pool)
            .await?;
            tracing::info!(
                subject = format!("{}/{}", job.category, job.subject_id),
                title = %row.title,
                "条目元信息富集完成"
            );
        }
        Err(e) => handle_job_error(state, &job, e).await?,
    }
    Ok(true)
}

async fn pick_due_job(pool: &SqlitePool) -> Result<Option<SubjectJob>> {
    let row = sqlx::query(
        "SELECT category, subject_id, attempts, priority FROM subject_jobs
         WHERE state IN ('queued','running')
           AND (next_run_ms IS NULL OR next_run_ms <= ?)
           AND (priority=0 OR NOT EXISTS (SELECT 1 FROM subject_cache_checks c
                WHERE c.category=subject_jobs.category AND c.subject_id=subject_jobs.subject_id AND c.hold_until_ms>?))
         ORDER BY priority ASC, updated_ms ASC, rowid ASC
         LIMIT 1",
    )
    .bind(ServerConfig::now_ms())
    .bind(ServerConfig::now_ms())
    .fetch_optional(pool)
    .await
    .context("查询到期富集 job 失败")?;
    Ok(row.map(|r| SubjectJob {
        category: r.get("category"),
        subject_id: r.get("subject_id"),
        attempts: r.get("attempts"),
        priority: r.get("priority"),
    }))
}

/// 失败处理对齐维度抓取，并在 rexxar 限流特性上加固（2026-10-28 实测：
/// movie 端点连发 4-8 个请求即 400 封锁约 5-6 分钟，404 同样累积触发；
/// 见《豆瓣源依赖性与脆弱性分析.md》）：
/// - 404 → 永久终态 not_found（顺带推迟同类 job——404 也会累积触发封锁）；
/// - 限流（4xx）→ 该类别 AIMD 升档 + 类别冷却（同类 job 全部推迟），
///   避免同类连发把全局熔断每 10 分钟拖停一轮；
/// - 指数退避 20s·3^(n-1)，跨过封锁窗口，不再于封锁期内烧光尝试次数；
/// - 重试耗尽 → 不终态 error 而是**落 intro 兜底行**（source='intro'），
///   数据降级但齐整、不再重试；records 无该条目（理论不可达）才维持 error；
/// - 可重试失败仍计入全局熔断（类别冷却已把同类失败隔开，熔断是最后防线）。
async fn handle_job_error(state: &AppState, job: &SubjectJob, e: FetchError) -> Result<()> {
    let now = ServerConfig::now_ms();
    tracing::warn!(
        subject = format!("{}/{}", job.category, job.subject_id),
        attempt = job.attempts + 1,
        error = %e,
        "条目富集失败"
    );
    let attempts = job.attempts + 1;
    let permanent = !e.is_transient();

    if permanent {
        // 404：条目已被豆瓣删除，终态。不计熔断（非拒绝服务），
        // 但 404 连发同样会触发端点封锁，顺带推迟同类 job
        defer_category_jobs(state, &job.category).await?;
        sqlx::query(
            "UPDATE subject_jobs SET state='not_found', attempts=?, next_run_ms=NULL,
                    last_error=?, updated_ms=?
             WHERE category=? AND subject_id=?",
        )
        .bind(attempts)
        .bind(format!("{}: {e}", e.code()))
        .bind(now)
        .bind(&job.category)
        .bind(&job.subject_id)
        .execute(&state.pool)
        .await?;
        return Ok(());
    }

    // 限流实锤（4xx）：类别 AIMD 升档 + 同类冷却，保证后续失败被其他类别的
    // 成功隔开（成功会重置熔断连续计数），不再形成"5 连败 → 全局停 10 分钟"循环
    let mut defer_until: Option<i64> = None;
    if matches!(e, FetchError::Blocked(_)) {
        state.subject_interval_after_block(&job.category);
        defer_until = Some(defer_category_jobs(state, &job.category).await?);
    }

    let over_limit = attempts >= MAX_ATTEMPTS;
    let (job_state, next_run, last_error) = if over_limit {
        match write_intro_fallback(state, job).await? {
            true => (
                "ok",
                None,
                format!("已降级 intro 兜底（重试耗尽）: {}: {e}", e.code()),
            ),
            false => ("error", None, format!("{}: {e}", e.code())),
        }
    } else {
        // 指数退避：20s / 60s / 180s / 540s（第 5 次耗尽走兜底）
        let backoff_ms = 20_000 * 3_i64.pow((attempts as u32).saturating_sub(1))
            + rand::thread_rng().gen_range(0..5_000);
        let next = now + backoff_ms;
        // 被限流时自身重试也须等类别冷却结束（它就是下一轮的探测请求）
        let next = defer_until.map_or(next, |until| next.max(until));
        ("queued", Some(next), format!("{}: {e}", e.code()))
    };

    sqlx::query(
        "UPDATE subject_jobs SET state=?, attempts=?, next_run_ms=?, last_error=?, updated_ms=?
         WHERE category=? AND subject_id=?",
    )
    .bind(job_state)
    .bind(attempts)
    .bind(next_run)
    .bind(last_error)
    .bind(now)
    .bind(&job.category)
    .bind(&job.subject_id)
    .execute(&state.pool)
    .await?;
    // 计入全局熔断：正常情况下类别冷却已隔开同类失败、其他类别的成功会
    // 重置计数；连续 5 次失败意味着多端点同时被拒，全局暂停是正确反应
    let tripped = state.record_crawl_failure();
    if tripped {
        tracing::info!("熔断已触发，worker 将暂停抓取直至冷却结束");
    }
    Ok(())
}

/// 类别冷却：把该类别全部非强制 job 推迟到冷却期后（MAX 保证不缩短既有
/// 更长退避），返回冷却截止时间。priority=0 的显式强制重排不受限
/// （管理员明确要求立即执行）。
async fn defer_category_jobs(state: &AppState, category: &str) -> Result<i64> {
    let until = ServerConfig::now_ms()
        + state.config.subject_category_cooldown_secs * 1000
        + rand::thread_rng().gen_range(0..30_000);
    sqlx::query(
        "UPDATE subject_jobs
         SET next_run_ms = MAX(COALESCE(next_run_ms, 0), ?)
         WHERE category = ? AND state IN ('queued','running') AND priority != 0",
    )
    .bind(until)
    .bind(category)
    .execute(&state.pool)
    .await
    .context("类别冷却推迟富集 job 失败")?;
    Ok(until)
}

/// 重试耗尽的兜底：以 records.intro（收藏页随抓取入库，零额外请求）合成
/// 降级行写入 subjects（source='intro'，无豆瓣评分、封面为收藏页小图），
/// 形状与 rexxar 行一致；job 置 ok，重试链终结。升级路径：单条 refresh
/// 或 requeue scope=stale（TTL 过期后）→ priority=0 强制 → rexxar 成功即
/// 覆盖为权威数据。返回 false = records 无该条目（入队来源即 records，
/// 理论不可达），调用方维持 error 终态。
async fn write_intro_fallback(state: &AppState, job: &SubjectJob) -> Result<bool> {
    let row = sqlx::query(
        "SELECT title, alt_title, subject_url, cover, intro FROM records
         WHERE category = ? AND subject_id = ? LIMIT 1",
    )
    .bind(&job.category)
    .bind(&job.subject_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(r) = row else { return Ok(false) };
    let record = crate::crawler::Record {
        category: job.category.clone(),
        status: String::new(),
        title: r.get("title"),
        alt_title: r.get("alt_title"),
        my_rating: None,
        date: String::new(),
        comment: String::new(),
        subject_url: r.get("subject_url"),
        subject_id: job.subject_id.clone(),
        cover: r.get("cover"),
        comment_id: String::new(),
        intro: r.get("intro"),
    };
    let subject_row = intro_fallback_row(&record);
    upsert_subject_row(
        &state.pool,
        &job.category,
        &job.subject_id,
        &subject_row,
        "intro",
    )
    .await?;
    tracing::warn!(
        subject = format!("{}/{}", job.category, job.subject_id),
        title = %subject_row.title,
        "富集重试耗尽，已落 intro 兜底行（可强制重排升级）"
    );
    Ok(true)
}

async fn upsert_subject_row(
    pool: &SqlitePool,
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
    .execute(pool)
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
    let res = sqlx::query("DELETE FROM subject_jobs WHERE category=? AND subject_id=?")
        .bind(category)
        .bind(subject_id)
        .execute(pool)
        .await
        .context("删除富集 job 失败")?;
    if res.rows_affected() == 0 {
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
    let placeholders = vec!["?"; states.len()].join(",");
    let mut sql = format!("DELETE FROM subject_jobs WHERE state IN ({placeholders})");
    if category.is_some_and(|c| !c.is_empty() && c != "all") {
        sql.push_str(" AND category = ?");
    }
    let mut query = sqlx::query(&sql);
    for st in states {
        query = query.bind(st);
    }
    if let Some(c) = category {
        query = query.bind(c);
    }
    Ok(query.execute(pool).await?.rows_affected())
}

/// POST /subjects/jobs/requeue：批量重排。
/// - failed / not_found：对应终态 job 重新排队（priority=0 插队；not_found 多为
///   条目被豆瓣删除，重排仅在怀疑误判时使用）；
/// - stale：subjects 中已过 TTL 的条目强制重抓（priority=0 绕过执行守卫）——
///   这是"重新抓取库中已有条目"唯一的批量入口。
pub async fn requeue_jobs(state: &AppState, scope: &str, category: Option<&str>) -> Result<u64> {
    let now = ServerConfig::now_ms();
    let has_category = category.is_some_and(|c| !c.is_empty() && c != "all");
    match scope {
        "failed" | "not_found" => {
            let from = if scope == "failed" {
                "error"
            } else {
                "not_found"
            };
            let mut sql = format!(
                "UPDATE subject_jobs SET state='queued', attempts=0, next_run_ms=NULL,
                        last_error=NULL, priority=0, updated_ms=?
                 WHERE state = '{from}'"
            );
            if has_category {
                sql.push_str(" AND category = ?");
            }
            let mut query = sqlx::query(&sql).bind(now);
            if let Some(c) = category {
                query = query.bind(c);
            }
            Ok(query.execute(&state.pool).await?.rows_affected())
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
                   priority=0, updated_ms=excluded.updated_ms \
                   WHERE subject_jobs.state NOT IN ('queued','running')",
            );
            let mut query = sqlx::query(&sql).bind(now).bind(now).bind(stale_before);
            if let Some(c) = category {
                query = query.bind(c);
            }
            Ok(query.execute(&state.pool).await?.rows_affected())
        }
        _ => unreachable!("scope 已在路由层校验"),
    }
}

// --------------------------------------------------------------------------- //
// 读侧：API 视图
// --------------------------------------------------------------------------- //

/// subjects 行 → 对外 JSON（结构见方案 §7.1）
fn subject_view(category: &str, subject_id: &str, r: &sqlx::sqlite::SqliteRow) -> Value {
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
    if let Some(row) = sqlx::query(
        "SELECT title, original_title, year, aka_json, cover_url, rating_value,
                rating_count, meta_json, status, fetched_ms, url, source
         FROM subjects WHERE category = ? AND subject_id = ?",
    )
    .bind(category)
    .bind(subject_id)
    .fetch_optional(&state.pool)
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
    .fetch_optional(&state.pool)
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
fn intro_fallback_view(record: &crate::crawler::Record) -> Value {
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
