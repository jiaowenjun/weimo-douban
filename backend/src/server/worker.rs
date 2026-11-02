//! 抓取 worker——weimo-ocr image_sync 模式：数据库即队列，单 worker 串行
//! （天然实现豆瓣侧全局限速），watch channel 即时唤醒 + 60s 兜底扫描。

use anyhow::{Context, Result};
use rand::Rng;
use sqlx::{Row, SqlitePool};
use std::time::Duration;
use tracing::{info, warn};

use crate::crawler::{
    self, collection_url, parse_items, parse_total, referer_for, Category, Record, Status,
};

use super::config::ServerConfig;
use super::fetcher::{DoubanFetcher, FetchError};
use super::service;
use super::state::AppState;

/// 单维度最大尝试次数（含首次）
const MAX_ATTEMPTS: i64 = 5;
const IDLE_SCAN: Duration = Duration::from_secs(60);

pub(super) struct DueDimension {
    uid: String,
    category: String,
    dim_status: String,
    next_start: i64,
    attempts: i64,
    staging: bool,
}

pub async fn run_worker<F: DoubanFetcher>(state: AppState, fetcher: F) {
    let mut wakeup = state.worker_wakeup_receiver();
    loop {
        // 熔断期间不抓取，等冷却结束（分段等待，最长 60s 一段）
        let circuit_ms = state.circuit_remaining_ms();
        if circuit_ms > 0 {
            tokio::time::sleep(Duration::from_millis(circuit_ms.min(60_000) as u64)).await;
            continue;
        }

        // Subject results commit before pacing; only the shared minimum gates all requests.
        // Category-specific deadlines leave other categories available to the same worker.
        let global_delay = super::subject_queue::global_delay_ms(&state.pool)
            .await
            .unwrap_or(1000);
        if global_delay > 0 {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(global_delay.min(60_000) as u64)) => {},
                _ = wakeup.changed() => {},
            }
            continue;
        }
        match process_next_due(&state, &fetcher).await {
            Ok(true) => {} // 处理了一个维度，立即取下一个
            Ok(false) => {
                // 维度队列空闲：处理条目富集 job（rexxar），同样串行共享限速/熔断；
                // 无到期 job 时进入等待（wakeup 即时唤醒 + 60s 兜底扫描）
                match crate::server::subject::process_next_job(&state, &fetcher).await {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(e) => {
                        warn!(error = %e, "subject job tick failed");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                }
                // A crashed/disabled center client cannot send a wakeup when its
                // short lease expires. Revisit at that deadline instead of making
                // normal jobs wait for the 60s idle scan after an App restart.
                let now = ServerConfig::now_ms();
                let lease_until: Option<i64> = sqlx::query_scalar(
                    "SELECT MIN(hold_until_ms) FROM subject_cache_checks WHERE hold_until_ms>?",
                )
                .bind(now)
                .fetch_one(&state.pool)
                .await
                .unwrap_or(None);
                let mut idle_scan = lease_until
                    .map(|until| Duration::from_millis((until - now).max(1) as u64))
                    .unwrap_or(IDLE_SCAN)
                    .min(IDLE_SCAN);
                if let Ok(Some(delay)) = super::subject_queue::next_delay_ms(&state).await {
                    idle_scan = idle_scan.min(Duration::from_millis(delay as u64));
                }
                tokio::select! {
                    changed = wakeup.changed() => {
                        if changed.is_err() {
                            warn!("worker wakeup channel closed");
                            tokio::time::sleep(IDLE_SCAN).await;
                        }
                    }
                    _ = tokio::time::sleep(idle_scan) => {}
                }
            }
            Err(e) => {
                warn!(error = %e, "worker tick failed");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// 处理一个到期维度（一页抓取 + 状态推进）。返回是否处理了维度；
/// 供 run_worker 循环与测试复用（测试可直接驱动而无需起后台 worker）。
pub async fn process_next_due<F: DoubanFetcher>(state: &AppState, fetcher: &F) -> Result<bool> {
    match pick_due_dimension(&state.pool).await? {
        Some(dim) => {
            process_dimension(state, fetcher, &dim).await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

async fn pick_due_dimension(pool: &SqlitePool) -> Result<Option<DueDimension>> {
    let row = sqlx::query(
        "SELECT uid, category, dim_status, next_start, attempts, staging
         FROM dimensions
         WHERE state IN ('queued','running')
           AND (next_sync_ms IS NULL OR next_sync_ms <= ?)
         ORDER BY updated_ms ASC, rowid ASC
         LIMIT 1",
    )
    .bind(ServerConfig::now_ms())
    .fetch_optional(pool)
    .await
    .context("查询到期维度失败")?;

    Ok(row.map(|r| DueDimension {
        uid: r.get("uid"),
        category: r.get("category"),
        dim_status: r.get("dim_status"),
        next_start: r.get("next_start"),
        attempts: r.get("attempts"),
        staging: r.get::<i64, _>("staging") != 0,
    }))
}

async fn process_dimension<F: DoubanFetcher>(
    state: &AppState,
    fetcher: &F,
    dim: &DueDimension,
) -> Result<()> {
    let Some(cat) = Category::from_key(&dim.category) else {
        anyhow::bail!("未知 category: {}", dim.category);
    };
    let Some(st) = Status::from_key(&dim.dim_status) else {
        anyhow::bail!("未知 status: {}", dim.dim_status);
    };
    let now = ServerConfig::now_ms();

    // 首次处理的维度把关联任务置为 running
    sqlx::query(
        "UPDATE tasks SET status = 'running', started_ms = COALESCE(started_ms, ?)
         WHERE status = 'queued'
           AND task_id IN (SELECT task_id FROM task_dims
                           WHERE uid = ? AND category = ? AND dim_status = ? AND state = 'waiting')",
    )
    .bind(now)
    .bind(&dim.uid)
    .bind(&dim.category)
    .bind(&dim.dim_status)
    .execute(&state.pool)
    .await
    .context("标记关联任务 running 失败")?;

    let start = dim.next_start.max(0) as usize;
    let url = collection_url(cat, &dim.uid, st, start);
    let referer = referer_for(&dim.uid);

    match fetcher.fetch_page(&url, &referer).await {
        Ok(html) => {
            state.record_crawl_success();
            // 全局节流：单 worker 串行 + 每次成功抓取后休眠 = 全局令牌桶，
            // 覆盖跨维度连续抓取的场景
            let interval =
                state.config.crawl_min_interval_secs + rand::thread_rng().gen_range(0.0..1.0);
            tokio::time::sleep(Duration::from_secs_f64(interval)).await;
            let total = parse_total(&html).map(|t| t as i64);
            let items = parse_items(&html, cat, st);
            // 解析哨兵：豆瓣改版/登录墙/JS challenge 会以 200+正常体量+0 条呈现，
            // 必须在写库与维度推进前拦截，否则静默清空
            //（《豆瓣源依赖性与脆弱性分析.md》§3 F1/F2）
            if let Some(reason) = parse_anomaly(&items, total, start) {
                handle_fetch_error(state, dim, FetchError::ParseAnomaly(reason)).await?;
            } else {
                process_page(state, dim, start, &html, total, items).await?;
            }
        }
        Err(e) => handle_fetch_error(state, dim, e).await?,
    }

    // 通知该维度关联的所有任务（SSE 进度推流；无订阅者时为空操作）
    let linked: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT task_id FROM task_dims
         WHERE uid = ? AND category = ? AND dim_status = ?",
    )
    .bind(&dim.uid)
    .bind(&dim.category)
    .bind(&dim.dim_status)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    for task_id in linked {
        state.notify_progress(&task_id);
    }
    Ok(())
}

/// 正常页面的入库与维度推进：资料块顺带解析、写库、终点判定。
/// 由 process_dimension 在解析哨兵通过后调用。
async fn process_page(
    state: &AppState,
    dim: &DueDimension,
    start: usize,
    html: &str,
    total: Option<i64>,
    items: Vec<Record>,
) -> Result<()> {
    let now = ServerConfig::now_ms();
    let empty_page = items.is_empty();

    // 页面顶部资料块顺带解析：头像/用户名随任意一页抓取即可获得
    if let Some(profile) = crawler::parse_profile(html) {
        if let Err(e) = service::upsert_user_profile(&state.pool, &dim.uid, &profile).await {
            // 资料是顺带产物，失败不阻断条目入库
            warn!(uid = %dim.uid, error = %e, "用户资料入库失败");
        }
    }

    // 本页写库；主键冲突即重复（翻页偏移/标记新增），交给 INSERT OR IGNORE
    let table = if dim.staging {
        "record_staging"
    } else {
        "records"
    };
    service::write_records(
        &state.pool,
        table,
        &dim.uid,
        &dim.category,
        &dim.dim_status,
        &items,
    )
    .await?;
    let fetched_count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {table} WHERE uid = ? AND category = ? AND dim_status = ?",
    ))
    .bind(&dim.uid)
    .bind(&dim.category)
    .bind(&dim.dim_status)
    .fetch_one(&state.pool)
    .await
    .context("统计维度记录数失败")?;

    // 终点判定：next_start 越过总数；无 total 时只能靠空页终止。
    // 不以"本页不满 15 条"判定——页面可能因条目被删而少渲染。
    let next_start = (start + crawler::PAGE_SIZE) as i64;
    let finished = match total {
        Some(t) => next_start >= t,
        None => empty_page,
    };

    if finished {
        let mut tx = state.pool.begin().await?;
        if dim.staging {
            // Reconcile rather than delete/reinsert: unchanged rows retain their identity.
            sqlx::query("DELETE FROM records WHERE uid=? AND category=? AND dim_status=?
                AND subject_id NOT IN (SELECT subject_id FROM record_staging WHERE uid=? AND category=? AND dim_status=?)")
                .bind(&dim.uid).bind(&dim.category).bind(&dim.dim_status)
                .bind(&dim.uid).bind(&dim.category).bind(&dim.dim_status)
                .execute(&mut *tx).await?;
            sqlx::query("INSERT INTO records SELECT * FROM record_staging WHERE uid=? AND category=? AND dim_status=?
                ON CONFLICT(uid, category, dim_status, subject_id) DO UPDATE SET
                title=excluded.title, alt_title=excluded.alt_title, my_rating=excluded.my_rating,
                date=excluded.date, comment=excluded.comment, subject_url=excluded.subject_url,
                cover=excluded.cover, comment_id=excluded.comment_id, intro=excluded.intro")
                .bind(&dim.uid).bind(&dim.category).bind(&dim.dim_status).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM record_staging WHERE uid=? AND category=? AND dim_status=?")
                .bind(&dim.uid)
                .bind(&dim.category)
                .bind(&dim.dim_status)
                .execute(&mut *tx)
                .await?;
        } else {
            // 直写路径收尾：防御性清掉该维度可能残存的暂存行（正常直写流
            // 不产生，历史中断换路径等场景兜底），维持终态不变量——ok 维度
            // 在 record_staging 无行
            sqlx::query("DELETE FROM record_staging WHERE uid=? AND category=? AND dim_status=?")
                .bind(&dim.uid)
                .bind(&dim.category)
                .bind(&dim.dim_status)
                .execute(&mut *tx)
                .await?;
        }
        let fetched_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM records WHERE uid=? AND category=? AND dim_status=?",
        )
        .bind(&dim.uid)
        .bind(&dim.category)
        .bind(&dim.dim_status)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE dimensions SET state='ok', staging=0, fetched_count=?, total=?,
                    next_start=?, fetched_ms=?, attempts=0, next_sync_ms=NULL,
                    last_error=NULL, updated_ms=?
             WHERE uid=? AND category=? AND dim_status=?",
        )
        .bind(fetched_count)
        .bind(total)
        .bind(next_start)
        .bind(now)
        .bind(now)
        .bind(&dim.uid)
        .bind(&dim.category)
        .bind(&dim.dim_status)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        info!(
            uid = %dim.uid,
            dimension = format!("{}/{}", dim.category, dim.dim_status),
            items = fetched_count,
            "维度抓取完成"
        );
        service::finalize_linked_tasks(state, &dim.uid, &dim.category, &dim.dim_status, true, "")
            .await?;
        // 维度完成即入队条目富集（集合差 + INSERT OR IGNORE 跨用户去重）。
        // 只入缺失条目：subjects 已有的不重抓。失败不阻断主流程——
        // 富集是异步增强，缺席只是少了结构化字段。
        if state.config.subject_enrich {
            if let Err(e) = crate::server::subject::enqueue_missing_subjects(
                &state.pool,
                &dim.uid,
                &dim.category,
            )
            .await
            {
                warn!(uid = %dim.uid, error = %e, "条目富集入队失败");
            }
        }
    } else {
        sqlx::query(
            "UPDATE dimensions SET state='running', fetched_count=?, total=?,
                    next_start=?, updated_ms=?
             WHERE uid=? AND category=? AND dim_status=?",
        )
        .bind(fetched_count)
        .bind(total)
        .bind(next_start)
        .bind(now)
        .bind(&dim.uid)
        .bind(&dim.category)
        .bind(&dim.dim_status)
        .execute(&state.pool)
        .await?;
    }
    Ok(())
}

/// 解析哨兵：区分"合法空集"与"页面改版/登录墙导致的静默空页"
/// （《豆瓣源依赖性与脆弱性分析.md》§3 F1/F2）。
///
/// 返回 Some(原因) 表示本页解析结果不可信，维度按 PARSE_ERROR 处理：
/// - 单页条数超过约定页大小：豆瓣疑似调整分页，next_start 15 步进会漏条目；
/// - 首页 0 条但 total>0：条目全部被隐藏不合常理，疑似改版/被墙；
/// - 中段整页 0 条：需连续 15+ 条全部被隐藏才合法，不合常理。
///
/// 合法空集不告警：total 未知（空页即终止信号）、total=0，以及尾页探测为空
/// （隐藏条目使 total 虚高于可见数，见 README 已知限制）。
fn parse_anomaly(items: &[Record], total: Option<i64>, start: usize) -> Option<String> {
    if items.len() > crawler::PAGE_SIZE {
        return Some(format!(
            "单页 {} 条超过约定页大小 {}，豆瓣疑似调整分页",
            items.len(),
            crawler::PAGE_SIZE
        ));
    }
    let Some(t) = total else { return None };
    if !items.is_empty() || t <= 0 {
        return None;
    }
    if start == 0 {
        Some(format!("首页解析 0 条但 total={t}"))
    } else if start + crawler::PAGE_SIZE < t as usize {
        Some(format!("第 {start} 条起整页为空但 total={t}"))
    } else {
        None // 尾页探测为空：隐藏条目虚高 total，合法
    }
}

async fn handle_fetch_error(state: &AppState, dim: &DueDimension, e: FetchError) -> Result<()> {
    let now = ServerConfig::now_ms();
    let code = e.code();
    warn!(
        uid = %dim.uid,
        dimension = format!("{}/{}", dim.category, dim.dim_status),
        attempt = dim.attempts + 1,
        error = %e,
        "维度抓取失败"
    );

    // 404 永久失败；其余按次数退避重试，超限终态失败
    let permanent = !e.is_transient();
    let over_limit = dim.attempts + 1 >= MAX_ATTEMPTS;
    if permanent || over_limit {
        sqlx::query(
            "UPDATE dimensions SET state='error', attempts=?, next_sync_ms=NULL,
                    last_error=?, updated_ms=?
             WHERE uid=? AND category=? AND dim_status=?",
        )
        .bind(dim.attempts + 1)
        .bind(format!("{code}: {e}"))
        .bind(now)
        .bind(&dim.uid)
        .bind(&dim.category)
        .bind(&dim.dim_status)
        .execute(&state.pool)
        .await?;
        if !permanent {
            state.record_crawl_failure();
        }
        service::finalize_linked_tasks(
            state,
            &dim.uid,
            &dim.category,
            &dim.dim_status,
            false,
            code,
        )
        .await?;
    } else {
        let tripped = state.record_crawl_failure();
        let backoff_ms = (dim.attempts + 1) * 20_000 + rand::thread_rng().gen_range(0..5_000);
        sqlx::query(
            "UPDATE dimensions SET attempts=?, next_sync_ms=?, last_error=?, updated_ms=?
             WHERE uid=? AND category=? AND dim_status=?",
        )
        .bind(dim.attempts + 1)
        .bind(now + backoff_ms)
        .bind(format!("{code}: {e}"))
        .bind(now)
        .bind(&dim.uid)
        .bind(&dim.category)
        .bind(&dim.dim_status)
        .execute(&state.pool)
        .await?;
        if tripped {
            info!("熔断已触发，worker 将暂停抓取直至冷却结束");
        }
    }
    Ok(())
}

// --------------------------------------------------------------------------- //
// 解析哨兵单测：合法空集与改版空页的判定边界
// --------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anomaly_on_page_size_breach() {
        let items = vec![Record::default(); 16];
        assert!(parse_anomaly(&items, Some(100), 0).is_some());
    }

    #[test]
    fn anomaly_on_first_page_empty_with_total() {
        // 改版/登录墙的典型形态：200 + total>0 + 首页 0 条
        assert!(parse_anomaly(&[], Some(45), 0).is_some());
        // 小维度（total ≤ 15）同样不能幸免——全部隐藏不合常理
        assert!(parse_anomaly(&[], Some(8), 0).is_some());
    }

    #[test]
    fn anomaly_on_middle_page_empty() {
        // start=30，30+15=45 < 100：中段整页为空
        assert!(parse_anomaly(&[], Some(100), 30).is_some());
    }

    #[test]
    fn legit_empty_tail_probe_passes() {
        // 隐藏条目使 total 虚高：最后一页可见 0 条属正常（start+15 >= total 且已抓过前页）
        assert!(parse_anomaly(&[], Some(45), 30).is_none());
        assert!(parse_anomaly(&[], Some(46), 45).is_none());
    }

    #[test]
    fn legit_empty_dimension_passes() {
        // 用户真的没有标记：total 未知（页面无 subject-num）或 0
        assert!(parse_anomaly(&[], None, 0).is_none());
        assert!(parse_anomaly(&[], Some(0), 0).is_none());
    }

    #[test]
    fn normal_page_never_flagged() {
        let items = vec![Record::default(); 15];
        assert!(parse_anomaly(&items, Some(208), 0).is_none());
        assert!(parse_anomaly(&items, None, 15).is_none());
    }
}
