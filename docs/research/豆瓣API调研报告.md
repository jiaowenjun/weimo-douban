# 豆瓣 API 与书影音短评获取方案调研报告

> **调研日期**：2026-10-25
> **调研范围**：豆瓣官方 API 现状、获取"用户自己的书影音短评"的非官方技术路径、相关开源项目
> **数据说明**：文中所有 GitHub 项目的 Star 数、语言、最后更新时间均通过 GitHub API 逐一核实（截至调研当日），非搜索结果转述。

---

## TL;DR（一分钟速览）

1. **豆瓣官方 API 已停止服务**：官方文档站 `developers.douban.com` 早已下线，豆瓣不再向公众发放 API Key，目前**不存在任何可用的官方开放接口**。
2. **社区的事实替代方案有三条路**：登录 Cookie + 移动端内部接口（rexxar）、公开收藏页 HTML 抓取（免登录）、RSS 订阅（仅最新 10 条）。
3. **获取自己的书影音短评，首选现成开源项目**：
   - 普通用户一键备份 → 「豆伴」（doufen-org/tofu，Chrome 扩展）
   - 开发者命令行导出 → zcyoung2000/douban-backup、zx2592/douban_backup
   - 全自动持续同步 → lizheming/doumark-action（GitHub Action → CSV/Notion/NeoDB）
4. **所有方案均为非官方**，接口随时可能变更，需注意限速与账号风险（rexxar 条目接口的实测限流行为见 §2.1.1）。

---

## 目录

