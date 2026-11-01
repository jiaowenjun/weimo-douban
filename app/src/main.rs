//! 微墨·豆瓣书影音 macOS 桌面 App——Tauri 壳 + 内嵌 weimo-douban 同步服务器。
//!
//! 架构（全栈单进程）：启动时在回环随机端口拉起 `weimo_douban::server`
//! （API + 抓取/富集/订阅后台任务），前端静态资源（frontend/web 构建产物，
//! vite base=/douban/）经 rust-embed 编进二进制、由同一个 axum 在 /douban/
//! 前缀下服务（与线上 weimo.ink/douban/ 的 URL 结构一致，前端零改动），
//! 窗口直接加载 http://127.0.0.1:<port>/douban/ ——前后端同源，零 CORS、
//! 零跨进程。数据落在 ~/Library/Application Support/ink.weimo.douban/。
//!
//! 桌面形态两件套：
//! - 菜单栏常驻：LSUIElement（无 Dock 图标）+ 托盘菜单，关窗=隐藏，
//!   进程与订阅轮询/富集后台任务持续推进；
//! - 自动更新：tauri-plugin-updater，manifest 走 Gitee 国内源（GitHub
//!   Releases 回退，SYNC_UPDATER_URL 可覆盖），启动静默自检 +
//!   托盘手动检查，minisign 签名校验后安装重启。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod static_files;

use anyhow::{Context, Result};
use clap::Parser as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::menu::{Menu, MenuBuilder, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::DownloadEvent;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;
use weimo_douban::server::config::ServerConfig;

/// 前端静态资源挂载前缀（与生产站点 weimo.ink/douban/ 及 vite base 一致）
const WEB_MOUNT: &str = "/douban";

/// 更新 manifest 默认端点（按序回退）：Gitee 国内源优先——主要用户群在
/// 大陆，GitHub 直连常不可达；数据源 = 仓库 app-latest 分支的 latest.json
/// （发布脚本把下载直链改写成 Gitee 发行版后以孤儿提交推送到该分支，raw
/// 直链稳定无版本号，弥补 Gitee 没有 latest/download 别名的缺口）。
/// GitHub Releases latest-download 重定向作海外/故障回退（仓库公开，免
/// 鉴权）。SYNC_UPDATER_URL 可覆盖为单端点（自托管 / 本地联调）
const UPDATER_ENDPOINTS: [&str; 2] = [
    "https://gitee.com/jiaowenjun/weimo-douban/raw/app-latest/latest.json",
    "https://github.com/jiaowenjun/weimo-douban/releases/latest/download/latest.json",
];

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "weimo_douban=info,weimo_douban_app=info".into()),
        )
        .init();
    tracing::info!("weimo-douban app v{} 启动", env!("CARGO_PKG_VERSION"));

    tauri::Builder::default()
        // 单实例：二次启动聚焦既有窗口——两个进程写同一 SQLite 必须避免
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_updater::Builder::new()
                .pubkey(include_str!("../updater.pubkey").to_string())
                .build(),
        )
        .menu(|handle| Ok(Menu::default(handle)?))
        // 菜单栏常驻：关窗=隐藏不退出，订阅轮询/富集后台任务继续推进
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            build_tray(app.handle())?;

            // 先把回环随机端口绑定到手（backlog 生效，后续连接不会拒绝），
            // 服务装配在后台任务里做，完成后才创建窗口
            let listener = tauri::async_runtime::block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .context("绑定回环端口失败")?;
                anyhow::Ok(listener)
            })?;

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = run_server(handle.clone(), listener).await {
                    eprintln!("本地服务启动失败: {error:#}");
                    handle
                        .dialog()
                        .message(format!("本地服务启动失败：{error:#}"))
                        .kind(MessageDialogKind::Error)
                        .show(move |_| handle.exit(1));
                }
            });

            // 启动后静默自检更新（手动检查走托盘菜单）
            let updater_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                check_update(updater_handle, false).await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("运行 weimo-douban app 失败");
}

