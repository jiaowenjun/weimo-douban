//! Optional desktop acceleration. This scheduler never shares Douban pacing/circuit state.

use std::{
    collections::HashSet,
    sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use axum::{extract::State, Json};
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::sync::{Mutex, Notify};

use super::{
    config::ServerConfig,
    error::{AppError, AppResult},
    state::AppState,
    subject_cache::{
        BatchRequest, BatchResponse, CachedSubject, SubjectKey, BATCH_LIMIT, RESPONSE_LIMIT,
    },
};

const MISS_TTL_MS: i64 = 30 * 60 * 1000;
const HOLD_MS: i64 = 4_000; // Slightly longer than the entire 3s HTTP budget; expires after crashes.

pub struct CacheClient {
    url: url::Url,
    http: reqwest::Client,
    enabled: AtomicBool,
    generation: AtomicU64,
    blocked_until_ms: AtomicI64,
    changed: Notify,
    tick: Mutex<()>,
}

impl CacheClient {
    pub fn from_config(config: &ServerConfig) -> Option<Self> {
        let value = config.subject_cache_url.as_deref()?.trim();
        if value.is_empty() {
            return None;
        }
        let url = match url::Url::parse(value) {
            Ok(url)
                if url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none()
                    && url.query().is_none()
                    && (url.scheme() == "https"
                        || (url.scheme() == "http"
                            && matches!(
                                url.host_str(),
                                Some("127.0.0.1" | "localhost" | "[::1]")
                            ))) =>
            {
                url
            }
            _ => {
                tracing::warn!("中心缓存地址无效，继续使用本地抓取");
                return None;
            }
        };
        // Deliberately separate from ReqwestFetcher: no Douban cookies or referer,
        // no cross-host redirects, and no center credential required.
        let http = match reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(800))
            .timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("weimo-douban-subject-cache/1")
            .build()
        {
            Ok(client) => client,
            Err(_) => {
                tracing::warn!("中心缓存客户端不可用，继续使用本地抓取");
                return None;
            }
        };
        Some(Self {
            url,
            http,
            enabled: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            blocked_until_ms: AtomicI64::new(0),
            changed: Notify::new(),
            tick: Mutex::new(()),
        })
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    fn change_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.blocked_until_ms.store(0, Ordering::Release);
        self.changed.notify_waiters();
    }
}

pub async fn load_preferences(state: &AppState) -> Result<()> {
    if let Some(client) = &state.subject_cache_client {
        let value: Option<String> = sqlx::query_scalar(
            "SELECT value FROM app_preferences WHERE key='subject_cache_enabled'",
        )
        .fetch_optional(&state.pool)
        .await?;
        client.change_enabled(value.as_deref() != Some("false"));
    }
    Ok(())
}

pub async fn get_preferences(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"available":state.subject_cache_client.is_some(),
        "enabled":state.subject_cache_client.as_ref().is_some_and(|c| c.enabled())}))
}

#[derive(Deserialize)]
pub struct PreferencesInput {
    pub enabled: bool,
}

pub async fn set_preferences(
    State(state): State<AppState>,
    Json(input): Json<PreferencesInput>,
) -> AppResult<Json<Value>> {
    let Some(client) = &state.subject_cache_client else {
        return Err(AppError::InvalidArgument("中心缓存已由启动配置关闭".into()));
    };
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query(
        "INSERT INTO app_preferences(key,value) VALUES('subject_cache_enabled',?)
        ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .bind(if input.enabled { "true" } else { "false" })
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE subject_cache_checks SET hold_until_ms=0")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    client.change_enabled(input.enabled);
    state.wake_worker();
    Ok(Json(json!({"available":true,"enabled":input.enabled})))
}

pub async fn run(state: AppState) {
    let Some(client) = &state.subject_cache_client else {
        return;
    };
    let mut wakeup = state.worker_wakeup_receiver();
    // Spread desktop startup/recovery bursts. No startup or UI path awaits this task.
    let jitter = rand::thread_rng().gen_range(200..1500);
    tokio::time::sleep(Duration::from_millis(jitter)).await;
    loop {
        let processed = match sync_once(&state).await {
            Ok(n) => n > 0,
            Err(error) => {
                tracing::warn!(%error,"本地中心缓存任务失败，豆瓣抓取继续运行");
                false
            }
        };
        if processed {
            // sync_once wakes the source worker after releasing leases. Do not
            // consume our own wakeup as permission to skip center pacing.
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(1500)) => {},
                _ = client.changed.notified() => {},
            }
            wakeup.borrow_and_update();
            continue;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(5)) => {},
            _ = client.changed.notified() => {},
            _ = wakeup.changed() => {},
        }
    }
}

