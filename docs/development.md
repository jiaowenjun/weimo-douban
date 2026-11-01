# 开发与自行部署

面向使用者的介绍、下载和使用场景见[项目首页](../README.md)。本页保留本地开发与服务端接入入口。

## 同步服务器

在仓库根目录运行：

```bash
cargo build --release --manifest-path backend/Cargo.toml
SYNC_SERVER_BIND=127.0.0.1:8080 \
SYNC_SERVER_DATA_DIR=./backend/data \
SYNC_SERVER_TOKEN=replace-with-your-token \
./backend/target/release/weimo-douban-server
```

服务器提供异步同步任务、标记查询、订阅更新和后台清理。参数与环境变量以 [ServerConfig](../backend/src/server/config.rs) 为准；接口说明见[服务器 API 设计](design/服务器API设计.md)。对外部署参考[部署手册](../ops/txy部署手册.md)。

创建同步任务示例：

```bash
curl -X POST http://127.0.0.1:8080/api/v1/syncs \
  -H 'Authorization: Bearer replace-with-your-token' \
  -H 'Content-Type: application/json' \
  -d '{"user":"你的豆瓣ID","categories":["movie","book","music"]}'
```

使用返回的任务 ID 查询进度，完成后读取标记：

```bash
curl -H 'Authorization: Bearer replace-with-your-token' \
  'http://127.0.0.1:8080/api/v1/syncs/任务ID'

curl -H 'Authorization: Bearer replace-with-your-token' \
  'http://127.0.0.1:8080/api/v1/users/你的豆瓣ID/marks?category=movie&status=collect&page=1&page_size=50'
```

请保留默认访问间隔，用于个人记录备份，避免高频或大规模采集。数据可用性受豆瓣公开页面、隐私设置和访问限制影响。

## 桌面 App 与前端

| 项目 | 开发说明 |
|---|---|
| macOS / Windows 桌面 App | [构建、配置与发布](../app/README.md) |
| 原生 macOS 客户端 | [原生壳开发说明](../app-mac/README.md)，独立实验版本，未包含在当前公开安装包中 |
| 公开浏览界面 | [前端开发](../frontend/web/README.md) |
| 管理后台 | [管理后台开发](../frontend/admin/README.md) |
| 本地 MCP | [工具参数、构建与测试](../mcp/README.md) |

桌面 App 的构建会同时打包独立 MCP 程序。Agent 按需启动它，通过 stdio 只读访问现有数据库，不启动另一套同步任务。

## 设计资料

设计与方案（[design/](design/)）：

- [服务器技术选型](design/服务器技术选型.md)
- [服务器 API 设计](design/服务器API设计.md)
- [缓存与同步策略](design/缓存与同步策略.md)
- [条目元信息富集](design/条目元信息富集方案.md)
- [中心详情缓存](design/中心条目缓存接入方案.md)
- [mac 原生迁移路线图](design/tauri_app_mac_原生渐进式迁移路线图.md)

豆瓣源调研与分析（[research/](research/)）：

- [豆瓣 API 调研](research/豆瓣API调研报告.md)
- [豆瓣源依赖性与限制](research/豆瓣源依赖性与脆弱性分析.md)
