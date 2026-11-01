//! Public, bounded metadata export. Never touches records, jobs, cookies or the crawler.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use tokio::sync::Semaphore;

use super::state::AppState;

pub const BATCH_LIMIT: usize = 100;
pub const RESPONSE_LIMIT: usize = 256 * 1024;
pub const ITEM_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct SubjectKey {
    pub category: String,
    pub subject_id: String,
}

impl SubjectKey {
    pub fn valid(&self) -> bool {
        matches!(self.category.as_str(), "movie" | "book" | "music")
            && !self.subject_id.is_empty()
            && self.subject_id.len() <= 20
            && self.subject_id.bytes().all(|b| b.is_ascii_digit())
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BatchRequest {
    pub items: Vec<SubjectKey>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CachedSubject {
    #[serde(flatten)]
    pub key: SubjectKey,
    pub title: String,
    pub original_title: String,
    pub year: Option<i64>,
    pub aka: Vec<String>,
    pub url: String,
    pub cover_url: String,
    pub rating_value: Option<f64>,
    pub rating_count: Option<i64>,
    pub meta: Value,
    pub fetched_ms: i64,
    pub status: String,
    pub source: String,
}

impl CachedSubject {
    pub fn valid(&self, now: i64) -> bool {
        if !self.key.valid()
            || self.source != "rexxar"
            || self.status != "ok"
            || self.title.trim().is_empty()
            || self.title.len() > 4096
            || self.fetched_ms <= 0
            || self.fetched_ms > now + 300_000
            || self
                .rating_value
                .is_some_and(|v| !v.is_finite() || !(0.0..=10.0).contains(&v))
            || self.rating_count.is_some_and(|n| n < 0)
            || !valid_url(&self.url, false)
            || !valid_url(&self.cover_url, true)
        {
            return false;
        }
        let Some(meta) = self.meta.as_object() else {
            return false;
        };
        for (key, value) in meta {
            let valid = match key.as_str() {
                "intro" | "subtitle" | "series" => value.is_string(),
                "is_tv" => value.is_null() || value.is_boolean(),
                "episodes_count" => value.is_null() || value.as_i64().is_some_and(|n| n >= 0),
                "directors" | "actors" | "genres" | "countries" | "languages" | "durations"
                | "pubdate" | "authors" | "translators" | "press" | "pages" | "price"
                | "singers" | "media" | "discs" | "publisher" | "version" | "songs" => value
                    .as_array()
                    .is_some_and(|a| a.iter().all(Value::is_string)),
                _ => true, // Future additive fields do not invalidate schema v1.
            };
            if !valid {
                return false;
            }
        }
        serde_json::to_vec(self).is_ok_and(|b| b.len() <= ITEM_LIMIT)
    }
}

fn valid_url(value: &str, cover: bool) -> bool {
    if value.is_empty() {
        return true;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    let host = url.host_str().unwrap_or_default();
    matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && (host == "douban.com"
            || host.ends_with(".douban.com")
            || (cover && (host == "doubanio.com" || host.ends_with(".doubanio.com"))))
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BatchResponse {
    pub schema_version: u32,
    pub items: Vec<CachedSubject>,
    pub missing: Vec<SubjectKey>,
}

/// One separate read-only connection; lazy opening does not affect server startup.
pub struct CacheServer {
    pub pool: SqlitePool,
    slots: Semaphore,
    budget: Mutex<(Instant, f64)>,
}

impl CacheServer {
    pub fn new(path: &Path) -> Self {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .busy_timeout(Duration::from_millis(100));
        Self {
            pool: SqlitePoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_millis(100))
                .connect_lazy_with(options),
            slots: Semaphore::new(2),
            budget: Mutex::new((Instant::now(), 2.0)),
        }
    }

    fn allow(&self) -> bool {
        let mut budget = self.budget.lock().unwrap();
        let now = Instant::now();
        budget.1 = (budget.1 + now.duration_since(budget.0).as_secs_f64() * 2.0).min(2.0);
        budget.0 = now;
        if budget.1 < 1.0 {
            return false;
        }
        budget.1 -= 1.0;
        true
    }
}

pub fn router(state: &AppState) -> Router {
    match &state.subject_cache {
        Some(cache) => Router::new()
            .route("/subjects/batch", post(batch))
            .layer(DefaultBodyLimit::max(16 * 1024))
            .with_state(cache.clone()),
        None => Router::new(),
    }
}

fn unavailable(status: StatusCode) -> Response {
    (
        status,
        [
            (header::RETRY_AFTER, "60"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(json!({"error":{"code":"CACHE_UNAVAILABLE","message":"缓存繁忙，请使用本地抓取"}})),
    )
        .into_response()
}

async fn batch(
    State(cache): State<Arc<CacheServer>>,
    Json(request): Json<BatchRequest>,
) -> Response {
    if request.items.is_empty()
        || request.items.len() > BATCH_LIMIT
        || request.items.iter().any(|key| !key.valid())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":{"code":"INVALID_ARGUMENT"}})),
        )
            .into_response();
    }
    if !cache.allow() {
        return unavailable(StatusCode::TOO_MANY_REQUESTS);
    }
    let Ok(_permit) = cache.slots.try_acquire() else {
        return unavailable(StatusCode::SERVICE_UNAVAILABLE);
    };
    match tokio::time::timeout(
        Duration::from_millis(500),
        load_batch(&cache.pool, request.items),
    )
    .await
    {
        Ok(Ok(result)) => match serde_json::to_vec(&result) {
            Ok(body) if body.len() <= RESPONSE_LIMIT => (
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                body,
            )
                .into_response(),
            _ => unavailable(StatusCode::SERVICE_UNAVAILABLE),
        },
        _ => unavailable(StatusCode::SERVICE_UNAVAILABLE),
    }
}

async fn load_batch(pool: &SqlitePool, keys: Vec<SubjectKey>) -> anyhow::Result<BatchResponse> {
    let mut seen = HashSet::new();
    let keys: Vec<_> = keys
        .into_iter()
        .filter(|key| seen.insert(key.clone()))
        .collect();
    let mut found = HashMap::new();
    for category in ["movie", "book", "music"] {
        let ids: Vec<_> = keys.iter().filter(|key| key.category == category).collect();
        if ids.is_empty() {
            continue;
        }
        // Filter status/source after primary-key lookup: SQLite otherwise prefers
        // idx_subjects_retry(status) and scans all enriched entries for each batch.
        let sql = format!("SELECT subject_id,title,original_title,year,aka_json,url,cover_url,
            rating_value,rating_count,fetched_ms,status,source,
            CASE WHEN length(CAST(meta_json AS BLOB)) <= {ITEM_LIMIT} THEN meta_json ELSE NULL END AS meta_json
            FROM subjects WHERE category=? AND subject_id IN ({})", vec!["?";ids.len()].join(","));
        let mut query = sqlx::query(&sql).bind(category);
        for key in ids {
            query = query.bind(&key.subject_id);
        }
        let rows = query.fetch_all(pool).await?;
        for row in rows {
            if row.get::<String, _>("status") != "ok" || row.get::<String, _>("source") != "rexxar"
            {
                continue;
            }
            let Some(meta) = row.get::<Option<String>, _>("meta_json") else {
                continue;
            };
            let (Ok(meta), Ok(aka)) = (
                serde_json::from_str(&meta),
                serde_json::from_str(&row.get::<String, _>("aka_json")),
            ) else {
                continue;
            };
            let item = CachedSubject {
                key: SubjectKey {
                    category: category.into(),
                    subject_id: row.get("subject_id"),
                },
                title: row.get("title"),
                original_title: row.get("original_title"),
                year: row.get("year"),
                aka,
                url: row.get("url"),
                cover_url: row.get("cover_url"),
                rating_value: row.get("rating_value"),
                rating_count: row.get("rating_count"),
                meta,
                fetched_ms: row.get("fetched_ms"),
                status: "ok".into(),
                source: "rexxar".into(),
            };
            if item.valid(super::config::ServerConfig::now_ms()) {
                found.insert(item.key.clone(), item);
            }
        }
    }
    let mut response = BatchResponse {
        schema_version: 1,
        items: Vec::new(),
        missing: Vec::new(),
    };
    for key in keys {
        match found.remove(&key) {
            Some(item) => response.items.push(item),
            None => response.missing.push(key),
        }
    }
    Ok(response)
}
