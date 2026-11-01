//! Content versions and bounded caches for read-only user projections.
use std::{
    collections::{BTreeMap, HashMap},
    hash::{Hash, Hasher},
    sync::Arc,
};

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::State,
    http::{header, HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use sqlx::Row;
use tokio::sync::Mutex;

use super::{error::AppResult, state::AppState};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 128;

#[derive(Default)]
pub struct ProjectionCache {
    // Per-key locks merge concurrent misses; bounded together with their cached body.
    entries: HashMap<String, Arc<Mutex<Option<Bytes>>>>,
    bytes: usize,
}

#[derive(Serialize)]
pub struct Versions {
    pub profile: String,
    pub stats: String,
    pub movie: String,
    pub book: String,
    pub music: String,
    pub all: String,
    /// Paging guards exclude subject enrichment; the original fields still
    /// describe content freshness and serve clients using cache_version.
    pub movie_marks: String,
    pub book_marks: String,
    pub music_marks: String,
    pub all_marks: String,
    /// 富集待处理 job 数（queued+running，全局）——前端据此在同步终态
    /// 后维持高频版本轮询（富集逐条落库持续可见），pending 归零自动停
    pub subjects_pending: i64,
}

pub async fn versions(state: &AppState, uid: &str, status: &str) -> AppResult<Versions> {
    // One statement gives a consistent snapshot of the identity and all revisions.
    let rows = sqlx::query("SELECT i.value, v.scope, v.revision FROM cache_identity i
        LEFT JOIN cache_versions v ON v.scope IN (?, ?, ?, ?, 'subjects:movie', 'subjects:book', 'subjects:music') WHERE i.id=1")
        .bind(format!("profile:{uid}"))
        .bind(format!("{uid}:movie:{status}"))
        .bind(format!("{uid}:book:{status}"))
        .bind(format!("{uid}:music:{status}"))
        .fetch_all(&state.pool).await?;
    let identity: String = rows[0].get("value");
    let revisions: BTreeMap<String, i64> = rows
        .iter()
        .filter_map(|r| {
            r.get::<Option<String>, _>("scope")
                .map(|key| (key, r.get("revision")))
        })
        .collect();
    let revision = |key: String| revisions.get(&key).copied().unwrap_or(0);
    let marks: Vec<_> = ["movie", "book", "music"]
        .iter()
        .map(|cat| revision(format!("{uid}:{cat}:{status}")))
        .collect();
    let subjects: Vec<_> = ["movie", "book", "music"]
        .iter()
        .map(|cat| revision(format!("subjects:{cat}")))
        .collect();
    // pending 入队/出队不必然伴随 revision 变化（入队无触发器），单独
    // 计数；量级为 job 表行数（千级内），COUNT 毫秒级
    let subjects_pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM subject_jobs WHERE state IN ('queued','running')")
            .fetch_one(&state.pool)
            .await?;
    let base = format!("v1-{identity}-{status}");
    Ok(Versions {
        profile: format!("{base}-p{}", revision(format!("profile:{uid}"))),
        stats: format!("{base}-{:?}", marks),
        movie: format!("{base}-{}-{}", marks[0], subjects[0]),
        book: format!("{base}-{}-{}", marks[1], subjects[1]),
        music: format!("{base}-{}-{}", marks[2], subjects[2]),
        all: format!("{base}-{:?}-{:?}", marks, subjects),
        movie_marks: format!("{base}-m{}", marks[0]),
        book_marks: format!("{base}-m{}", marks[1]),
        music_marks: format!("{base}-m{}", marks[2]),
        all_marks: format!("{base}-m{:?}", marks),
        subjects_pending,
    })
}

fn version_for<'a>(
    versions: &'a Versions,
    endpoint: &str,
    category: Option<&str>,
) -> Option<&'a str> {
    match endpoint {
        "profile" => Some(&versions.profile),
        "stats" => Some(&versions.stats),
        "subject-dims" | "marks" => match category {
            Some("movie") => Some(&versions.movie),
            Some("book") => Some(&versions.book),
            Some("music") => Some(&versions.music),
            Some("all") if endpoint == "marks" => Some(&versions.all),
            _ => None,
        },
        "cache-version" => Some(&versions.all),
        _ => None,
    }
}

fn paging_version_for<'a>(versions: &'a Versions, category: Option<&str>) -> Option<&'a str> {
    match category {
        Some("movie") => Some(&versions.movie_marks),
        Some("book") => Some(&versions.book_marks),
        Some("music") => Some(&versions.music_marks),
        Some("all") => Some(&versions.all_marks),
        _ => None,
    }
}

fn changed_response() -> Response {
    (StatusCode::CONFLICT, Json(serde_json::json!({"error": {"code": "CACHE_VERSION_CHANGED", "message": "数据已更新，正在重新加载"}}))).into_response()
}

fn etag(key: &str) -> String {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hash);
    format!("W/\"{:x}\"", hash.finish())
}

fn matches(headers: &HeaderMap, tag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| {
            h.split(',').any(|t| {
                t.trim() == "*" || t.trim().trim_start_matches("W/") == tag.trim_start_matches("W/")
            })
        })
}

