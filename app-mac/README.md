# 微墨·豆瓣 mac 原生壳（WeimoDoubanMac）

构建脚本还会把独立的 `weimo-douban-mcp` 放入 `Contents/Resources`，供本地 Agent 用 stdio 只读查询共享数据库。运行该程序的 `--print-config` 可生成实际路径配置，详见 [MCP 接入](../mcp/README.md)。

`app-mac/` 是《[tauri app mac 原生渐进式迁移路线图](../tauri_app_mac_原生渐进式迁移路线图.md)》
的执行落地：**独立的 macOS 原生客户端**，与 `app/`（Tauri 版）并行存在、
零改动后者（也不动 `backend/`、`frontend/`）。对应路线图终局 4B「换壳不
换核」的第一步，v1 交付 P0 四件套 + 开机自启。

## 架构

```
WeimoDoubanNative.app（单进程 + sidecar）
├─ Swift 原生壳（AppKit，混合形态：窗口在=Dock 常驻，收起=纯托盘，SPM 零第三方依赖）
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

- **混合桌面形态**：窗口可见期间 `.regular`（Dock 图标 + Cmd+Tab + 系统
  菜单栏，最小主菜单：关于/隐藏/退出），收起（关窗/热键 toggle）回
  `.accessory`（纯托盘，Dock 图标随消）——比 Tauri 版恒 LSUIElement 更
  进一步：菜单栏常驻哲学不变，窗口使用期 Dock 可达；Dock 点击重开窗口
  （applicationShouldHandleReopen）。
- **数据目录共用**：`~/Library/Application Support/ink.weimo.douban/`
  （Tauri 版同库同 covers，免重抓）。SQLite 使用 WAL，但两版各有抓取
  worker，**两版同时运行仍有写冲突风险**，启动时检测 Tauri 版在
  跑则弹警告，由用户定夺；本壳自身按 bundle id 单实例。
- **bundle id 独立**：`ink.weimo.douban.native`，与 Tauri 版可共存。
- **前端零改动**：WKWebView documentStart 注入 `window.isTauri = true`，
  复用前端现成的壳内分支（隐藏下载 CTA/GitHub 入口、CTA 变「开始备份」）。
- **窗口状态原生**：`frameAutosaveName` 记忆位置尺寸，首启居中（路线图 1.3）。
- **外链走系统浏览器**：导航策略仅放行 127.0.0.1，其余 NSWorkspace 默认
  浏览器（路线图 1.4）。
- **CSV 导出 → NSSavePanel**：blob 锚点经 `shouldPerformDownload` →
  WKDownload → 原生保存面板自选位置（路线图 2.1 原生形态）。
- **原生右键菜单**（路线图 2.2）：右键标记卡片 → 复制标题 / 复制豆瓣
  链接 / 在豆瓣打开。实现=壳 documentStart 注入 contextmenu 桥脚本
  （按 `li[data-subject-url]` 属性选择器提取，与 weimo-ui 类名解耦）→
  `WKScriptMessageHandler`（弱代理切断保留环）→ NSMenu 原生弹出；
  无卡片上下文不拦截保留 WebKit 默认。**这是唯一动了前端的一处**：
  MarkCard 根节点挂两个惰性 data 属性（`data-subject-url`/`data-title`，
  web 端零行为差异）——2.2 在路线图本就属「需页面配合」的过墙项。
- **同步→通知端到端已真实验证**：临时数据目录 + wenjun30 music collect
  28 条实测——任务 15s 完成、`[notify] 投递 同步完成 | 共 28 条` 命中、
  subjects_pending=24 进入「详情拉取中」、marks API 返回真实 subject_url。
- **标题栏一体化**（路线图 3.1，纯壳侧零前端改动）：fullSizeContentView +
  透明系统标题栏 + 隐藏标题——web 顶栏顶到窗口顶，红绿灯悬浮于顶栏左侧
  空白带。**零 CSS 让位的几何依据**：窗口最小 960px 恒 ≥700px 布局，
  左槽内容被模板 margin 推至 ~329px；欢迎页左槽壳内为空——探针实测
  marks 页 leftInteractive=0×0、欢迎页 null，红绿灯区（0..78 × 0..28）
  无任何有面积的交互元素。拖拽=壳桥（顶栏条带内 mousedown 非交互元素 →
  mouseDown 存根 performDrag；磨砂条下滚过的卡片经 closest(交互) 放行）、
  双击空白=窗口 zoom（macOS 惯例）；布局探针（load/resize 量矩形回传
  日志）供后续回归核验。
- **通知**（路线图 1.1 路线 i，零后端改动）：SyncMonitor 与前端灵动岛
  同端点同口径——任务 `queued/running → completed/partial/failed` 跃迁、
  `subjects_pending >0 → 0` 跃迁发系统通知，点击唤起主窗口；首帧只建
  基线不回放历史。dev 直跑（无 bundle id）通知降级为日志。
- **托盘菜单**：打开主窗口 / 状态行（同步中 P% · 详情拉取中 N 条 · 空闲）/
  立即同步（全部已登记用户 POST）/ 打开数据文件夹 / 偏好设置… / GitHub
  仓库 / 退出。
- **壳设置原生偏好窗**（控件原生化第一件）：⌘,（应用菜单）/ Dock 右键
  菜单 / 托盘三入口的标准偏好窗口——开机自启 NSSwitch、全局唤起快捷键
  录制（复用 HotKeyRecorder 浮层）、数据文件夹入口；混合形态协同（窗开
  .regular，关窗且主窗不可见则降回 accessory）。**内容偏好（卡片显示两
  项）刻意留 web 弹层**：状态由 React 持有（localStorage），原生写不进去
  （只能 reload 生效）且 web 端同款弹层必须保留——双编辑器漂移税不值；
  控件原生的正确姿势是把「壳职能」界面原生出，而非给 web 控件换皮。
- **全局快捷键**（路线图 1.5）：Carbon `RegisterEventHotKey`（非沙盒系统级
  热键标准通道），默认 ⌘⇧M 唤起/收起主窗口（App 前台且窗口可见时热键=收起，
  Raycast 式 toggle）；组合经托盘菜单录制面板重录，UserDefaults 持久化，
  注册失败（被占用）告警不致命。**2.3 唤起直落搜索暂缓**：前端搜索功能
  未实现（TopBarSearch 为占位弹窗），落地后在唤起分支补一行
  `evaluateJavaScript` 聚焦搜索框即可（原生壳无 IPC 墙）。

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

App 默认使用微墨中心详情缓存，可在设置中关闭；中心离线或限流时自动从本机访问豆瓣。
`SYNC_SUBJECT_CACHE_URL=''` 可在启动时禁用，设置中不能覆盖该禁用。
偏好存入本地数据库，跨随机端口持久。中心仅收到条目编号和请求 IP，
不接收用户评分、短评或豆瓣 Cookie。详见[中心缓存方案](../中心条目缓存接入方案.md)。

## v1 范围外（后续项，见路线图）

- 自动更新：GitHub `latest.json` 属 Tauri 版版本线，与本壳版本不可比；
  分发与更新方案待定（Sparkle 或独立 manifest）后再做
- 唤起直落搜索（2.3）：等前端搜索功能实现后补一行 evaluateJavaScript
- 深链接（1.7，无场景暂不排期）
- 跟随系统深浅色（3.2）：纯前端项（设置加「跟随系统」档，web 端同
  受益），不属壳侧迁移
- 标题栏一体化的**视觉微调**（如红绿灯与磨砂条的对位观感）：几何已
  核验零碰撞，视觉定稿随 web 端设计迭代

## 已知待实测（GUI 侧留用户）

- 顶栏空白处拖拽窗口 / 双击 zoom、右键菜单交互、CSV 导出
  （blob → WKDownload → NSSavePanel）：均需真实键鼠操作。
- ad-hoc 签名下系统通知权限登记行为（首次发通知时请求授权）。

## 测试钩子

`WEIMO_MAC_START_URL`：覆盖窗口初始地址——完整 URL 原样用；`#/movie`
等以 # 开头的值拼在本实例 baseURL 上（回环端口每次随机，写死端口必踩
死链）。端到端探针/深链验证用。
