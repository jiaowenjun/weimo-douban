# 微墨·豆瓣（weimo-douban）

免登录获取豆瓣用户的**公开书影音标记与短评**并同步入库，对外提供 HTTP API（电影/图书/音乐 × 看过·读过·听过/在看·在读·在听/想看·想读·想听）。

原理与背景见 [豆瓣API调研报告.md](豆瓣API调研报告.md) 中的"路径②：公开收藏页 HTML 抓取"——豆瓣官方 API 已停止服务，本工具直接抓取公开收藏页（`movie/book/music.douban.com/people/{uid}/...`）并解析，**无需登录、无需 Cookie**。

服务器 API 规范见 [服务器API设计.md](服务器API设计.md)（已实现）；技术栈与实现模式的分析见 [服务器技术选型.md](服务器技术选型.md)（基于 weimo 系列后端的实地考察，结论：axum + tokio + sqlx/SQLite + DB 即队列的 worker 模式）。

Rust 实现（reqwest + scraper + clap），编译为单二进制服务器，无需运行时环境：

- `weimo-douban-server`——axum 同步服务器（异步任务 + 维度缓存 + 订阅增量 + 全局限速），API 规范见 [服务器API设计.md](服务器API设计.md)

## 使用

```bash
cd backend
cargo build --release
SYNC_SERVER_BIND=0.0.0.0:8080 \
SYNC_SERVER_DATA_DIR=./data \
SYNC_SERVER_TOKEN=my-secret \        # 留空则不启用鉴权
SYNC_DOUBAN_COOKIE='dbcl2="xxx"' \   # 可选：登录抓取，补全豆瓣隐藏条目
./target/release/weimo-douban-server
```

核心流程（异步任务模型，细节见设计文档）：

```bash
# 1. 创建任务（wait_timeout 可选同步等待 ≤30s）
curl -X POST http://127.0.0.1:8080/api/v1/syncs \
     -H "Authorization: Bearer my-secret" -H "Content-Type: application/json" \
     -d '{"user":"wenjun30","categories":["movie","book","music"]}'

# 2. 轮询进度（progress[] 按维度给出 fetched/total）
curl -H "Authorization: Bearer my-secret" http://127.0.0.1:8080/api/v1/syncs/{task_id}

# 3. 取结果汇总（各维度条目数与含短评数）
curl -H "Authorization: Bearer my-secret" http://127.0.0.1:8080/api/v1/syncs/{task_id}/result

# 4. 结构化读库内数据（分页 JSON，按日期倒序；未缓存返回 404 引导建任务）
curl -H "Authorization: Bearer my-secret" \
     "http://127.0.0.1:8080/api/v1/users/wenjun30/marks?category=movie&status=collect&page=1&page_size=50"
```

抓取结果直接存入 SQLite `records` 表（一条标记一行，按维度长期保留，不随任务过期清理）；服务器不生成导出文件，数据经 API 查询。

其余端点：任务列表/删除（`GET/DELETE /api/v1/syncs`）、封面代理（`GET /api/v1/covers?u=...`，绕过豆瓣 CDN 防盗链并磁盘缓存，`/marks` 的 cover 字段已自动改写指向它）、服务元信息（`GET /api/v1/server/info`）、健康检查（`GET /healthz`，不鉴权）。全部配置项（限速、TTL、熔断、保留期等）经环境变量设置，见 `backend/src/server/config.rs`。

内置防护：按 `(uid, category, status)` 维度缓存（TTL 24h，`refresh:true` 强制刷新）、同维度 single-flight、单 worker 串行 + 每次抓取 ≥2s 间隔（全局令牌桶）、403/超时指数退避重试、连续 5 次失败熔断 10 分钟、per-IP 限流。

缓存恢复、内容版本、HTTP 条件请求及全量同步快照策略见 [缓存与同步策略.md](缓存与同步策略.md)。

## macOS 桌面 App（app/）

Tauri 2 全栈封装：backend 以 lib 内嵌、前端 dist 编进二进制，单进程在回环随机端口同时服务 API 与静态资源，窗口加载同源 `/douban/`——零 CORS、零跨进程、不依赖线上服务器。桌面形态：菜单栏常驻（LSUIElement + 托盘，关窗后台任务继续）、自动更新走 Gitee 国内源（GitHub Releases 回退，主要用户群在大陆）：

```bash
pnpm --dir app install
pnpm --dir app tauri build          # 产出 .app / .dmg / 更新签名产物
./app/scripts/release.sh            # 发布：gh release + latest.json + Gitee 镜像同步
```

详见 [app/README.md](app/README.md)。

## 管理后台（frontend/admin/）

服务器自带一个 Ant Design Pro 风格的管理页面（Vite + React + pnpm），提供概览监控、抓取任务的创建/进度、库内数据浏览：

```bash
cd frontend/admin && pnpm install && pnpm dev   # 开发：http://localhost:5173（已代理到 :8080）
cd frontend/admin && pnpm build                 # 产物为纯静态 dist/，可同源部署或配合 CORS 跨源部署
```

详见 [frontend/admin/README.md](frontend/admin/README.md)。

## 参数

| 参数 | 默认 | 说明 |
|---|---|---|
| `user` | 必填 | 豆瓣用户 ID 或个人主页 URL |
| `--category` | movie book music | 类型 |
| `--status` | collect | collect=看过·读过·听过，do=在，wish=想 |
| `--format` | csv json | csv / json / md / xlsx |
| `--out` | output | 输出目录 |
| `--delay` | 2.0 | 翻页间隔秒数（**请勿调小**） |
| `--max-pages` | 无 | 每类别最多页数（测试用） |

## 特性

- 兼容两代页面结构：电影/音乐（`div.item.comment-item`）与图书（`li.subject-item`）
- `book.douban.com` 反爬较严，已自动携带浏览器级请求头 + Referer
- 403/418/5xx 自动指数退避重试；404 快速失败
- **断点续爬**：进度存于 `output/.{user}_{cat}_{status}.ckpt.json`，中断后重跑自动续传；完成后自动清理
- 按 `subject_id` 去重（主键冲突即忽略）


## 数据字段（Record 12 字段）

`category, status, title, alt_title, my_rating, date, comment, subject_url, subject_id, cover, comment_id, intro`

## 已知限制

- **豆瓣隐藏条目**：个别条目（已删除/被锁定/仅登录可见）豆瓣"计数包含但列表不渲染"，免登录抓取会差 1~2 条（如测试账号电影 208/209、音乐 28/29）——抓取器并未丢数据，是豆瓣不发给未登录访客。服务器可设 `SYNC_DOUBAN_COOKIE='dbcl2="xxx"'`（浏览器登录豆瓣后从开发者工具复制）以登录身份抓取补全
- 仅能获取用户**公开**的标记；设置了隐私的账号拿不到
- 页面结构变更时解析器需要更新（2026-10 实测有效）

## 仅供个人数据备份

请保持默认限速，尊重豆瓣服务条款；请勿用于大规模抓取。