fn cache_headers(response: &mut Response, tag: &str) {
    response
        .headers_mut()
        .insert(header::ETAG, tag.parse().unwrap());
    // Browser-private validation: each newly issued request checks the data version.
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "private, no-cache".parse().unwrap());
}

/// Auth/rate middleware stays outside this layer, including for cache hits and 304s.
/// Only projection/profile responses are cached: marks contain time-dependent `fresh`.
pub async fn middleware(State(state): State<AppState>, req: Request<Body>, next: Next) -> Response {
    if req.method() != axum::http::Method::GET {
        return next.run(req).await;
    }
    let segments: Vec<_> = req.uri().path().trim_matches('/').split('/').collect();
    // Axum nested routers may expose either the nested or the full path.
    let Some(user_index) = segments.iter().position(|s| *s == "users") else {
        return next.run(req).await;
    };
    if segments.len() != user_index + 3 {
        return next.run(req).await;
    }
    let uid = super::routes::urldecode(segments[user_index + 1]);
    let endpoint = segments[user_index + 2].to_string();
    if !["cache-version", "subject-dims", "stats", "profile", "marks"].contains(&endpoint.as_str())
    {
        return next.run(req).await;
    }
    let params: HashMap<_, _> =
        url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let status = params
        .get("status")
        .map(String::as_str)
        .unwrap_or("collect");
    if !["collect", "do", "wish"].contains(&status) {
        return next.run(req).await;
    }
    let v = match versions(&state, &uid, status).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let category = params.get("category").map(String::as_str);
    let Some(version) = version_for(&v, &endpoint, category) else {
        return next.run(req).await;
    };
    // Paging against an obsolete index must retry using the new manifest. Check
    // both sides of the read so a publication during SQL assembly cannot mix pages.
    // New clients send both guards. Old servers still enforce cache_version;
    // new servers use only paging_version for marks, allowing live details to
    // change without invalidating an unchanged record sequence.
    let paging = endpoint == "marks" && params.contains_key("paging_version");
    let expected = params.get(if paging {
        "paging_version"
    } else {
        "cache_version"
    });
    let guarded_version = if paging {
        paging_version_for(&v, category)
    } else {
        Some(version)
    };
    if expected.is_some_and(|expected| Some(expected.as_str()) != guarded_version) {
        return changed_response();
    }
    if endpoint == "marks" {
        let response = next.run(req).await;
        if let Some(expected) = expected {
            let current = match versions(&state, &uid, status).await {
                Ok(current) => current,
                Err(e) => return e.into_response(),
            };
            let current_version = if paging {
                paging_version_for(&current, category)
            } else {
                version_for(&current, &endpoint, category)
            };
            if Some(expected.as_str()) != current_version {
                return changed_response();
            }
        }
        return response;
    }
    let key = if endpoint == "cache-version" {
        // pending 掺进 ETag：job 入队不 bump revision（触发器只盯内容），
        // 缺了它 pending 变化会被 304 吞掉，前端感知不到轮询该续/该停
        format!(
            "manifest-v2:{}:{}:{}:{}",
            req.uri(),
            v.all,
            v.profile,
            v.subjects_pending
        )
    } else {
        format!("{}:{version}", req.uri())
    };
    let tag = etag(&key);
    // Version endpoint always exists; projection 304s require a cached successful body.
    if endpoint == "cache-version" {
        let mut response = if matches(req.headers(), &tag) {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            Json(v).into_response()
        };
        cache_headers(&mut response, &tag);
        return response;
    }
    let slot = {
        let mut cache = state.projections.lock().await;
        if cache.entries.len() >= MAX_ENTRIES || cache.bytes >= MAX_BYTES {
            cache.entries.clear();
            cache.bytes = 0;
        }
        cache.entries.entry(key.clone()).or_default().clone()
    };
    let mut cached = slot.lock().await;
    if let Some(body) = cached.as_ref() {
        let mut response = if matches(req.headers(), &tag) {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            ([(header::CONTENT_TYPE, "application/json")], body.clone()).into_response()
        };
        cache_headers(&mut response, &tag);
        return response;
    }
    let mut response = next.run(req).await;
    let current = match versions(&state, &uid, status).await {
        Ok(current) => current,
        Err(e) => return e.into_response(),
    };
    if Some(version) != version_for(&current, &endpoint, category) {
        // Never retain a projection assembled across two published versions.
        state.projections.lock().await.entries.remove(&key);
        if expected.is_some() {
            return changed_response();
        }
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
        return response;
    }
    if !response.status().is_success() {
        state.projections.lock().await.entries.remove(&key);
        return response;
    }
    let (parts, body) = response.into_parts();
    // The API generates bounded-by-user JSON; large projections are served but not retained.
    let body = match to_bytes(body, usize::MAX).await {
        Ok(body) => body,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if body.len() <= MAX_ENTRY_BYTES {
        let mut cache = state.projections.lock().await;
        if cache.bytes + body.len() <= MAX_BYTES
            && cache
                .entries
                .get(&key)
                .is_some_and(|entry| Arc::ptr_eq(entry, &slot))
        {
            cache.bytes += body.len();
            *cached = Some(body.clone());
        }
    }
    let mut response = Response::from_parts(parts, Body::from(body));
    cache_headers(&mut response, &tag);
    response
}