/// 装配并运行内嵌服务器：后台任务 + API 路由 + /douban/ 静态资源 →
/// 创建主窗口（服务就绪前开窗只会砸在一个空端口上）
async fn run_server(handle: tauri::AppHandle, listener: tokio::net::TcpListener) -> Result<()> {
    let data_dir = handle
        .path()
        .app_data_dir()
        .context("获取应用数据目录失败")?;

    // 空参数 parse：忽略 Finder 传入的启动参数（clap 对未知参数会报错），
    // 但保留 SYNC_CRAWL_*/SYNC_SUBJECT_* 等环境变量覆盖（从终端启动时可调参）
    let mut config =
        ServerConfig::try_parse_from(["weimo-douban-app"]).context("解析服务器配置失败")?;
    config.bind = listener.local_addr()?.to_string();
    config.data_dir = data_dir;
    // 回环单用户 + 前后端同源：不启用鉴权与 CORS
    config.token = String::new();
    config.cors_origins = String::new();
    config.configure_desktop();

    let router = weimo_douban::server::build_server(config).await?;
    let router = router.nest_service(WEB_MOUNT, static_files::router());

    let addr = listener.local_addr()?;
    tracing::info!("app 服务就绪: http://{addr}{WEB_MOUNT}/");
    // 窗口创建派发到主线程：macOS 上从后台线程建窗会丢失尺寸/定位
    // （实测落成 500×500 异位窗），tao 的窗口操作必须在主线程
    let window_handle = handle.clone();
    let port = addr.port();
    handle
        .run_on_main_thread(move || create_main_window(&window_handle, port))
        .ok();

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn create_main_window(handle: &tauri::AppHandle, port: u16) {
    let url: tauri::Url = format!("http://127.0.0.1:{port}{WEB_MOUNT}/")
        .parse()
        .expect("窗口地址构造失败");

    // 下载生命周期钩子（标记 CSV 导出走 blob 锚点点击）：不挂时 wry 对
    // macOS 的下载导航一律 Cancel——App 内点导出实际静默无效；挂上即放行
    // （默认存系统下载文件夹，重名自动加 (n) 后缀），完成/失败弹窗告知。
    // macOS 的 Finished 事件 wry 恒不回传最终路径，Requested 阶段先按 url
    // 记下目的地、Finished 时配对取回（Windows 的 Finished 自带路径优先）
    let notify_handle = handle.clone();
    let decided: Arc<Mutex<Option<(String, PathBuf)>>> = Arc::new(Mutex::new(None));
    let decided_on_request = decided.clone();
    if let Err(error) = WebviewWindowBuilder::new(handle, "main", WebviewUrl::External(url))
        .title("微墨·豆瓣")
        .inner_size(1280.0, 860.0)
        .min_inner_size(960.0, 600.0)
        .focused(true)
        .on_download(move |_webview, event| match event {
            DownloadEvent::Requested { url, destination } => {
                tracing::info!(%url, path = ?destination, "下载开始");
                *decided_on_request.lock().expect("下载状态锁中毒") =
                    Some((url.to_string(), destination.clone()));
                true
            }
            DownloadEvent::Finished { url, path, success } => {
                let fallback = decided
                    .lock()
                    .expect("下载状态锁中毒")
                    .take()
                    .filter(|(requested_url, _)| *requested_url == url.as_str())
                    .map(|(_, path)| path);
                notify_download_done(&notify_handle, success, path.or(fallback).as_deref());
                true
            }
            _ => true,
        })
        .build()
    {
        // 窗口失败不致命（托盘仍可重试打开），但基本不可能发生
        tracing::error!("创建主窗口失败: {error:#}");
    } else if let Some(window) = handle.get_webview_window("main") {
        let _ = window.center();
        let _ = window.set_focus();
        tracing::info!(
            "主窗口已创建: outer={:?} pos={:?}",
            window.outer_size(),
            window.outer_position()
        );
    }
}

// --------------------------------------------------------------------------- //
// 下载完成提示（CSV 导出等 webview 内下载）
// --------------------------------------------------------------------------- //

/// 下载结束弹窗：成功告知保存位置，副按钮一键在文件管理器中定位选中；
/// 失败仅报错。均非阻塞（on_download 回调在 macOS 跑主线程，不能
/// blocking 等用户点按——与 dialog_ask 的后台线程用法区分）
fn notify_download_done(handle: &tauri::AppHandle, success: bool, path: Option<&Path>) {
    if !success {
        tracing::warn!("下载失败");
        dialog_error(handle, "下载失败，请重试");
        return;
    }
    let Some(path) = path else {
        dialog_info(handle, "下载完成，已保存到系统下载文件夹");
        return;
    };
    tracing::info!("下载完成: {}", path.display());
    let reveal_label = if cfg!(target_os = "macos") {
        "在访达中显示"
    } else {
        "在资源管理器中显示"
    };
    let revealed_path = path.to_path_buf();
    handle
        .dialog()
        .message(format!("已保存到：\n{}", path.display()))
        .title("下载完成")
        // ok 位（右/主按钮）= 定位文件，取消位 = 关闭；路径含中文与空格，
        // display() 原样呈现（Explorer/访达均接受未加引号的展示文本）
        .buttons(MessageDialogButtons::OkCancelCustom(
            reveal_label.into(),
            "完成".into(),
        ))
        .show(move |reveal| {
            if reveal {
                reveal_in_file_manager(&revealed_path);
            }
        });
}

/// 在系统文件管理器中定位并选中文件（只选中不打开）：macOS 用 open -R，
/// Windows 用 explorer /select,（opener crate 同款做法），其余平台忽略
fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg("-R").arg(path);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg(format!("/select,{}", path.display()));
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("true");
    if let Err(error) = command.status() {
        tracing::warn!("打开文件位置失败: {error}");
    }
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

// --------------------------------------------------------------------------- //
// 托盘（菜单栏常驻）
// --------------------------------------------------------------------------- //

fn build_tray(handle: &tauri::AppHandle) -> Result<()> {
    let open = MenuItem::with_id(handle, "open", "打开主窗口", true, None::<&str>)?;
    let update = MenuItem::with_id(handle, "update", "检查更新…", true, None::<&str>)?;
    let quit = MenuItem::with_id(handle, "quit", "退出", true, None::<&str>)?;
    let menu = MenuBuilder::new(handle)
        .item(&open)
        .item(&update)
        .separator()
        .item(&quit)
        .build()?;

    // 托盘图标：菜单栏 NSStatusItem 不像 Dock 有系统圆角蒙版，满出血方形
    // （default_window_icon = icons/icon.png）直角原样露出，故改用自带 22.5%
    // 透明圆角的独立资产（icons/tray-icon.png，编译期内嵌），与 App 图标观感一致
    TrayIconBuilder::with_id("main-tray")
        .icon(tauri::include_image!("icons/tray-icon.png"))
        .tooltip("微墨·豆瓣")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "update" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    check_update(app, true).await;
                });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        // 左键点托盘 = 开/聚焦主窗口（菜单只走右键）
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(handle)?;
    Ok(())
}

