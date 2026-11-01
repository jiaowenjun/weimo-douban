# 微墨·豆瓣 mac 原生壳（WeimoDoubanMac）

`app-mac/` 是《[tauri app mac 原生渐进式迁移路线图](../tauri_app_mac_原生渐进式迁移路线图.md)》
的执行落地：**独立的 macOS 原生客户端**，与 `app/`（Tauri 版）并行存在、
零改动后者（也不动 `backend/`、`frontend/`）。对应路线图终局 4B「换壳不
换核」的第一步，v1 交付 P0 四件套 + 开机自启。

## 架构

```
WeimoDoubanNative.app（单进程 + sidecar）
├─ Swift 原生壳（AppKit，LSUIElement 托盘常驻，SPM 零第三方依赖）
│   ├─ NSStatusItem + 菜单（白墨滴模板图标=与 Tauri 版统一，isTemplate 自适配明暗；symbol 回退链兜底）
│   ├─ NSWindow + WKWebView ← 加载 http://127.0.0.1:<port>/douban/
│   └─ SyncMonitor：3s 轮询 localhost API → 状态行 + 系统通知
└─ weimo-douban-mac-server（Rust sidecar，链 ../backend lib）
    ├─ /api/v1/**（抓取/富集/订阅后台任务全在内）
    └─ rust-embed 前端 dist → /douban/（整栈原子快照）
    ├─ 就绪信号：stdout 机器可读行 `WEIMO_READY <url>`（壳逐行解析）
    └─ 父进程看门狗：壳崩溃即自杀，防孤儿占数据目录
```

与 Tauri 版的差别只在壳：窗口/托盘/通知/菜单全部原生（AppKit），
服务器装配与数据完全同构。

## 关键设计

- **数据目录共用**：`~/Library/Application Support/ink.weimo.douban/`
  （Tauri 版同库同 covers，免重抓）。SQLite 未开 WAL、busy_timeout 仅
  sqlx 默认 5s——**两版同时运行有写冲突风险**，启动时检测 Tauri 版在
  跑则弹警告，由用户定夺；本壳自身按 bundle id 单实例。
- **bundle id 独立**：`ink.weimo.douban.native`，与 Tauri 版可共存。
- **前端零改动**：WKWebView documentStart 注入 `window.isTauri = true`，
  复用前端现成的壳内分支（隐藏下载 CTA/GitHub 入口、CTA 变「开始备份」）。
- **窗口状态原生**：`frameAutosaveName` 记忆位置尺寸，首启居中（路线图 1.3）。
- **外链走系统浏览器**：导航策略仅放行 127.0.0.1，其余 NSWorkspace 默认
  浏览器（路线图 1.4）。
- **CSV 导出 → NSSavePanel**：blob 锚点经 `shouldPerformDownload` →
  WKDownload → 原生保存面板自选位置（路线图 2.1 原生形态）。
- **通知**（路线图 1.1 路线 i，零后端改动）：SyncMonitor 与前端灵动岛
  同端点同口径——任务 `queued/running → completed/partial/failed` 跃迁、
  `subjects_pending >0 → 0` 跃迁发系统通知，点击唤起主窗口；首帧只建
  基线不回放历史。dev 直跑（无 bundle id）通知降级为日志。
- **托盘菜单**：打开主窗口 / 状态行（同步中 P% · 详情拉取中 N 条 · 空闲）/
  立即同步（全部已登记用户 POST）/ 打开数据文件夹 / 开机自启（SMAppService）/
  GitHub 仓库 / 退出。

## 构建与运行

```bash
# 开发直跑（debug，免组装 bundle；通知/自启/单实例三件因无 bundle id 而降级）
./scripts/dev.sh

# 组装 .app（前端 dist → cargo release → swift release → bundle → ad-hoc 签名）
./scripts/build-app.sh
open build/WeimoDoubanNative.app
```

依赖：Xcode 命令行工具（swift 5.9+）、cargo、pnpm。前端 dist 是 server
crate 的编译期依赖（rust-embed，`#[folder]` 相对 `server/Cargo.toml`），
两个脚本都会先构建前端。

## 手工调试 sidecar

```bash
./server/target/debug/weimo-douban-mac-server --data-dir /tmp/weimo-test
# stdout: WEIMO_READY http://127.0.0.1:<port>/douban/
curl http://127.0.0.1:<port>/healthz        # {"status":"ok"}（根路径，不在 /api/v1 下）
```

`SYNC_CRAWL_*`/`SYNC_SUBJECT_*` 等环境变量透传（语义同服务器部署）；
`bind`/`token`/`cors` 由进程强制接管。

## v1 范围外（后续项，见路线图）

- 全局快捷键唤起（P1，Carbon RegisterEventHotKey）
- 自动更新：GitHub `latest.json` 属 Tauri 版版本线，与本壳版本不可比；
  分发与更新方案待定（Sparkle 或独立 manifest）后再做
- 深链接、标题栏一体化（P2/P3，见路线图）

## 已知待实测

- CSV 导出（blob 锚点 → WKDownload → NSSavePanel）在真机上的完整链路；
  不通则记为缺口，回落方案见路线图 2.1（壳改写 download destination 或
  页面走桥接）。
- ad-hoc 签名下系统通知权限登记行为（首次发通知时请求授权）。
