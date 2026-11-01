//! Authenticated, bounded HTTP transport. No worker, migration, or upstream fetch.
use anyhow::{ensure, Result};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rmcp::transport::streamable_http_server::{
    session::never::NeverSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::DoubanMcp;

pub const MAX_REQUEST_BYTES: usize = 16 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

pub struct HttpConfig {
    pub token: String,
    pub allowed_hosts: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub requests_per_minute: u32,
    pub burst: u32,
    pub max_concurrent_requests: usize,
}

struct Gate {
    token_hash: [u8; 32],
    slots: Semaphore,
    budget: Mutex<(Instant, f64)>,
    per_second: f64,
    burst: f64,
}

impl Gate {
    fn allow(&self) -> bool {
        let mut budget = self.budget.lock().unwrap();
        let now = Instant::now();
        budget.1 = (budget.1 + now.duration_since(budget.0).as_secs_f64() * self.per_second)
            .min(self.burst);
        budget.0 = now;
        if budget.1 < 1.0 {
            return false;
        }
        budget.1 -= 1.0;
        true
    }
}

pub fn router(
    server: DoubanMcp,
    config: HttpConfig,
    cancellation: CancellationToken,
) -> Result<Router> {
    ensure!(
        server.remote,
        "HTTP transport requires a restricted remote reader"
    );
    ensure!(
        config.token.len() >= 32 && config.token.bytes().all(|b| b.is_ascii_graphic()),
        "MCP token must contain at least 32 printable non-space ASCII characters"
    );
    ensure!(
        !config.allowed_hosts.is_empty()
            && config
                .allowed_hosts
                .iter()
                .all(|h| !h.is_empty() && !h.contains('*') && HeaderValue::from_str(h).is_ok()),
        "explicit allowed hosts are required"
    );
    ensure!(
        (1..=600).contains(&config.requests_per_minute),
        "requests_per_minute must be 1–600"
    );
    ensure!((1..=60).contains(&config.burst), "burst must be 1–60");
    ensure!(
        (1..=4).contains(&config.max_concurrent_requests),
        "max_concurrent_requests must be 1–4"
    );
    let gate = Arc::new(Gate {
        token_hash: Sha256::digest(config.token.as_bytes()).into(),
        slots: Semaphore::new(config.max_concurrent_requests),
        budget: Mutex::new((Instant::now(), f64::from(config.burst))),
        per_second: f64::from(config.requests_per_minute) / 60.0,
        burst: f64::from(config.burst),
    });
    let mut transport = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_allowed_hosts(config.allowed_hosts)
        .with_allowed_origins(config.allowed_origins)
        .enforce_origin_validation()
        .with_cancellation_token(cancellation);
    transport.max_request_body_bytes = MAX_REQUEST_BYTES;
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        transport,
    );
    // Both forms are served directly; no redirect may discard POST bodies or credentials.
    let protected = Router::new()
        .route_service("/mcp", service.clone())
        .route_service("/mcp/", service)
        .layer(middleware::from_fn_with_state(gate, guard));
    Ok(Router::new()
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .merge(protected))
}

fn failure(status: StatusCode, code: &'static str) -> Response {
    let mut response = (status, Json(json!({"error":code}))).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if status == StatusCode::UNAUTHORIZED {
        // A pre-shared pilot credential, not an OAuth authorization server.
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"weimo-douban-mcp\""),
        );
    }
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    }
    response
}

async fn guard(State(gate): State<Arc<Gate>>, request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let valid = token.is_some_and(|value| {
        let supplied: [u8; 32] = Sha256::digest(value.as_bytes()).into();
        bool::from(supplied.ct_eq(&gate.token_hash))
    });
    if !valid {
        return failure(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    }
    if !gate.allow() {
        return failure(StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED");
    }
    let Ok(_permit) = gate.slots.try_acquire() else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "BUSY");
    };
    let (parts, body) = request.into_parts();
    let body = match tokio::time::timeout(Duration::from_secs(5), to_bytes(body, MAX_REQUEST_BYTES))
        .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return failure(StatusCode::PAYLOAD_TOO_LARGE, "REQUEST_TOO_LARGE"),
        Err(_) => return failure(StatusCode::REQUEST_TIMEOUT, "REQUEST_TIMEOUT"),
    };
    // Current tools emit a finite terminal response, no subscriptions or progress streams.
    // Buffer with a hard cap so slow readers never retain a DB/query permit indefinitely.
    match tokio::time::timeout(REQUEST_TIMEOUT, async {
        let response = next.run(Request::from_parts(parts, Body::from(body))).await;
        let (mut parts, body) = response.into_parts();
        let body = match to_bytes(body, MAX_RESPONSE_BYTES).await {
            Ok(body) => body,
            Err(_) => return failure(StatusCode::BAD_GATEWAY, "RESPONSE_TOO_LARGE_OR_INCOMPLETE"),
        };
        parts
            .headers
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        Response::from_parts(parts, Body::from(body))
    })
    .await
    {
        Ok(response) => response,
        Err(_) => failure(StatusCode::GATEWAY_TIMEOUT, "QUERY_TIMEOUT"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    #[tokio::test]
    async fn http_concurrency_is_held_until_completion_or_cancellation() {
        let gate = Arc::new(Gate {
            token_hash: Sha256::digest(b"test").into(),
            slots: Semaphore::new(1),
            budget: Mutex::new((Instant::now(), 10.0)),
            per_second: 1.0,
            burst: 10.0,
        });
        let (started, mut received) = tokio::sync::mpsc::channel(1);
        let app = Router::new()
            .route(
                "/slow",
                get(move || {
                    let started = started.clone();
                    async move {
                        started.send(()).await.unwrap();
                        std::future::pending::<String>().await
                    }
                }),
            )
            .route("/fast", get(|| async { "done" }))
            .layer(middleware::from_fn_with_state(gate, guard));
        let request = |uri| {
            Request::builder()
                .uri(uri)
                .header("Authorization", "Bearer test")
                .body(Body::empty())
                .unwrap()
        };
        let task = tokio::spawn(app.clone().oneshot(request("/slow")));
        received.recv().await.unwrap();
        let response = app.clone().oneshot(request("/fast")).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        task.abort();
        let _ = task.await;
        assert_eq!(
            app.oneshot(request("/fast")).await.unwrap().status(),
            StatusCode::OK
        );
    }
}
