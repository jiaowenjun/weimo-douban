# tauri app · mac 原生渐进式迁移路线图

> 评估对象：`app/`（Tauri 2 壳 + 内嵌同步服务器，方案 B 全栈封装，v0.4.2）。
> 本文回答一个问题：**桌面端可以渐进式做哪些原生迁移，按什么顺序做，哪些明确不做。**
> 通用方法论（三层原生化：系统层 / 控件层 / 架构层）在此按本项目实际裁剪——本文所有现状描述均对照代码核实，涉及外部 API 的事实注明已验证/需实测。

---

## 0. 结论先行

1. **本项目不需要、也不应该做 UI 层的原生重写。** 业务边界已经天然干净（backend 是独立 crate，HTTP API `/api/v1` 就是业务层，web 端与 App 壳共用同一份前端），UI 是产品身份（weimo-ui 设计系统、FLIP 卡片、热力图、tagtree）。SwiftUI 重写 = 全新客户端工程，对单一维护者的收益/成本比不成立。
2. **真正的原生化缺口集中在壳层系统交互**，且大部分是纯 Rust 侧改动、完全不碰前端：
   - **P0 四件套**：系统通知、托盘菜单富化、窗口状态记忆、外链走系统浏览器；
   - P1：全局快捷键唤起、开机自启；
   - P2+：NSSavePanel 导出、原生右键菜单、标题栏一体化等，需要「过 IPC 墙」，先做架构决策再做功能。
3. **「过 IPC 墙」是本项目原生化唯一的架构级决策**（现状刻意零 IPC、前端零 Tauri 依赖）。推荐次序：壳侧无感拦截（C，够用优先）＞ 壳注入桥（B）＞ 官方 remote IPC（A）。
4. **终局推荐长期混合架构**（保留 Tauri + WebView 渲染主界面，壳层持续原生化）。独立 SwiftUI 客户端列为远期观望项，且真做时也是「换壳不换核」——backend 以 sidecar 进程形态复用，HTTP 边界现成，不需要 Swift↔Rust FFI。

---

## 1. 现状盘点（已核实）

### 1.1 架构

```
微墨·豆瓣.app（单进程，LSUIElement 无 Dock 图标）
├─ Tauri 2.12.5 / wry 0.57.1（WKWebView 窗口壳，无自定义协议、无 IPC）
├─ weimo_douban::server::build_server()        ← backend 以 lib 依赖直链内嵌
│   ├─ axum API（/api/v1/**，含抓取/富集/订阅后台任务）
│   └─ rust-embed 前端 dist → 挂载 /douban/     ← 编译期进二进制，整栈原子快照
├─ 窗口加载 http://127.0.0.1:<随机回环端口>/douban/（前后端同源，零 CORS）
└─ 托盘常驻：左键开/聚焦窗口，右键菜单；关窗 = 隐藏不退出
```

### 1.2 已具备的原生能力

| 能力 | 实现 | 位置 |
|---|---|---|
| 托盘（NSStatusItem） | 左键开窗 / 右键菜单（打开主窗口、检查更新…、退出） | `app/src/main.rs:280-323` |
| 菜单栏常驻形态 | `LSUIElement=true`（无 Dock 图标） | `app/Info.plist` |
| 单实例 | 二次启动聚焦既有窗口（防双进程写 SQLite） | `main.rs:60-62` |
| 自动更新 | updater 插件，GitHub Releases manifest + minisign，启动静默自检 + 托盘手动检查 | `main.rs:64-68, 328-410` |
| 原生消息对话框 | dialog 插件（下载完成含「在访达中显示」、更新确认/结果） | `main.rs:30, 213-245` |
| 下载生命周期 | `on_download` 钩子放行 CSV blob 导出 → 系统 Downloads + 完成弹窗 + `open -R` 定位 | `main.rs:167-190, 249-267` |
| 默认应用菜单 | `Menu::default`（LSUIElement 下窗口激活时菜单栏可见，Edit 复制粘贴等可用） | `main.rs:69` |
| 主线程建窗 | `run_on_main_thread`（macOS 后台线程建窗丢尺寸/定位的坑已修） | `main.rs:140-144` |

### 1.3 三条刻意的设计约束（后续所有决策的前提）