// --------------------------------------------------------------------------- //
// 自动更新
// --------------------------------------------------------------------------- //

/// 构建更新器：默认端点 UPDATER_ENDPOINTS 按序回退（Gitee 优先、GitHub
/// 兜底——单端点超时或请求失败自动试下一个），SYNC_UPDATER_URL 可覆盖为
/// 单端点（自托管/本地联调；注意 release 构建仅接受 https，http 需在
/// tauri.conf 开 dangerousInsecureTransportProtocol）
fn build_updater(handle: &tauri::AppHandle) -> Result<tauri_plugin_updater::Updater> {
    let mut builder = handle.updater_builder();
    let override_url = env_or("SYNC_UPDATER_URL", "");
    let endpoints: Vec<tauri::Url> = if override_url.is_empty() {
        UPDATER_ENDPOINTS
            .iter()
            .map(|url| url.parse::<tauri::Url>())
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![override_url.parse::<tauri::Url>()?]
    };
    builder = builder.endpoints(endpoints)?;
    Ok(builder.build()?)
}

/// 检查并（经确认）安装更新。manual=true 为托盘手动触发（无更新/失败均弹
/// 窗告知）；启动自检静默失败。SYNC_UPDATE_AUTO=1 跳过确认直接装（无人值守）
async fn check_update(handle: tauri::AppHandle, manual: bool) {
    let updater = match build_updater(&handle) {
        Ok(updater) => updater,
        Err(error) => {
            tracing::warn!("更新器初始化失败: {error:#}");
            if manual {
                dialog_error(&handle, &format!("更新器初始化失败：{error:#}"));
            }
            return;
        }
    };
    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => {
            if manual {
                dialog_info(&handle, "已是最新版本");
            }
            return;
        }
        Err(error) => {
            tracing::warn!("检查更新失败: {error}");
            if manual {
                dialog_error(&handle, &format!("检查更新失败：{error}"));
            }
            return;
        }
    };
    let version = update.version.clone();
    tracing::info!("发现新版本 v{version}");
    if env_or("SYNC_UPDATE_AUTO", "") != "1" {
        let go = dialog_ask(
            &handle,
            "发现新版本",
            &format!("是否更新到 v{version}？\n\n下载完成后 App 将自动重启。"),
        )
        .await;
        if !go {
            return;
        }
    }
    if let Err(error) = update
        .download_and_install(
            |_, _| {},
            || {
                tracing::info!("更新包下载完成，开始安装");
            },
        )
        .await
    {
        tracing::error!("更新安装失败: {error}");
        dialog_error(&handle, &format!("更新安装失败：{error}"));
        return;
    }
    tracing::info!("已更新到 v{version}，重启生效");
    if env_or("SYNC_UPDATE_AUTO", "") == "1" {
        // 无人值守：不给确认对话框留时间
        tokio::time::sleep(Duration::from_millis(300)).await;
    } else {
        // 阻塞展示结果，用户知晓后再重启（show 是即发即忘，restart 会立刻杀掉它）
        let _ = handle
            .dialog()
            .message(format!("已更新到 v{version}，即将重启"))
            .blocking_show();
    }
    handle.restart();
}

// --------------------------------------------------------------------------- //
// 原生对话框薄封装（blocking 版：均从后台任务线程调用，切勿在主线程用）
// --------------------------------------------------------------------------- //

async fn dialog_ask(handle: &tauri::AppHandle, title: &str, message: &str) -> bool {
    let (send, receive) = tokio::sync::oneshot::channel();
    handle
        .dialog()
        .message(message)
        .title(title)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "确定".into(),
            "取消".into(),
        ))
        .show(move |ok| {
            let _ = send.send(ok);
        });
    receive.await.unwrap_or(false)
}

fn dialog_info(handle: &tauri::AppHandle, message: &str) {
    handle.dialog().message(message).show(|_| {});
}

fn dialog_error(handle: &tauri::AppHandle, message: &str) {
    handle
        .dialog()
        .message(message)
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}