/// One bounded batch, exposed for integration tests with a local HTTP mock.
/// Returns attempted key count. Faults open the center circuit, release leases,
/// and leave normal Douban jobs runnable without changing their retry counters.
pub async fn sync_once(state: &AppState) -> Result<usize> {
    let Some(client) = &state.subject_cache_client else {
        return Ok(0);
    };
    let Ok(_tick) = client.tick.try_lock() else {
        return Ok(0);
    };
    let now = ServerConfig::now_ms();
    if !state.config.subject_enrich
        || !client.enabled()
        || client.blocked_until_ms.load(Ordering::Acquire) > now
    {
        return Ok(0);
    }
    let generation = client.generation.load(Ordering::Acquire);
    let hold_until = now + HOLD_MS;
    let keys = claim_batch(state, now, hold_until).await?;
    if keys.is_empty() {
        return Ok(0);
    }
    // Register the cancellation listener before checking the generation, avoiding
    // a lost notification when settings change between selection and send.
    let changed = client.changed.notified();
    tokio::pin!(changed);
    changed.as_mut().enable();
    let mut retry_secs = 60;
    let result = if !client.enabled() || generation != client.generation.load(Ordering::Acquire) {
        Err(anyhow::anyhow!("中心查询已关闭"))
    } else {
        tokio::select! {
            result = fetch_batch(client, &keys, &mut retry_secs) => result,
            _ = &mut changed => Err(anyhow::anyhow!("中心缓存设置已改变")),
        }
    };
    let count = keys.len();
    match result {
        Ok(response)
            if client.enabled() && generation == client.generation.load(Ordering::Acquire) =>
        {
            let imported = import_batch(state, response.items, generation).await?;
            release_batch(state, &keys, hold_until, now + MISS_TTL_MS).await?;
            tracing::info!(checked = count, imported, "中心缓存批量查询完成");
        }
        result => {
            let retry_ms =
                (retry_secs.clamp(30, 300) * 1000) + rand::thread_rng().gen_range(0..15_000);
            let until = ServerConfig::now_ms() + retry_ms;
            if generation == client.generation.load(Ordering::Acquire) {
                client.blocked_until_ms.store(until, Ordering::Release);
            }
            release_batch(state, &keys, hold_until, until).await?;
            if let Err(error) = result {
                tracing::info!(%error,"中心缓存暂不可用，使用本地抓取");
            }
        }
    }
    state.wake_worker();
    Ok(count)
}

