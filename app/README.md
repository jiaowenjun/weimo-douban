# 微墨·豆瓣桌面 App（Tauri，macOS / Windows）

安装包同时附带 `weimo-douban-mcp`，由本地 Agent 按需启动，通过 stdio 只读查询应用数据库。它不随 GUI 启动、不运行同步任务；连接配置与工具说明见 [本地 MCP 使用指南](../mcp/local-guide.md)。构建钩子会先编译 MCP 再构建前端，`bundle.externalBin` 将对应平台的可执行文件加入安装包。

方案 B 全栈封装：**Tauri 壳 + 内嵌同步服务器，单进程自足运行**。不依赖
weimo.ink 也可工作。

## 架构

```
微墨·豆瓣.app（单进程）
├─ Tauri 2（WKWebView 窗口壳，无自定义协议、无 IPC）
├─ weimo_douban::server::build_server(config)   ← backend 以 lib 依赖内嵌
│   ├─ axum API（/api/v1/**，含抓取/富集/订阅后台任务）
│   └─ rust-embed 前端 dist → 挂载在 /douban/   ← 编译期进二进制
└─ 窗口加载 http://127.0.0.1:<随机回环端口>/douban/
```

关键设计：

- **前后端同源**：本地 axum 同时服务 API 与静态资源，窗口直接开 localhost
  URL——零 CORS（无需动后端 `cors_layer`）、零 Tauri IPC、零跨进程。
- **前端零改动**：App 构建即 `pnpm build`（不注入任何环境变量）——
  `vite.config.ts` 生产 `base=/douban/` 恰好匹配挂载前缀，API 基址回落
  同源 `/api/v1`，`resolveApiUrl` 原样透传。与线上 weimo.ink/douban/ 的
  URL 结构完全一致。
- **版本原子性**：dist 经 rust-embed 编进二进制，App = 整栈快照，
  `cache_version` 协议前后端永远同版（与 txy 原子符号链接部署同构）。
- **回环随机端口**：setup 里先 `TcpListener::bind("127.0.0.1:0")` 把端口
  绑定到手（backlog 生效，后续连接不会拒绝）；仅回环监听、token 留空。
- **单实例**（tauri-plugin-single-instance）：二次启动聚焦既有窗口，
  避免两个进程写同一 SQLite。

## 桌面形态两件套（v0.4.0）

### 1. 菜单栏常驻（LSUIElement）

`Info.plist` 置 `LSUIElement=true`（accessory app，无 Dock 图标）+ 状态
区托盘：**左键托盘开/聚焦主窗口，右键菜单（打开主窗口 / 检查更新… /
退出）**。关窗 = 隐藏不退出——订阅 RSS 轮询、富集流水线、缓存清理等
后台任务持续在跑（这正是「数据新鲜度 = App 存活期」的缓解手段）。
Windows 无 LSUIElement 概念（该 plist 项被忽略）：App 正常显示任务栏
图标 + 系统托盘，托盘交互/关窗隐藏行为与 macOS 一致（tauri `tray-icon`
跨平台，`main.rs` 无平台分支）。

### 2. 自动更新（tauri-plugin-updater）

- manifest 端点双通道（`UPDATER_ENDPOINTS` 按序超时回退）：**Gitee 国内
  源优先**（主要用户群在大陆，GitHub 直连常不可达）——数据源 = 仓库
  `app-latest` 分支的 latest.json（发布脚本把下载直链改写成 Gitee 发行版
  后以孤儿提交推送，raw 直链稳定无版本号，弥补 Gitee 无 latest/download
  别名的缺口；raw 缓存仅 60s）；GitHub Releases latest/download 重定向
  作海外/故障回退（仓库公开免鉴权）；
- 启动 5s 后静默自检，发现新版本弹窗确认；托盘菜单可手动检查；
- minisign 签名校验（`updater.pubkey` 编入二进制；私钥在
  `~/.tauri-keys/weimo-douban-updater.key`，**丢失即无法再推送更新，
  务必备份**）；
- 安装后重启（macOS 上整包替换 .app；Windows 上装 NSIS 更新包整包替换）。