1. **零 IPC**：窗口加载的是内嵌服务器的 `http://` 地址（非 `tauri://` 协议），前端无法调用 `invoke()`/事件系统——这是换取「前端零改动、与线上 weimo.ink/douban/ 完全同构」的代价。壳内唯一信号是 Tauri 对所有 webview 自动注入的 `window.isTauri`（前端 `welcome-page.tsx:38-40` 以此区分壳内外）。
2. **前端零 Tauri 依赖**：`frontend/web/package.json` 无任何 `@tauri-apps/*`；同一份构建产物既上 txy 又进 App 二进制。
3. **整栈原子快照**：dist 经 rust-embed 编进二进制，`cache_version` 协议前后端永远同版。

> 推论：**凡是纯壳侧（Rust）能做的原生化，都不破坏以上三条；凡是需要页面配合的，必须先「过 IPC 墙」（见 §3 阶段二的路线决策）。**

### 1.4 可复用的后端事件面（对壳侧原生通知至关重要，已核实）

- 进程内已有广播总线：`AppState.progress_tx: broadcast::Sender<String>`（`backend/src/server/state.rs:21`），`subscribe_progress()`（`state.rs:69`）可订阅；SSE 端点 `/api/v1/syncs/{task_id}/events`（`routes.rs:63`）即建在其上。
- 版本轮询协议：`/users/{uid}/cache-version`、富集任务 `/subjects/jobs`——前端灵动岛（TopBarSyncNotice）与 3s 轮询用的就是这些端点，壳侧可直接复用同一协议。
- 目前 `build_server` 只返回 Router，壳拿不到 `AppState` 句柄——需要暴露一个返回句柄的装配变体（小改，见 2.1）。

---

## 2. 本项目的原生化空间：三层框架套用

| 层次 | 通用定义 | 本项目结论 |
|---|---|---|
| **系统层**（窗口、菜单、托盘、通知、快捷键、文件对话框、Dock） | 系统能力集成 | **主战场，缺口清单见路线图**。且本项目重心是**托盘中心**而非菜单栏中心——LSUIElement 常驻形态下，托盘菜单/通知/全局快捷键才是第一交互面（窗口多数时间隐藏）。 |
| **控件层**（原生按钮、输入框、列表） | 页面内控件原生化 | **基本无需求**。主界面是浏览/筛选/统计型 UI，自绘设计系统（weimo-ui）是产品身份，跨 web/App 一致是特性不是债。逐控件原生化明确不做。 |
| **架构层**（业务与 UI 分离） | 共享业务层供双端复用 | **已经就绪**。backend 独立 crate + HTTP API 就是业务边界；通用方法论里「阶段二：剥离业务逻辑」对本项目而言是已完成项，无需再做。 |

另两个与通用叙事不同的点：本项目**无编辑器**（通用方法论最重的「编辑器迁移/WebView 编辑器过渡」议题无对应物）；**web 端是一等公民**（txy 部署，App 每次发版须与网页端对齐，见 `ops/txy部署手册.md` §7.5），任何双端分叉（UI 或行为）都是持续的维护税——这是「不做 SwiftUI 重写」的独立论据。

---

## 3. 渐进式路线图

> 成本记号：S（半天内）/ M（1–2 天）/ L（3 天以上）。所有阶段一改动均为纯壳侧，不碰前端构建产物，上线只走 App release；阶段二起涉及前端，发布必须按 §7.5 联动网页端。

### 阶段一：壳层系统集成（P0/P1，纯 Rust 侧）

#### 1.1 系统通知 —— P0，托盘形态下的最大缺口

- **现状**：同步完成/失败、更新就绪只体现在页面内灵动岛或弹窗；**窗口隐藏时（托盘常驻的常态）用户完全无感知**。订阅轮询/富集在后台默默推进，结果无人知晓。
- **方案**：`tauri-plugin-notification`（跨平台，Windows 同步受益）。触发数据源两条路：
  - **路线 i（零后端改动，先行验证形态）**：壳侧异步任务轮询自己的 localhost API（`/api/v1/syncs` + `/users/{uid}/cache-version` + `/subjects/jobs`），与前端灵动岛同一协议、同一判定口径（syncing→终态跃迁时发通知）；
  - **路线 ii（进程内事件，后续归位）**：backend 加一个 `build_server_with_handle` 装配变体（返回 `(Router, AppState)` 或事件句柄），壳直接 `subscribe_progress()`——纯装配层改动，零语义变化，后端测试不受影响。
