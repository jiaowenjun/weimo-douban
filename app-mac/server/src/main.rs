//! 微墨·豆瓣 mac 原生壳（app-mac/Sources/WeimoDoubanMac）的无头内嵌服务器。
//!
//! 与 app/（Tauri 版）同构的全栈装配：backend lib（`weimo_douban::server`，
//! API + 抓取/富集/订阅后台任务）+ rust-embed 前端 dist 挂 `/douban/`（vite
//! base=/douban/），回环随机端口单进程自足，前后端同源零 CORS 零鉴权。
//! 差异仅在壳：无 Tauri 运行时——就绪信号走 stdout 机器可读行
//! `WEIMO_READY http://127.0.0.1:<port>/douban/`，由 Swift 壳逐行解析；
//! 另带父进程看门狗（`WEIMO_PARENT_PID`），壳崩溃即自杀防孤儿。
//!
//! 数据目录与 Tauri 版共用（`~/Library/Application Support/ink.weimo.douban/`，
//! 壳经 `--data-dir` 传入，同库免重抓）。`SYNC_CRAWL_*`/`SYNC_SUBJECT_*` 等
//! 环境变量覆盖保留（与服务器部署同语义）；`bind`/`token`/`cors` 由本进程
//! 强制接管。

use anyhow::{Context, Result};
use clap::Parser as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;
use weimo_douban::server::config::ServerConfig;

mod static_files;

/// 前端静态资源挂载前缀（与生产站点 weimo.ink/douban/ 及 vite base 一致）
const WEB_MOUNT: &str = "/douban";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "weimo_douban=info".into()),
        )
        .init();

    let data_dir = data_dir_from_args();
    spawn_parent_watchdog();

    // 先绑定回环随机端口（backlog 生效，READY 行之后到达的连接不会拒绝）；
    // 服务装配（建目录/连 DB/spawn 后台任务）紧随其后
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("绑定回环端口失败")?;

    // 空参数 parse：忽略 Finder 传入的启动参数（clap 对未知参数会报错），
    // 但保留 SYNC_* 环境变量覆盖（从终端启动时可调参）
    let mut config =
        ServerConfig::try_parse_from(["weimo-douban-mac-server"]).context("解析服务器配置失败")?;
    config.bind = listener.local_addr()?.to_string();
    config.data_dir = data_dir;
    // 回环单用户 + 前后端同源：不启用鉴权与 CORS
    config.token = String::new();
    config.cors_origins = String::new();
    config.configure_desktop();

    let router = weimo_douban::server::build_server(config).await?;
    let router = router.nest_service(WEB_MOUNT, static_files::router());

    let addr = listener.local_addr()?;
    // 机器可读就绪行：Swift 壳按前缀解析出 base URL（Rust Stdout 行缓冲，
    // println 即 flush；backend 的 tracing 日志行混在同一流，按前缀区分）
    println!("WEIMO_READY http://{addr}{WEB_MOUNT}/");
    std::io::stdout().flush().ok();
    eprintln!("weimo-douban-mac-server listening on {addr}");

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// `--data-dir <path>`（Swift 壳传入共享数据目录；支持 `--data-dir=path` 两种
/// 写法），缺省回落后端默认（`data/`，仅供手工调试）
fn data_dir_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    for (index, arg) in args.iter().enumerate() {
        if let Some(value) = arg.strip_prefix("--data-dir=") {
            return PathBuf::from(value);
        }
        if arg == "--data-dir" {
            if let Some(value) = args.get(index + 1) {
                return PathBuf::from(value);
            }
        }
    }
    PathBuf::from("data")
}

/// 父进程看门狗：`WEIMO_PARENT_PID` 指向壳进程，kill(pid,0) 探活失败
/// （ESRCH，进程已不存在）即退出——壳崩溃/被杀时不留孤儿占着数据目录
fn spawn_parent_watchdog() {
    let Ok(pid) = std::env::var("WEIMO_PARENT_PID") else {
        return;
    };
    let Ok(pid) = pid.parse::<i32>() else {
        return;
    };
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(2));
        // 0 号信号纯探活：返回 0 存活；-1 且 errno==ESRCH 已死（EPERM 视为存活）
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            std::process::exit(0);
        }
    });
}