发布流程（**发版必走「完整部署」：网页端同步上 txy，见 ops/
《txy部署手册.md》§7.5**——欢迎页页脚的版本号+发布日期须与最新
Release 一致）：版本 bump 提交（四处改点：`tauri.conf.json` 与本目录
`Cargo.toml` 的 version、web 欢迎页 `APP_VERSION` 与 `APP_RELEASE_DATE`）
并 push → 手动 dispatch `app-windows` 工作流（出 Windows 产物，mac 无法
交叉编译）→ 本地 `./scripts/release.sh`（构建签名产物 → 拉取 Windows CI
产物 → 生成 latest.json → `gh release` 上传双平台 dmg/exe/tar.gz/sig/
latest.json，找不到匹配版本的 Windows 产物时自动回落仅发 macOS；末步
自动同步 **Gitee 国内镜像**——双推 main+tags 到 `gitee` remote + 附件
传 Gitee 发行版 + latest.json 改写下载直链后推 `app-latest` 孤儿分支
（App updater 的 Gitee 端点数据源，欢迎页下载直链同指 Gitee），大陆
免梯下载/更新。令牌= `~/.gitee/token` 或 `GITEE_TOKEN`
（[私人令牌](https://gitee.com/profile/personal_access_tokens)勾 projects；
缺令牌/缺 remote 自动跳过，不影响 GitHub 发布））→ `gh release edit`
换详细日志后补跑 `./scripts/release.sh --gitee-only v<版本>` 刷新 Gitee
版面与清单分支（幂等：已在附件跳过、标题描述覆盖更新）→
网页端按手册 §7 部署 txy 并过 §7.5.3 一致性断言。

## Windows 版（CI 构建，mac 无法交叉编译）

同一份代码直接编 Windows：依赖面干净（reqwest 走 rustls 无 OpenSSL、
sqlx sqlite bundled、托盘用 tauri 跨平台 `tray-icon`，`main.rs` 无
平台分支），产物为 NSIS 安装包 `weimo-douban_<ver>_x64-setup.exe`
（WebView2 缺失时安装包自动引导，Win10/11 一般自带）。

构建走 GitHub Actions（`.github/workflows/app-windows.yml`，
`windows-latest`，手动 dispatch）：

1. **一次性配置 Secrets**（Settings → Secrets and variables → Actions）：
   `TAURI_SIGNING_PRIVATE_KEY` = 私钥内容
   （`cat ~/.tauri-keys/weimo-douban-updater.key`）；
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` = 私钥口令（本仓密钥为空可不配）。
2. bump 版本 push 到 main → Actions 手动运行 app-windows；
3. 本地 `./scripts/release.sh` 拉取该 run 的产物（附 version.txt 校验
   版本一致才并入），与 mac 产物合并进同一 Release，latest.json 补
   `windows-x86_64` 条目（updater 直接指向 setup.exe 本体——Tauri v2
   对 exe 做 minisign 签名，无 v1 时代的 `.nsis.zip` 包装）。

CI 产物资产名全 ASCII（`weimo-douban` 前缀，与 mac 同约定——gh 上传
会剥掉 CJK 资产名前缀导致 updater URL 落空）。分发给他人的未签名 exe
首次运行有 SmartScreen 提示（「仍要运行」即可；要消除需代码签名证书，
暂不做）。

## 环境变量（从终端启动二进制时生效；Finder 启动为默认值）

| 变量 | 默认 | 说明 |
|---|---|---|
| `SYNC_UPDATER_URL` | Gitee 优先+GitHub 回退 | 更新 manifest 端点覆盖（设为单端点；release 构建仅接受 https；本地联调需临时开 `dangerousInsecureTransportProtocol` 重新构建） |
| `SYNC_SUBJECT_CACHE_URL` | 微墨中心 batch 接口 | 可选详情加速；空字符串禁用，故障自动回退本地豆瓣抓取。设置中的开关持久化在本地 SQLite |
| `SYNC_UPDATE_AUTO` | 空 | `1` = 发现新版本不经确认直接装（无人值守） |
| `SYNC_CRAWL_*` / `SYNC_SUBJECT_*` / `SYNC_DOUBAN_COOKIE` 等 | 后端默认 | 抓取/富集调参，语义与服务器部署一致（见 [ServerConfig](../backend/src/server/config.rs)） |

`bind`/`data_dir`/`token`/`cors_origins` 由 App 强制接管（回环、应用
数据目录、免鉴权），环境变量不生效。

## 数据目录

`~/Library/Application Support/ink.weimo.douban/`（`douban_export.db` +
`covers/`）；Windows 为 `%APPDATA%\ink.weimo.douban\`。删除该目录即回到
全新首启状态。

手工迁移数据（免全量重抓）：

```bash
sqlite3 <源>/douban_export.db ".backup ~/Library/Application Support/ink.weimo.douban/douban_export.db"
cp -R <源>/covers ~/Library/Application Support/ink.weimo.douban/
```

## 构建与运行

```bash
# 一次性
pnpm --dir app install
# 开发（构建前端 dist + cargo run 起 App）
pnpm --dir app tauri dev
# 产出 .app / .dmg / 更新签名产物（发布用 scripts/release.sh）
pnpm --dir app tauri build
```

- 前端 dist 是编译期依赖（rust-embed，`#[folder]` 相对 Cargo.toml）：
  改前端后需重新 `tauri build`/`tauri dev`；日常前端开发仍走
  frontend/web 的 vite dev 流程。
- `cargo build` 只更新裸二进制，**不会更新 .app bundle 里的副本**——
  本地替换需手动 cp 进 `Contents/MacOS/` 或直接 `tauri build`。
- 窗口经 `run_on_main_thread` 在主线程创建（macOS 上后台线程建窗会丢
  尺寸/定位）；CGWindowList 无录屏权限时 bounds 会被涂改成 500×500，
  判断窗口状态请以进程内 tao 查询（启动日志「主窗口已创建」）为准。
- 本机自用 ad-hoc 签名即可直接运行；分发给他人的 dmg 需 Developer ID
  签名 + 公证（Apple 开发者账号）。

## 工程结构

```
app/
├── Cargo.toml          # 依赖 ../backend（weimo_douban lib）+ tauri 2（tray-icon）
├── package.json        # @tauri-apps/cli（本地 devDep，避免 dlx cwd 漂移）
├── tauri.conf.json     # 窗口在 main.rs 动态创建；updater pubkey 在 plugins 段
├── Info.plist          # 并入 bundle：LSUIElement（菜单栏常驻形态）
├── updater.pubkey      # 更新签名公钥（私钥在 ~/.tauri-keys/，勿入库）
├── icons/              # 由 frontend/web/public/favicon.svg 栅格化生成
├── scripts/release.sh  # 构建→签名→latest.json→gh release 一条龙
└── src/
    ├── main.rs         # 端口绑定/服务拉起/窗口/托盘/更新/单实例
    └── static_files.rs # rust-embed dist + HashRouter 兜底 index.html
```

后端侧配套（0.3.1）：`server::build_server`（装配与绑定分离）。
