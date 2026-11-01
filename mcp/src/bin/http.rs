use anyhow::{ensure, Context, Result};
use clap::Parser;
use std::{net::SocketAddr, path::PathBuf};
use tokio_util::sync::CancellationToken;
use weimo_douban::server::reader::Reader;
use weimo_douban_mcp::{
    http::{router, HttpConfig},
    DoubanMcp,
};

#[derive(Parser)]
#[command(version, about = "微墨·豆瓣远程只读 MCP；须经 HTTPS 反代访问")]
struct Cli {
    #[arg(long, env = "MCP_BIND", default_value = "127.0.0.1:8021")]
    bind: SocketAddr,
    #[arg(long, env = "SYNC_SERVER_DATA_DIR")]
    data_dir: PathBuf,
    /// File containing a dedicated MCP credential, never the public web API token.
    #[arg(long, env = "MCP_TOKEN_FILE")]
    token_file: PathBuf,
    #[arg(long, env = "MCP_ALLOWED_UIDS", value_delimiter = ',', required = true)]
    allowed_uids: Vec<String>,
    #[arg(
        long,
        env = "MCP_ALLOWED_HOSTS",
        value_delimiter = ',',
        required = true
    )]
    allowed_hosts: Vec<String>,
    /// Empty means reject every request carrying Origin; non-browser clients are accepted.
    #[arg(long, env = "MCP_ALLOWED_ORIGINS", value_delimiter = ',')]
    allowed_origins: Vec<String>,
    #[arg(long, env = "MCP_REQUESTS_PER_MINUTE", default_value_t = 60)]
    requests_per_minute: u32,
    #[arg(long, env = "MCP_BURST", default_value_t = 10)]
    burst: u32,
    #[arg(long, env = "MCP_MAX_CONCURRENT", default_value_t = 2)]
    max_concurrent_requests: usize,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    ensure!(
        cli.bind.ip().is_loopback(),
        "MCP must bind to loopback behind an HTTPS reverse proxy"
    );
    let token = std::fs::read_to_string(&cli.token_file).context("读取 MCP 凭证文件失败")?;
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter("warn")
        .init();
    let server = DoubanMcp::remote(
        Reader::open(&cli.data_dir.join("douban_export.db")).await?,
        cli.allowed_uids,
    )?;
    let cancellation = CancellationToken::new();
    let app = router(
        server,
        HttpConfig {
            token: token.trim().to_owned(),
            allowed_hosts: cli.allowed_hosts,
            allowed_origins: cli.allowed_origins,
            requests_per_minute: cli.requests_per_minute,
            burst: cli.burst,
            max_concurrent_requests: cli.max_concurrent_requests,
        },
        cancellation.child_token(),
    )?;
    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    eprintln!(
        "weimo-douban-mcp-http listening on {}",
        listener.local_addr()?
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            #[cfg(unix)]
            {
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("install SIGTERM handler");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
            cancellation.cancel();
        })
        .await?;
    Ok(())
}
