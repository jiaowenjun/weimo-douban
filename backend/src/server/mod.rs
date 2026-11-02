//! 服务器模块——按《服务器API设计.md》《服务器技术选型.md》实现。

pub mod cache;
pub mod config;
pub mod covers;
pub mod db;
pub mod dims;
pub mod error;
pub mod fetcher;
pub mod reader;
pub mod routes;
pub mod service;
pub mod state;
pub mod stats;
pub mod subject;
pub mod subject_queue;
pub mod subject_cache;
pub mod subject_cache_client;
pub mod worker;

use anyhow::{Context, Result};
use clap::Parser as _;

/// 装配并运行服务器（供 bin/server.rs 调用；监听地址来自 clap/env）
pub async fn run_server() -> Result<()> {
    let config = config::ServerConfig::parse();
    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .with_context(|| format!("监听 {} 失败", config.bind))?;
    let app = build_server(config).await?;
    tracing::info!(
        "weimo-douban-server listening on {}",
        listener
            .local_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_default()
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// 装配后台任务与 API 路由（不绑定端口）。除 bin/server.rs 外，供嵌入方
/// （Tauri 桌面 app，见仓库 app/）在自有 listener 上组装运行——绑定策略
/// 与静态资源挂载由调用方决定
pub async fn build_server(config: config::ServerConfig) -> Result<axum::Router> {
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("创建数据目录失败: {}", config.data_dir.display()))?;

    let pool = db::connect(&config.db_path())
        .await
        .context("数据库初始化失败")?;
    subject_queue::recover_interrupted(&pool).await?;
    let state = state::AppState::new(config.clone(), pool);

    if let Err(error) = subject_cache_client::load_preferences(&state).await {
        tracing::warn!(%error, "读取中心缓存偏好失败，本次启动使用本地抓取");
    }
    if state.subject_cache_client.is_some() {
        tokio::spawn(subject_cache_client::run(state.clone()));
    }

    tokio::spawn(worker::run_worker(
        state.clone(),
        fetcher::ReqwestFetcher::with_cookie(&config.douban_cookie),
    ));
    tokio::spawn(service::run_subscription_loop(
        state.clone(),
        fetcher::ReqwestFetcher::with_cookie(&config.douban_cookie),
    ));
    tokio::spawn(service::run_cleanup_loop(state.clone()));

    if config.subject_enrich {
        tracing::info!(
            "条目元信息富集已启用（rexxar；存量数据可 POST /api/v1/subjects/backfill 回填）"
        );
    }

    Ok(routes::build_app(state))
}