- **联动**：通知点击 → `show_main_window()`（已有，`main.rs:269`）。与 1.2 托盘状态行共用同一数据源任务。
- **需实测**：ad-hoc 签名（未 Developer ID 公证）App 的 macOS 通知权限登记行为；通知中心展示的 App 名。
- 成本 S–M。

#### 1.2 托盘菜单富化 —— P0

- **现状**：3 项（打开主窗口/检查更新…/退出，`main.rs:281-289`），托盘作为第一交互面却几乎裸奔。
- **方案**：
  - 加「立即同步」（对已登记 uid POST `/api/v1/users/{uid}/syncs`，多用户则逐个或取全部）；
  - 加「打开数据文件夹」（reveal `app_data_dir`，`reveal_in_file_manager` 已有 `main.rs:249`）；
  - 加同步状态行（disabled MenuItem 实时文本「同步中 42%…」/「空闲」，数据源同 1.1）；
  - 可选：托盘 `set_title` 直接在菜单栏显示同步百分比（NSStatusItem 文本惯例；`TrayIcon::set_title` 已在 tauri 2.12.5 源码核实存在，`tauri/src/tray/mod.rs:538`。**需实测**：Windows 托盘无 title 概念，该调用在 Windows 的行为/是否纯 mac 分支）。
- 成本 S。

#### 1.3 窗口状态记忆 —— P0

- **现状**：每次启动 `window.center()`（`main.rs:196`），用户调好的尺寸/位置不保留。
- **方案**：`tauri-plugin-window-state`。注意与 `center()` 冲突：有恢复状态时须跳过居中（插件恢复时机与手写 center 的先后需核对，实现时以「首次启动才 center」为准）。
- 成本 S。

#### 1.4 外部链接走系统浏览器 —— P0

- **现状（需实测）**：欢迎页顶栏 GitHub 按钮、条目外链等 `target=_blank`/`window.open` 在 WKWebView 中的默认行为未验证（wry 对新窗口的处理可能是无反应或开裸 webview 窗口，两者体验都差）。
- **方案**：`WebviewWindowBuilder::on_navigation` 拦截非 `127.0.0.1` 导航 → return false 拒绝 + `tauri-plugin-opener` 交系统默认浏览器打开。纯壳侧。
- **需实测**：HashRouter 的 hash 变化不触发 navigation 回调（不应误拦）；`target=_blank` 是否走 navigation 回调还是需 `on_new_window`。
- 成本 S。

#### 1.5 全局快捷键唤起 —— P1

- **方案**：`tauri-plugin-global-shortcut`（如 ⌘⇧M）唤起+聚焦主窗口。本 App 的高频动作就是「随手查一部片/书」，托盘常驻 + 全局快捷键 = Raycast 式唤起，是原生体验增量最大的一项。
- **前置小决策**：快捷键目前无用户配置面——App 无设置持久层（可存 `app_data_dir` 下配置文件，或后端加 kv 端点，倾向前者）。
- 成本 S–M。可叠加 2.3（唤起直落搜索）。

#### 1.6 开机自启 —— P1

- **方案**：`tauri-plugin-autostart` + 托盘菜单开关项。README 哲学「数据新鲜度 = App 存活期」的自然延伸：自启 = 订阅/富集真正常驻。
- 成本 S。

#### 1.7 深链接 `weimo-douban://` —— P2，暂不排期

- 当前是单用户本地工具，无「网页→App 跳转」场景，收益低。列于此备查（若将来做「在 App 中打开」或浏览器书签直达再加）。

### 阶段二：过 IPC 墙——先做路线决策，再做功能

需要页面主动配合壳的能力，必须先选一条过墙路线（**架构决策记录**）：

| 路线 | 做法 | 优点 | 代价/风险 |
|---|---|---|---|
| **C：壳侧无感拦截** | `on_download`/`on_navigation` 等壳层回调，前端零配合 | 不碰前端、不破坏三条约束 | 能力限于壳能观察到的浏览器事件 |
| **B：壳注入桥（推荐）** | 壳经 initialization_script 注入 `window.weimoShell`，页面与壳之间的请求走内嵌服务器加壳专属端点（如 `/api/v1/shell/*`），前端只是 `fetch` | 前端保持零 Tauri 依赖（fetch 与现有 API 调用同范式）；能力面完全自控 | 自维护一份小协议；壳端点要防外部访问（仅回环+同源校验，现状已具备） |
| **A：官方 remote IPC** | tauri capability 的 `remote.urls` 授权 `http://127.0.0.1` 源，前端动态 import `@tauri-apps/api`（`IN_TAURI_APP` 守卫，web 构建不加载） | 官方插件全家桶（dialog/fs/opener…）直接可用 | **需实测**：随机端口下通配符支持；破坏「前端零 Tauri 依赖」洁癖；前端构建需条件加载改造 |

