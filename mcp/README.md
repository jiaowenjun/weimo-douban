# MCP 连接与开发指南

微墨·豆瓣提供本地与远程两种 MCP 连接，共用五个只读查询工具。桌面 App 与网页版分别保存数据，请按希望查询的数据来源选择连接。

| 希望查询的数据 | 使用指南 |
|---|---|
| 电脑中由桌面 App 同步的备份 | [本地 MCP 使用指南](local-guide.md)：macOS／Windows 配置、首次查询、常用场景与排错 |
| 服务器上由网页版同步并开放的记录 | [远程 MCP 使用指南](remote-guide.md)：访问凭证、HTTP 连接、账号权限与排错 |

## 本地连接（stdio）

安装微墨·豆瓣 v0.5.2 或更新版本并完成同步后，可以连接支持 stdio 的 AI 助手。助手在本机启动 App 附带的 MCP 程序，App 完全退出后仍可查询已有备份。完整步骤见[本地 MCP 使用指南](local-guide.md)。

## 生成连接配置

运行安装包附带程序的 `--print-config` 可得到实际路径配置。macOS、Windows 命令以及如何添加到助手，见[本地指南：生成连接配置](local-guide.md#生成连接配置)。

## 远程连接（无需安装 App）

远程 MCP 当前为邀请制，使用独立访问凭证查询服务器已同步并开放的账号。接入步骤、可复制配置、首次查询、常用场景与排错方法见[远程 MCP 使用指南](remote-guide.md)。

## 工具

两种连接都提供 `list_users`、`search_marks`、`get_subject`、`get_stats`、`list_facets`。先通过 `list_users` 选择账号，再使用返回的 `uid` 查询。工具用途和参数示例分别收录在[本地指南](local-guide.md#第一次查询)与[远程指南](remote-guide.md#第一次查询)中。

## 查询失败时

请先展开工具调用详情，查看实际参数与原始错误。参数缺失、空对象回填以及 ZCode + GLM-5.3 的案例，见 [AI 查询问题排查](troubleshooting.md)。

## 本地连接的只读边界与生命周期

以下内容供开发与自行构建时参考。MCP 使用官方 `rmcp 3.8.0`，查询实现由本地 stdio 与可选的 HTTP 二进制共享；远程服务的部署与范围控制见[运维说明](../ops/mcp/README.md)。

本地数据库连接启用 SQLite `read_only` 和 `query_only`，不执行 bootstrap、迁移、导出或任意 SQL。WAL 的辅助文件仍可能由 SQLite 为协调读取而创建；这是数据库协调行为，不是业务数据写入。查询使用短读事务，提交后才组装结果，不跨 Agent 调用持有事务。

本地数据目录优先级为 `--data-dir` → `SYNC_SERVER_DATA_DIR` → 系统应用数据目录下的 `ink.weimo.douban`。数据库不存在或结构不兼容时，程序报错退出，不自动创建或迁移；`--print-config` 只输出配置，不打开数据库或修改客户端设置。

stdio 不监听网络；日志走 stderr，stdout 只用于 MCP 协议，`--help`、`--version`、`--print-config` 是独立命令模式。关闭 stdin 后进程退出。不启动抓取、订阅或后台 worker；未来同步命令应交给 App 的唯一 worker 执行。

## 查询实现约定

- 结果同时提供 `structuredContent` 和 JSON 文本。查询只使用已发布的 records，刷新中仍可读上次快照；首次同步 running 状态可读已入库部分。`sync` 返回各维度状态和上次完成同步时间。
- 元信息沿用 App 的 rexxar 优先、intro 解析回退，`metadata_source`／`source` 表明来源。返回豆瓣条目链接，省略需要本地 HTTP 服务的封面代理。
- 每次查询最多读取 50,000 条经基础条件筛选的候选标记；超限会要求缩小范围，不会静默截断统计。工具设 15 秒超时；数据库忙时返回可重试错误。
- 各页使用各自查询时的数据库快照，同步期间列表可能变化。`query` 按字面子串搜索，ASCII 字母不区分大小写。

## 构建与验证

在仓库根目录运行：

```bash
cargo build --locked --release --manifest-path mcp/Cargo.toml --bin weimo-douban-mcp
cargo test --locked --manifest-path backend/Cargo.toml --test reader_tests
cargo test --locked --manifest-path mcp/Cargo.toml
```

Tauri 的 `beforeBuildCommand`／`beforeDevCommand` 自动编译 MCP，生成目标平台后缀的 sidecar，`externalBin` 将其放入安装包；开发阶段不自动启动 MCP。Windows 构建复用现有 workflow。原生 macOS 的 `app-mac/scripts/build-app.sh` 编译并复制 MCP 到 Resources；该实验版本独立于当前公开安装包。

自行构建的程序可显式选择开发数据目录并生成配置：

```bash
./mcp/target/release/weimo-douban-mcp --data-dir ./backend/data --print-config
```

原生 macOS 实验包完成构建后，在仓库根目录运行：

```bash
"app-mac/build/WeimoDoubanNative.app/Contents/Resources/weimo-douban-mcp" --print-config
```

`reader_tests` 覆盖拒绝写入／迁移、数据库缺失、WAL 并发、筛选、分页、统计、元信息回退和账号范围；MCP 测试通过真实子进程验证工具发现与调用、旧协议兼容、stdout 洁净、EOF 退出、配置输出。HTTP 构建、测试与部署步骤见[远程 MCP 运维说明](../ops/mcp/README.md)，桌面包只包含 stdio 程序。具体 AI 客户端需要分别验证连接。
