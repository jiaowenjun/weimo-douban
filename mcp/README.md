# 本地 Agent 接入（stdio MCP）

`weimo-douban-mcp` 是随桌面 App 分发的独立 Rust 可执行文件，使用官方 `rmcp 3.8.0`。Agent 负责启动它，通过 stdin/stdout 调用工具，不需要 Node.js、Python、HTTP 端口或运行中的 App。它读取 App 最后同步的数据，不启动抓取、订阅或后台 worker。

## 生成连接配置

完成 App 安装并至少同步一次数据后，运行附带程序的 `--print-config`，得到包含实际可执行文件路径和数据目录的 JSON：

```bash
# Tauri macOS 安装在 /Applications 时
"/Applications/微墨·豆瓣.app/Contents/MacOS/weimo-douban-mcp" --print-config

# 原生 macOS 开发包（从仓库根目录运行）
"app-mac/build/WeimoDoubanNative.app/Contents/Resources/weimo-douban-mcp" --print-config

# 自行指定数据目录，例如开发服务器的数据库
./mcp/target/release/weimo-douban-mcp --data-dir ./backend/data --print-config
```

Windows：在安装目录中找到 `weimo-douban-mcp.exe`，在 PowerShell 中用 `& '完整路径\weimo-douban-mcp.exe' --print-config` 调用。

输出格式如下，路径以命令实际输出为准：

```json
{
  "mcpServers": {
    "weimo-douban": {
      "command": "/实际安装位置/weimo-douban-mcp",
      "args": ["--data-dir", "/实际应用数据目录/ink.weimo.douban"]
    }
  }
}
```

将该条目合并到目标 Agent 的 MCP 设置中；有的客户端使用 TOML 或 UI 表单，只需填写同样的 command 和 args。不要覆盖已有服务器配置。程序仅输出配置，不自动修改 Agent 设置。

数据目录优先级：`--data-dir` → `SYNC_SERVER_DATA_DIR` → 系统应用数据目录下的 `ink.weimo.douban`。macOS 默认为 `~/Library/Application Support/ink.weimo.douban`；Windows 默认为 `%APPDATA%\ink.weimo.douban`。开发服务器请显式传目录，避免查错数据库。不支持在 JSON 路径里依赖 shell 展开 `~`。

数据库不存在或结构不兼容时，程序报错退出，不自动创建/迁移数据库；请用配套版本 App 完成初始化或升级。App 升级后重启 Agent 的 MCP 连接，使已运行的旧进程加载新程序。

## 工具

| 工具 | 参数和行为 |
|---|---|
| `list_users` | 分页列出本地用户、标记数、最近同步时间；先选择 uid |
| `search_marks` | 按 uid、类别、状态、关键词、用户星级、标记日期、作品年份、维度筛选标记及短评 |
| `get_subject` | 按 category + subject_id 获取作品详情，包含豆瓣评分和分类元信息 |
| `get_stats` | 按相同筛选条件计算类别/状态计数、用户星级分布、标记年份计数 |
| `list_facets` | 按相同筛选条件聚合指定维度，按计数降序分页 |

查询示例：“我 2026 年看过的五星电影”对应：

```json
{
  "uid": "从 list_users 获取",
  "category": "movie",
  "status": "collect",
  "my_rating_min": 5,
  "my_rating_max": 5,
  "marked_from": "2026-01-01",
  "marked_to": "2026-12-31",
  "limit": 20
}
```

- `category`：movie/book/music；`status`：collect/do/wish。省略时包括全部类别或状态。
- `my_rating_min/max` 是用户的 1–5 星评分，结果中的 `douban_rating` 是另一套公共评分。
- `marked_from/to` 是标记日期；`release_year_min/max` 是作品上映/出版/发行年份。上下限均包含边界，未评分/未知年份不匹配对应范围筛选。
- `query` 对标题、别名、短评做字面子串搜索；ASCII 字母不区分大小写，最多 200 字符。
- `facets` 形如 `[{"name":"directors","value":"王家卫"}]`，多个条件为 AND；支持 genres/countries/directors/press/authors/translators/singers。指作品元信息维度，不是用户自定义标签。
- 分页默认为 `limit=20, offset=0`，limit 上限 100；继续读取返回的 `next_offset`。各页使用各自查询时的数据库快照，同步期间列表可能变化。
- 结果同时提供 `structuredContent` 和 JSON 文本。查询只使用已发布的 records，刷新中仍可读上次快照；首次同步 running 状态可读已入库部分。`sync` 返回各维度状态、上次完成同步时间，不能将 running 数据当作完整结果。
- 元信息沿用 App 的 rexxar 优先、intro 解析回退，`metadata_source`/`source` 表明来源。返回豆瓣条目链接，省略需要本地 HTTP 服务的封面代理。
- 每次查询最多读取 50,000 条经基础条件筛选的候选标记；超限会要求缩小范围，不会返回静默截断的统计。工具设 15 秒超时；数据库忙时返回可重试错误。详情或评论仍可能较长，必要时缩小页数。

## 只读边界与生命周期

数据库连接启用 SQLite `read_only` 和 `query_only`，不执行 bootstrap、迁移、导出或任意 SQL。WAL 的辅助文件仍可能由 SQLite 为协调读取而创建；这是数据库协调行为，不是业务数据写入。查询使用短读事务，提交后才组装结果，不跨 Agent 调用持有事务。

该 Agent 能读取指定数据库内各用户的已同步书影音和短评，数据如何进入模型上下文取决于 Agent 设置。stdio 不监听网络；日志走 stderr，stdout 只用于 MCP 协议（`--help/--version/--print-config` 是独立命令模式）。关闭 stdin 后进程退出。

当前不提供触发同步、删除或修改数据的工具；未来同步命令应交给 App 的唯一 worker 执行。

## 构建与验证

```bash
cargo build --locked --release --manifest-path mcp/Cargo.toml
cargo test --manifest-path backend/Cargo.toml --test reader_tests
cargo test --locked --manifest-path mcp/Cargo.toml
```

Tauri 的 `beforeBuildCommand`/`beforeDevCommand` 自动编译 MCP，生成目标平台后缀的 sidecar，`externalBin` 将其放入安装包；开发阶段不自动启动 MCP。Windows 构建复用现有 workflow。原生 macOS 的 `app-mac/scripts/build-app.sh` 编译并复制 MCP 到 Resources。

`reader_tests` 覆盖拒绝写入/迁移、数据库缺失、WAL 并发、筛选、分页、统计和元信息回退；MCP 测试通过真实子进程验证工具发现与调用、旧协议兼容、stdout 洁净、EOF 退出、配置输出。真实 Agent 配置需要在对应客户端完成。