> 推荐次序：**C 够用就 C；需要页面主动调用时用 B**（与现有架构哲学最一致）；仅当前端确实需要官方插件全家桶时才引入 A。

#### 2.1 CSV 导出升级 NSSavePanel —— P2

- **现状**：blob 下载 → 系统 Downloads + 完成弹窗 + 访达定位（`main.rs:167-190`），已可用。
- **方案（先走 C）**：`DownloadEvent::Requested` 的 `destination` 是 `&mut PathBuf`、可在回调内改写（已在 tauri 2.12.5 源码核实，`tauri/src/webview/mod.rs:87`）——弹原生保存面板让用户选位置后改写。**需实测**：Requested 回调内同步等用户点按的时序（回调持有借用，阻塞等面板可能卡死下载委托）；不可行则回落 B/A（前端直接调原生保存对话框 + 写文件）。
- 成本 M。

#### 2.2 原生右键菜单 —— P2

- 标记卡片右键：复制条目标题/豆瓣链接、在豆瓣打开（联动 1.4 的 opener）。
- 需 A/B（JS `contextmenu` → 壳 `Menu::popup`）。成本 M。

#### 2.3 快捷键唤起直落搜索 —— P1（依赖 1.5）

- 全局快捷键唤起后自动聚焦页内搜索框：路线 B 注入脚本调 focus，或纯前端监听 URL query（如 `?focus=search`，web 端无害）。成本 S。

### 阶段三：窗口与视觉原生化（动前端，需 §7.5 联动发布）

#### 3.1 标题栏一体化 —— P3

- **现状**：系统默认标题栏 + 前端自绘 topbar，双层顶条。
- **方案**：`titleBarStyle` HiddenInset/overlay + 前端 `IN_TAURI_APP` 条件安全区。**主要坑**：无 IPC 则窗口拖拽区不可用（Tauri 的 drag-region 走 IPC）——**本项依赖过墙（A/B）**，是它被排在 P3 的原因。成本 M–L。

#### 3.2 跟随系统深浅色 —— P3（实际是纯前端改动，web 端同样受益）

- WKWebView 的 `prefers-color-scheme` 跟随系统，前端 `matchMedia` 即可监听，壳零配合；产品决策点是应用内手动主题与「跟随系统」档的优先级（设置页加一档）。成本 S–M。

#### 3.3 NSVisualEffectView 材质窗口 —— 可选，不排期

- `window-vibrancy`；与 weimo-ui 材质体系（card-surface）叠加效果需设计评估。

### 阶段四：终局

**4A. 长期混合架构（推荐，默认路线）**：壳继续 Tauri，UI 继续 web，壳层按本路线图持续原生化。论据：① weimo-ui 是跨端产品身份；② 业务边界已干净；③ SwiftUI 重写对单维护者是不成比例的工程量（tagtree/FLIP/热力图/富集交互全要重做）；④ web 端是一等公民，双 UI 是持续分叉税。

**4B. 独立 SwiftUI 客户端（远期观望，不排期）**。触发条件（满足其三再立项）：macOS 成为绝对主平台；需要 Spotlight/Shortcuts/Widget/菜单栏实时图表级深度系统集成；web 端退役或降级。**真做时的形态是「换壳不换核」**：backend 已是独立 lib 且 HTTP API 完备，SwiftUI 客户端以 sidecar 进程跑 `weimo_douban` 二进制（同今日 App 形态，只把 WebView 壳换成 SwiftUI 壳），经 localhost HTTP 通信——不需要 Swift↔Rust FFI（异步、错误、内存、类型转换的长期维护成本远高于走现成 HTTP 边界）。

**明确不做**：
- Tauri 内嵌 SwiftUI 视图（NSHostingView 手工桥接，无官方支持，布局/焦点/事件全自管，深水区）；
- 逐控件原生化（无需求面，见 §2 控件层结论）；
- 通用方法论的「WebView 编辑器过渡」（本项目无编辑器议题）。