- [1. 豆瓣官方 API：历史与现状](#1-豆瓣官方-api历史与现状)
- [2. 非官方数据获取的三条技术路径](#2-非官方数据获取的三条技术路径)
- [3. 开源项目汇总（按使用场景分类）](#3-开源项目汇总按使用场景分类)
- [4. 选型建议](#4-选型建议)
- [5. 风险与注意事项](#5-风险与注意事项)
- [附录 A：关键接口与 URL 速查](#附录-a关键接口与-url-速查)
- [附录 B：参考资料](#附录-b参考资料)

---

## 1. 豆瓣官方 API：历史与现状

### 1.1 曾经的官方 API v2

豆瓣曾提供一套对外的开放平台 API（v2 版本），主要特征：

| 维度 | 说明 |
|---|---|
| 认证方式 | 统一使用 **OAuth2** |
| 返回格式 | 统一为 JSON，并支持 `callback` 参数（JSONP） |
| 数据覆盖 | 电影、图书、音乐、条目搜索、用户信息、广播、日记等 |
| 官方文档 | `https://developers.douban.com/wiki/?title=api_v2` |

### 1.2 现状：官方 API 已事实性停止服务

- **官方文档站已下线**：上述 URL 长期无法访问（连接直接被关闭），最早从 2018 年起就有开发者公开反映打不开。
- **不再发放 API Key**：新开发者无法申请凭证，`api.douban.com` 上的旧接口仅对极早期拿到 key 的合作方可能有效。
- **第三方生态的佐证**：Star 数 1378 的 Calibre 豆瓣元数据插件 [fugary/calibre-douban](https://github.com/fugary/calibre-douban) 在项目描述中明确写道 *"Douban no longer provides book APIs to the public"*。
- **结论**：任何"调用豆瓣官方 API"的设想目前都不成立，所有数据获取只能走非官方路径（见第 2 节）。

### 1.3 官方文档的社区镜像（最接近"官方文档"的资料）

官方文档关停后，社区留下了若干整理镜像，适合需要了解历史 API 设计（字段结构、OAuth2 流程）的开发者参考：

| 镜像 | 地址 | 说明 |
|---|---|---|
| byoungd 整理版 | [byoungd/douban-api-docs](https://github.com/byoungd/douban-api-docs) | 整理自官方 v2 文档，较完整 |
| goddlts 在线版 | [goddlts.github.io/douban-api-docs](https://goddlts.github.io/douban-api-docs/) | 可直接在线浏览的 API V2 文档 |
| SocratesClub 版 | [SocratesClub/douban-api-docs](https://github.com/SocratesClub/douban-api-docs) | 另一份文档仓库 |

### 1.4 澄清：豆瓣官方 GitHub 组织 ≠ 数据 API

GitHub 上的 [douban 官方组织](https://github.com/douban)确实存在且活跃，但开源的是**技术框架**而非数据接口，例如：

- [douban/rexxar-ios](https://github.com/douban/rexxar-ios) —— 豆瓣的混合移动开发框架
- [douban/DOUAudioStreamer](https://github.com/douban/DOUAudioStreamer)（⭐2747）—— 流媒体音频播放器
- douban/beansdb（已归档）—— 分布式 KV 存储

一个有趣的知识点：移动端内部接口的路径 `m.douban.com/rexxar/api/v2/...` 正是得名于 rexxar 框架，这也侧面说明它是**豆瓣 App 自己在用的内部接口**，而非对外承诺的开放 API。

---

## 2. 非官方数据获取的三条技术路径

所有能拿到"用户自己的书影音标记与短评"的社区项目，技术上都基于以下三条路径之一（或组合）。

### 2.1 路径一：登录 Cookie + 移动端内部接口（rexxar）

**原理**：豆瓣移动端网页（`m.douban.com`）暴露了一套内部 JSON 接口，统称 rexxar 接口，无需 API Key，但需要构造特定请求头（如 `Referer`）才能访问。

- 接口形态：`https://m.douban.com/rexxar/api/v2/...`
- 公开数据（榜单、条目详情）直接可访问，例如影视热门榜单 `/rexxar/api/v2/subject/recent_hot/`
- **涉及个人数据（自己的收藏、短评）时必须携带登录 Cookie**

**登录 Cookie 的三个关键字段**：

| Cookie 字段 | 作用 |
|---|---|
| `dbcl2` | 登录态凭证，格式形如 `"12345:abcdef"`，决定"你是谁" |
| `ck` | 写操作 / 身份校验参数（非登录态本身） |
| `bid` | 设备/浏览器标识，先访问一次 `www.douban.com` 即可获得 |

获取方式：浏览器登录豆瓣 → F12 打开开发者工具 → Network 面板复制任意请求的完整 Cookie 串。部分工具（如 zcyoung2000/douban-backup）支持自动读取本地浏览器的 Cookie。

#### 2.1.1 实测行为记录：rexxar 端点级突发限流（2026-10-28）

> 条目详情（公开数据，免 Cookie）请求 `https://m.douban.com/rexxar/api/v2/{category}/{id}`（移动端 UA + `Referer: https://m.douban.com/`）时的实测限流行为。背景：weimo-douban 后端富集期间生产日志出现 movie 条目批量 HTTP 400、全局熔断每 10 分钟一轮的循环；以下三组实验确诊根因。工程应对见《豆瓣源依赖性与脆弱性分析.md》F8。

**实验一：单发对照**——日志中失败的条目（狩猎 6985810 / Hello！树先生 4135710 / 肖申克的救赎 1292052）在封锁解除后逐个单发：**全部 200**，响应 11-17KB 完整 JSON。→ 400 不是条目级确定性失败（条目未下架、请求构造无误）。

**实验二：突发复现**——1s 间隔连发 8 个 movie 请求（随机 ID）：前 3 个 404（ID 不存在），**第 4 个起全部 400**。→ 连发 4-8 个请求即触发封锁，且 **404 同样累积计数**（批量探测不存在条目会更快触发）。

**实验三：恢复探测与端点隔离**——封锁后立即探测：movie 400，而**同一 IP、同样请求头下 music/book 端点 200**（端点级隔离，非 IP 全局封禁）；此后每 60-120s 探测一次 movie，封锁约 **5-6 分钟**自动解除（240s 仍 400，360s 已 200），无需换 IP/Cookie。

**安全间隔验证**——恢复后 20s 间隔连发 5 个 movie 请求全部 200；生产侧观察：3-4s 间隔下 music/book 长跑数百次无恙，movie 则成功数个后必再触发。→ **三类端点阈值差异极大，不能共用同一固定间隔**。

| 观察项 | 实测值 |
|---|---|
| 触发形态 | movie 端点连发 4-8 个请求（含 404）后，后续请求全部 400 |
| 封锁时长 | ~5-6 分钟自动解除（240s 仍封锁 / 360s 已恢复） |
| 作用域 | 端点级 × IP 级：`/movie/` 被封时 `/music/`、`/book/` 同 IP 正常 |
| 错误码形态 | **HTTP 400**（而非常规限流码 403/429），极易误判为客户端请求错误或条目失效 |
| movie 安全间隔 | 20s 稳定；3-4s 必触发 |
| music/book 容忍度 | 3-4s 间隔数百次无恙 |

**对使用者的启示**：

- 400 ≠ 请求构造错误 / 条目失效——先怀疑突发限流，用"封锁解除后单发对照"验证
- 重试退避必须跨过封锁窗（≥ 分钟量级指数退避），线性短退避（20s×n）会在封锁窗内烧光重试次数
- 按 category 分别限速与冷却：被限流的端点单独冷却，不要拖累其他端点或全局暂停
- 404 批量探测同样累积触发封锁

复现命令：

```bash
UA="Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.0 Mobile/15E148 Safari/604.1"
# 突发复现：1s 间隔连发 8 个（前几个 404 也计），第 4-8 个起观察 400
for i in 1 2 3 4 5 6 7 8; do
  curl -s -o /dev/null -w "%{http_code}\n" -H "User-Agent: $UA" -H "Referer: https://m.douban.com/" \
    "https://m.douban.com/rexxar/api/v2/movie/$((35600000 + RANDOM % 800000))"
  sleep 1
done
```

### 2.2 路径二：公开收藏页 HTML 抓取（免登录）

**原理**：每个豆瓣用户的书影音收藏页是**公开网页**（除非用户设置了隐私），直接抓取 HTML 并解析即可，无需任何登录。

以用户 ID `{uid}` 为例，各分站的收藏页路径规则统一（`wish`/`do`/`collect` 分别对应 想看、在看、看过）：

- 电影：`https://movie.douban.com/people/{uid}/collect`
- 图书：`https://book.douban.com/people/{uid}/collect`
- 音乐：`https://music.douban.com/people/{uid}/collect`

页面中每条记录包含：条目名、个人评分（星级）、**短评**、标记日期、标签。 doumark 系列、zh3li/douban-scraper 等项目采用此方案。

### 2.3 路径三：RSS 订阅（最简单，但只有 10 条）

**原理**：豆瓣为每个用户提供了书影音动态 RSS：

```
https://www.douban.com/feed/people/{uid}/interests
```

- 优点：标准格式、无反爬、实现成本最低
- 致命限制：**只包含最新约 10 条记录**，无法回溯历史
- 适用场景：配合定时任务（如每小时拉一次）**增量积累**数据，或作为通知源；不适合一次性全量导出

### 2.4 三条路径对比

| 维度 | ① Cookie + rexxar | ② 公开页抓取 | ③ RSS |
|---|---|---|---|
| 需要登录 | ✅ 需要（Cookie） | ❌ 不需要 | ❌ 不需要 |
| 能否拿到全量历史 | ✅ 能 | ✅ 能（公开部分） | ❌ 仅最新 10 条 |
| 能否拿到短评 | ✅ 能 | ✅ 能 | ⚠️ 部分能 |
| 数据格式 | JSON（结构化，最干净） | HTML（需解析） | XML |
| 封号/风控风险 | 相对较高（带身份） | 较低 | 几乎无 |
| 实现复杂度 | 中 | 中 | 低 |
| 接口稳定性 | 随时可能变 | 页面改版即失效 | 相对稳定 |

**实践建议**：只需自己的公开数据 → 路径②最稳妥；需要私密数据或更结构化的 JSON → 路径①；做增量同步 → 路径③做补充。

---

## 3. 开源项目汇总（按使用场景分类）

> Star 数与更新时间核实于 2026-10-25。"活跃"指当年仍有提交。

### 3.1 浏览器扩展（开箱即用，适合普通用户）

#### [doufen-org/tofu（豆伴）](https://github.com/doufen-org/tofu) ⭐626 · JavaScript · 2025-03 更新

最有名的豆瓣备份工具，Chrome 扩展（可在 Chrome 应用商店直接安装，也可本地加载），官方博客 [blog.doufen.org](https://blog.doufen.org)。

- 备份**自己或他人**的豆瓣数据：书影音标记、评论、日记、广播
- 数据存本地 IndexedDB，支持**离线浏览**备份内容
- 支持导出 Excel，图片可上传 Cloudinary 云存储
- 技术上较现代（Manifest V3 + Dexie）

#### [niall133/dou_archive（纳豆）](https://github.com/niall133/dou_archive) ⭐18 · JavaScript · 2026-09 活跃

Chrome / Edge 扩展，专注**观影记录导出与 [Letterboxd](https://letterboxd.com) 迁移**。导出字段完整：状态、条目 ID、片名、链接、个人评分、星级、标记日期、标签和**短评**。

#### [UlyC/DouBanExport](https://github.com/UlyC/DouBanExport) ⭐44 · JavaScript · 2023-07 后停更

豆瓣导出插件，无需登录，可导出到 Obsidian。⚠️ 两年未更新，接口可能已失效，选用前需自行验证。

### 3.2 命令行工具（可编程，适合开发者）

#### [zcyoung2000/douban-backup](https://github.com/zcyoung2000/douban-backup) ⭐17 · Python · 2026-06 活跃

- **自动读取本地浏览器 Cookie**（Chrome / Edge / Brave / Firefox / Safari），也可手动传入 `ck` / `dbcl2` 或完整 Cookie 串
- 备份书影音游剧全部标记（想看 / 在看 / 看过）为与「豆伴」结构一致的 Excel
- 原始数据存 SQLite，支持 `--user {ID}` 备份指定用户
- 基于 `uv` 运行，一条命令完成：`uv run douban_backup.py`

#### [zx2592/douban_backup](https://github.com/zx2592/douban_backup) ⭐13 · Python · 2026-09 活跃

一键导出电影、书籍、音乐、游戏和长评，字段包含评分、评语、标签、标记日期和封面图片；输出格式支持 **Excel / CSV / Markdown / JSON**。

#### [zh3li/douban-scraper](https://github.com/zh3li/douban-scraper) ⭐1 · Python · 2026-05 活跃

- 导出指定用户的评价（电影 / 书籍 / 音乐 / 广播）为 JSON / CSV
- **无需登录、无需浏览器自动化**（走路径②公开页抓取）
- 支持断点续爬、可配置限速 —— 工程细节较完善

#### [zzht1/douban-export](https://github.com/zzht1/douban-export) ⭐0 · Python · 2026-06 更新

将用户的电影和图书记录导出为 CSV 和 Markdown，提供 CLI 与 Flask Web 双界面，附带分析报告与社交卡片生成。

### 3.3 自动同步（GitHub Action / CI / Notion）

#### [lizheming/doumark-action](https://github.com/lizheming/doumark-action) ⭐105 · 2023-12 更新

**最省心的自动化方案**：一个 GitHub Action，抓取公开收藏页（无需登录、无需 Cookie），定时同步书影音游标记到多种目的地。

- 输入参数：豆瓣 ID、类型（movie / book / music / game）、状态（mark / doing / done）、格式（**csv / json / Notion / NeoDB**）
- 典型用法：每小时 cron 一次，数据自动 commit 到仓库，长期积累全量历史
- 使用教程见作者博客：[怡红院落《豆瓣书影音同步 GitHub Action 教程》](https://imnerd.org/doumark.html)
- 同作者还有 Drone CI 版本：[lizheming/drone-doumark](https://github.com/lizheming/drone-doumark)（⭐38）

#### 其他同步项目

- [AlieZVzz/Douban-Notion-Movie-Sync](https://github.com/AlieZVzz/Douban-Notion-Movie-Sync) ⭐5 · 2026-07 活跃 —— 从豆瓣 RSS 读取"看过"的电影，补充导演 / 类型 / 海报后写入 Notion（受 RSS 10 条限制）
- [bambooom/douban-backup](https://github.com/bambooom/douban-backup) —— 从 RSS 或油猴脚本导出的 CSV 同步到 Notion / NeoDB
- [stdrc/notion-douban-sync](https://github.com/stdrc/notion-douban-sync) —— 反向工具：把 Notion 里的书评**写回**豆瓣

### 3.4 笔记软件集成

| 项目 | Stars | 状态 | 说明 |
|---|---|---|---|
| [Wanxp/obsidian-douban](https://github.com/Wanxp/obsidian-douban) | ⭐274 | 2026-08 活跃 | Obsidian 插件，支持**扫码或 Cookie 登录**，把书影音条目及个人标记（含短评）拉成 Markdown 笔记，[官方文档](https://obsidian-douban.wxp.hk/) |
| [Glaube-TY/siyuan-douban](https://github.com/Glaube-TY/siyuan-douban) | ⭐30 | 2026-10 活跃 | 思源笔记插件：豆瓣书籍信息获取、自动生成读书笔记、同步微信读书笔记 |
| [cosformula/douban-sync-skill](https://github.com/cosformula/douban-sync-skill) | ⭐33 | 2026-02 活跃 | 把"豆瓣收藏同步到 Markdown"封装成 **AI agent 技能**，思路新颖 |

### 3.5 相关生态（非"个人短评"方向，供参考）

- **[NeoDB](https://github.com/neodb-social/neodb)** —— 开源书影音记录平台（豆瓣替代品），上述多个项目支持把豆瓣数据迁移过去，迁移思路见[《豆瓣逃离计划》](https://ulyc.github.io/2022/02/11/Douban-Escape-Plan)
- **[fugary/calibre-douban](https://github.com/fugary/calibre-douban)** ⭐1378 —— Calibre 豆瓣元数据插件（替代已关闭的官方图书 API）
- **[zhanghai/Douya](https://github.com/zhanghai/Douya)** ⭐4531 —— Material Design 第三方豆瓣客户端（2021 停更）
- **[lanbing510/DouBanSpider](https://github.com/lanbing510/DouBanSpider)** ⭐2789 —— 豆瓣读书爬虫（条目数据，非个人数据）
- **[iphysresearch/TOP250movie_douban](https://github.com/iphysresearch/TOP250movie_douban)** ⭐1028 —— 豆瓣 Top250 **电影条目短评**的爬取与情感分析（注意：爬的是电影的公开短评，不是用户自己的）
- **API 代理类**（解决小程序等场景 403，均较老旧）：[byoungd/douban-api-proxy](https://github.com/byoungd/douban-api-proxy)、qfz9527/douban-api-proxy、zce/douban-api-proxy

### 3.6 项目速查总表

| 项目 | Stars | 形态 | 免登录 | 维护状态 | 一句话定位 |
|---|---|---|---|---|---|
| [doufen-org/tofu](https://github.com/doufen-org/tofu) | 626 | Chrome 扩展 | ❌ 需登录 | 2025-03 | 全量备份 + 离线浏览 + Excel |
| [Wanxp/obsidian-douban](https://github.com/Wanxp/obsidian-douban) | 274 | Obsidian 插件 | ❌ 需登录 | 活跃 | 短评 → Markdown 笔记 |
| [lizheming/doumark-action](https://github.com/lizheming/doumark-action) | 105 | GitHub Action | ✅ | 2023-12 | 定时同步 CSV/Notion/NeoDB |
| [UlyC/DouBanExport](https://github.com/UlyC/DouBanExport) | 44 | 浏览器插件 | ✅ | ⚠️ 停更 | 导出 / Obsidian |
| [cosformula/douban-sync-skill](https://github.com/cosformula/douban-sync-skill) | 33 | Agent 技能 | ✅ | 活跃 | 同步到 Markdown |
| [Glaube-TY/siyuan-douban](https://github.com/Glaube-TY/siyuan-douban) | 30 | 思源笔记插件 | ❌ | 活跃 | 读书笔记管理 |
| [lizheming/drone-doumark](https://github.com/lizheming/drone-doumark) | 38 | Drone CI 插件 | ✅ | 2023-12 | doumark 的 CI 版 |
| [niall133/dou_archive](https://github.com/niall133/dou_archive) | 18 | Chrome/Edge 扩展 | ✅ | 活跃 | Letterboxd 迁移 |
| [zcyoung2000/douban-backup](https://github.com/zcyoung2000/douban-backup) | 17 | Python CLI | ❌ 需登录 | 活跃 | 自动读浏览器 Cookie |
| [zx2592/douban_backup](https://github.com/zx2592/douban_backup) | 13 | Python CLI | ❌ 需登录 | 活跃 | 多格式一键导出 |
| [zh3li/douban-scraper](https://github.com/zh3li/douban-scraper) | 1 | Python CLI | ✅ | 活跃 | 断点续爬 + 限速 |
| [zzht1/douban-export](https://github.com/zzht1/douban-export) | 0 | CLI + Flask | ✅ | 2026-06 | CSV/Markdown 导出 |

---

## 4. 选型建议

### 4.1 按需求场景推荐

| 你的需求 | 推荐方案 |
|---|---|
| 一次性把自己账号的数据备份下来看看 | **豆伴（tofu）** 扩展一键备份，或 **zcyoung2000/douban-backup** 命令行导 Excel |
| 想要评分、短评、标签等完整字段的 Excel/CSV | **zx2592/douban_backup** 或 **豆伴** |
| 数据长期自动积累（不想手动跑） | **lizheming/doumark-action**：GitHub 定时任务 + CSV/Notion |
| 迁移到 Letterboxd 等其他平台 | **niall133/dou_archive（纳豆）**；迁往 NeoDB 可用 doumark-action（format: neodb） |
| 导入 Obsidian / 思源等笔记软件 | **Wanxp/obsidian-douban**、Glaube-TY/siyuan-douban，或参考[少数派教程](https://sspai.com/post/103817) |
| 自己写代码实现（学习/定制） | 见 4.2 |

### 4.2 自研参考路线

若要在本仓库（`weimo-douban`）自行实现，建议：

1. **首选免登录路线**（路径②）：抓取 `movie/book/music.douban.com/people/{uid}/{wish|do|collect}` 分页 HTML，解析出条目、星级、短评、日期、标签。参考 [zh3li/douban-scraper](https://github.com/zh3li/douban-scraper) 的分页与断点续爬实现。
2. **需要更干净的结构化数据时**（路径①）：用登录 Cookie 访问 `m.douban.com/rexxar/api/v2/...` 内部接口。参考 [zcyoung2000/douban-backup](https://github.com/zcyoung2000/douban-backup) 的 Cookie 获取（自动读浏览器）与请求构造。
3. **做增量同步**（路径③）：RSS feed + 定时拉取作为补充，注意只覆盖最新 10 条。
4. **建议实现的功能点**（借鉴现有项目共性）：限速（数秒一次）、断点续爬、原始数据先落 SQLite、导出层支持 Excel / CSV / Markdown / JSON。

---

## 5. 风险与注意事项

1. **无官方授权**：所有路径均为非官方方案，豆瓣没有任何兼容性承诺，接口 / 页面结构 / Cookie 规则**随时可能变更**，项目失效是常态（如 UlyC/DouBanExport 停更后大概率已不可用）。
2. **限速是硬要求**：请求间隔建议 ≥ 数秒；高频抓取可能导致封 IP，带 Cookie 的高频请求甚至可能**封号**。
3. **Cookie 安全**：`dbcl2` 等同于账号密码，只在本地工具中使用，不要提交到公开仓库、不要发给第三方服务（警惕需要你粘贴 Cookie 的在线网站）。
4. **隐私边界**：免登录路线只能拿到对方**公开**的标记；备份他人数据时注意尊重隐私设置。
5. **数据时效**：开源项目的 Star 数与维护状态会变化，选用前建议到仓库确认最近提交与 issue 区的可用性反馈。

---

## 附录 A：关键接口与 URL 速查

| 用途 | URL / 字段 | 备注 |
|---|---|---|
| 官方 API 文档（历史地址） | `https://developers.douban.com/wiki/?title=api_v2` | ❌ 已关停，仅存档意义 |
| 文档镜像（在线） | https://goddlts.github.io/douban-api-docs/ | 社区整理 |
| rexxar 内部接口 | `https://m.douban.com/rexxar/api/v2/...` | 需构造请求头；个人数据需 Cookie |
| rexxar 条目详情（公开，免 Cookie） | `https://m.douban.com/rexxar/api/v2/{movie\|book\|music}/{id}` | 移动端 UA + `Referer: https://m.douban.com/`；⚠️ **movie 端点严格突发限流**（连发 4-8 个即 400 封锁 ~5-6 分钟，404 亦累积，详见 §2.1.1） |
| rexxar 榜单示例 | `https://m.douban.com/rexxar/api/v2/subject/recent_hot/` | 公开数据，无需登录 |
| 电影收藏页（看过） | `https://movie.douban.com/people/{uid}/collect` | 公开页，`wish`/`do` 对应想看/在看 |
| 图书收藏页（读过） | `https://book.douban.com/people/{uid}/collect` | 同上 |
| 音乐收藏页（听过） | `https://music.douban.com/people/{uid}/collect` | 同上 |
| 个人书影音 RSS | `https://www.douban.com/feed/people/{uid}/interests` | ⚠️ 仅最新约 10 条 |
| 登录 Cookie 关键字段 | `dbcl2`（登录态）、`ck`（校验）、`bid`（设备标识） | F12 → Network → 复制完整 Cookie |

## 附录 B：参考资料

**官方相关**

- 豆瓣官方 GitHub 组织：https://github.com/douban （框架开源，非数据 API）
- rexxar 混合开发框架：https://github.com/douban/rexxar-ios
- API v2 文档镜像：[byoungd/douban-api-docs](https://github.com/byoungd/douban-api-docs) · [goddlts 在线版](https://goddlts.github.io/douban-api-docs/) · [SocratesClub/douban-api-docs](https://github.com/SocratesClub/douban-api-docs)

**教程与文章**

- [怡红院落：豆瓣书影音同步 GitHub Action 教程](https://imnerd.org/doumark.html)（doumark 作者）
- [少数派：将豆瓣书影音记录迁移至 Obsidian](https://sspai.com/post/103817)
- [豆瓣逃离计划（迁移到 NeoDB 等）](https://ulyc.github.io/2022/02/11/Douban-Escape-Plan)
- [eallion：豆瓣同步到 Notion 和 NeoDB](https://www.eallion.com/douban-sync)（RSS 10 条限制说明）
- [豆伴官方博客](https://blog.doufen.org) · [Obsidian-Douban Cookie 登录文档](https://obsidian-douban.wxp.hk/20_howtouse_25_setting_login_douban_cookie.html)

**本文核实的开源项目**（均在第 3 节列出并附链接，此处从略）