async fn claim_batch(state: &AppState, now: i64, hold_until: i64) -> Result<Vec<SubjectKey>> {
    // The write reservation coordinates with the source worker's conditional
    // running transition. A center lease never races an already-running fetch.
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let rows = sqlx::query(
        "SELECT DISTINCT r.category,r.subject_id FROM records r
        LEFT JOIN subjects s ON s.category=r.category AND s.subject_id=r.subject_id
        LEFT JOIN subject_jobs j ON j.category=r.category AND j.subject_id=r.subject_id
        LEFT JOIN subject_cache_checks c ON c.category=r.category AND c.subject_id=r.subject_id
        WHERE (s.category IS NULL OR s.source='intro')
          AND (j.state IS NULL OR j.state!='running')
          AND NOT (COALESCE(j.priority,1)=0 AND COALESCE(j.state,'')='queued')
          AND COALESCE(c.next_check_ms,0)<=? AND COALESCE(c.hold_until_ms,0)<=?
        ORDER BY r.category,r.subject_id LIMIT ?",
    )
    .bind(now)
    .bind(now)
    .bind(BATCH_LIMIT as i64)
    .fetch_all(&mut *tx)
    .await?;
    let keys: Vec<_> = rows
        .iter()
        .map(|r| SubjectKey {
            category: r.get("category"),
            subject_id: r.get("subject_id"),
        })
        .filter(SubjectKey::valid)
        .collect();
    for key in &keys {
        sqlx::query(
            "INSERT INTO subject_cache_checks(category,subject_id,next_check_ms,hold_until_ms)
            VALUES(?,?,?,?) ON CONFLICT(category,subject_id) DO UPDATE SET
            next_check_ms=excluded.next_check_ms,hold_until_ms=excluded.hold_until_ms",
        )
        .bind(&key.category)
        .bind(&key.subject_id)
        .bind(hold_until)
        .bind(hold_until)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(keys)
}

async fn release_batch(
    state: &AppState,
    keys: &[SubjectKey],
    hold_until: i64,
    next_check: i64,
) -> Result<()> {
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    for key in keys {
        sqlx::query(
            "UPDATE subject_cache_checks SET hold_until_ms=0,next_check_ms=?
            WHERE category=? AND subject_id=? AND hold_until_ms=?",
        )
        .bind(next_check)
        .bind(&key.category)
        .bind(&key.subject_id)
        .bind(hold_until)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn fetch_batch(
    client: &CacheClient,
    keys: &[SubjectKey],
    retry_secs: &mut i64,
) -> Result<BatchResponse> {
    let body = serde_json::to_vec(&BatchRequest {
        items: keys.to_vec(),
    })?;
    let mut response = client
        .http
        .post(client.url.clone())
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .context("中心连接失败")?;
    if let Some(seconds) = response
        .headers()
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
    {
        *retry_secs = seconds;
    }
    if !response.status().is_success() {
        bail!("中心返回 HTTP {}", response.status().as_u16());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > RESPONSE_LIMIT {
            bail!("中心响应超过大小上限");
        }
        bytes.extend_from_slice(&chunk);
    }
    let result: BatchResponse = serde_json::from_slice(&bytes)?;
    if result.schema_version != 1 {
        bail!("中心协议版本不兼容");
    }
    let mut remaining: HashSet<_> = keys.iter().cloned().collect();
    for item in &result.items {
        if !remaining.remove(&item.key) || !item.valid(ServerConfig::now_ms()) {
            bail!("中心条目校验失败");
        }
    }
    for key in &result.missing {
        if !remaining.remove(key) {
            bail!("中心缺失列表校验失败");
        }
    }
    if !remaining.is_empty() {
        bail!("中心响应未覆盖请求条目");
    }
    Ok(result)
}

async fn import_batch(state: &AppState, items: Vec<CachedSubject>, generation: u64) -> Result<u64> {
    let Some(client) = &state.subject_cache_client else {
        return Ok(0);
    };
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    if !client.enabled() || client.generation.load(Ordering::Acquire) != generation {
        return Ok(0);
    }
    let mut imported = 0;
    for item in items {
        let job = sqlx::query(
            "SELECT priority,state FROM subject_jobs WHERE category=? AND subject_id=?",
        )
        .bind(&item.key.category)
        .bind(&item.key.subject_id)
        .fetch_optional(&mut *tx)
        .await?;
        if job.as_ref().is_some_and(|j| {
            j.get::<String, _>("state") == "running"
                || (j.get::<i64, _>("priority") == 0 && j.get::<String, _>("state") == "queued")
        }) {
            continue;
        }
        let result = sqlx::query("INSERT INTO subjects(category,subject_id,title,original_title,year,aka_json,url,cover_url,
            rating_value,rating_count,meta_json,raw_json,status,source,attempts,next_retry_ms,fetched_ms)
            VALUES(?,?,?,?,?,?,?,?,?,?,?,'','ok','rexxar',0,NULL,?)
            ON CONFLICT(category,subject_id) DO UPDATE SET title=excluded.title,original_title=excluded.original_title,
            year=excluded.year,aka_json=excluded.aka_json,url=excluded.url,cover_url=excluded.cover_url,
            rating_value=excluded.rating_value,rating_count=excluded.rating_count,meta_json=excluded.meta_json,
            raw_json='',status='ok',source='rexxar',attempts=0,next_retry_ms=NULL,fetched_ms=excluded.fetched_ms
            WHERE subjects.source!='rexxar' OR subjects.status!='ok' OR subjects.fetched_ms<excluded.fetched_ms")
            .bind(&item.key.category).bind(&item.key.subject_id).bind(&item.title).bind(&item.original_title)
            .bind(item.year).bind(serde_json::to_string(&item.aka)?).bind(&item.url).bind(&item.cover_url)
            .bind(item.rating_value).bind(item.rating_count).bind(item.meta.to_string()).bind(item.fetched_ms)
            .execute(&mut *tx).await?;
        imported += result.rows_affected();
        sqlx::query("UPDATE subject_jobs SET state='ok',attempts=0,next_run_ms=NULL,last_error=NULL,retry_since_ms=NULL,run_token=NULL,updated_ms=?
            WHERE category=? AND subject_id=? AND priority!=0 AND state!='running'")
            .bind(ServerConfig::now_ms()).bind(&item.key.category).bind(&item.key.subject_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(imported)
}