---

## 4. 风险与坑（实录 + 待实测清单）

**已实录（见 `app/README.md` 与既往排障）**：`cargo build` 不更新 .app bundle 副本；CGWindowList 无录屏权限时 bounds 被涂改；后台线程建窗丢尺寸（须 `run_on_main_thread`）；更新器端点须 `updater_builder` 自建；更新私钥丢失即断更新链；发布期间工作树必须干净（防 mac/win 内容分叉）+ 并行会话 stash 撞车；Release 资产名必须 ASCII。

**本次新增待实测**：

| # | 事项 | 关联项 |
|---|---|---|
| 1 | ad-hoc 签名 App 的 macOS 通知权限登记与展示名 | 1.1 |
| 2 | `on_navigation` 对 HashRouter hash 变化零误拦；`target=_blank` 实际走向 | 1.4 |
| 3 | `DownloadEvent::Requested` 回调内弹保存面板的时序可行性 | 2.1 |
| 4 | remote IPC 对随机端口（`127.0.0.1:*`）的通配支持 | 路线 A |
| 5 | `TrayIcon::set_title` 的 Windows 行为（是否需 cfg 分支） | 1.2 |
| 6 | `window-state` 恢复与手写 `center()` 的先后协调 | 1.3 |

**Windows 兼容铁律**：`main.rs` 目前「无平台分支」是既有哲学——所有新 macOS 特性要么选跨平台插件（notification/global-shortcut/autostart/window-state 均跨平台），要么 `#[cfg]` 兜住 Windows 编译与行为；每次壳改动双平台至少编译验证（mac 本地 + app-windows CI）。

**发布联动**：阶段一改动不进 web bundle → 只走 App release 流程（`app/scripts/release.sh` + `app-windows` dispatch）；阶段二/三起涉及前端 → **发版必须按 `ops/txy部署手册.md` §7.5 与网页端联动**（App 与 weimo.ink 同一 Release 对齐）。

---

## 5. 优先级总表

| 优先级 | 项 | 路线图 | 成本 | 前置 |
|---|---|---|---|---|
| **P0** | 系统通知（同步/富集/更新就绪） | 1.1 | S–M | 待实测 #1 |
| **P0** | 托盘菜单富化（立即同步/数据文件夹/状态行） | 1.2 | S | 与 1.1 共用数据源 |
| **P0** | 窗口状态记忆 | 1.3 | S | 待实测 #6 |
| **P0** | 外链走系统浏览器 | 1.4 | S | 待实测 #2 |
| P1 | 全局快捷键唤起（+直落搜索 2.3） | 1.5/2.3 | S–M | 快捷键配置持久化小决策 |
| P1 | 开机自启 | 1.6 | S | — |
| P2 | CSV 导出 NSSavePanel | 2.1 | M | 待实测 #3，可能回落 A/B |
| P2 | 原生右键菜单 | 2.2 | M | 需过墙（A/B） |
| P2 | 深链接 | 1.7 | S | 暂不排期 |
| P3 | 标题栏一体化 | 3.1 | M–L | 依赖过墙（drag-region） |
| P3 | 跟随系统深浅色 | 3.2 | S–M | 产品决策（手动/跟随优先级） |
| 远期 | 独立 SwiftUI 客户端（换壳不换核） | 4B | L | 触发条件见 §3 阶段四 |

**推荐节奏**：下一个壳版本做 P0 四件套（一个迭代内可完成，全部纯壳侧）；P1 跟随下一版；P2 起先落「过 IPC 墙」的路线决策（推荐 B），再逐项做；P3 视前端迭代窗口安排。

---

## 6. 验证口径

- 阶段一（纯壳）：`cargo build` 双平台过 + `app/README.md` 既有排障口径（进程内 tao 查询窗口状态，不信 CGWindowList）+ 手测清单（托盘各项/通知点击/重启恢复位置/外链打开）。
- 阶段二/三（涉前端）：`tsc -b`/`oxlint` 0 警 0 错、前端测试全绿之外，**必须回归 `IN_TAURI_APP === false` 路径**（线上站点与本地 dev 行为零变化），IAB 走查壳内路径；发布按 §7.5 联动并过一致性断言。
