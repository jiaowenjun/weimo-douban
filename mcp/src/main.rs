use anyhow::{Context, Result};
use clap::Parser;
use rmcp::ServiceExt;
use serde_json::json;
use std::path::PathBuf;
use weimo_douban::server::reader::Reader;
use weimo_douban_mcp::DoubanMcp;

#[derive(Parser)]
#[command(
    version,
    about = "微墨·豆瓣本地只读 MCP（stdio）；App 退出后仍可查询已有数据"
)]
struct Cli {
    /// App 数据目录；默认按操作系统解析 ink.weimo.douban 应用数据目录
    #[arg(long, env = "SYNC_SERVER_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// 输出 mcpServers 配置并退出，不打开数据库或修改 Agent 设置
    #[arg(long)]
    print_config: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = cli
        .data_dir
        .or_else(|| dirs::data_dir().map(|p| p.join("ink.weimo.douban")))
        .context("无法确定应用数据目录；请显式指定 --data-dir")?;
    let data_dir = if data_dir.is_absolute() {
        data_dir
    } else {
        std::env::current_dir()?.join(data_dir)
    };
    if cli.print_config {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"mcpServers":{"weimo-douban":{
                "command":std::env::current_exe()?, "args":["--data-dir",data_dir]
            }}}))?
        );
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let reader = Reader::open(&data_dir.join("douban_export.db")).await?;
    let service = DoubanMcp::local(reader)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}
