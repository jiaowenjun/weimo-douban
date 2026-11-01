# txy 远程 MCP 运维

远程 MCP 使用独立进程 `weimo-douban-mcp-http`，与 8020 同步后端共享只读 SQLite 数据；HTTPS 地址为 `https://weimo.ink/douban/mcp`，内部仅绑定 `127.0.0.1:8021`。它复用本地 MCP 的五个查询工具，不启动 worker、不迁移数据库、不抓取豆瓣。桌面包继续只包含 stdio 二进制。

## 开放策略

首期为邀请制：独立的高熵 Bearer 凭证 + 显式 UID 白名单，初始仅 `wenjun30`。**同一凭证的所有持有者都能读取整个白名单，包括短评**，不是按登录用户隔离；不能把新用户 UID 加入白名单理解为给该用户创建独享账号。需要各用户独立权限时，应先实现逐凭证 scope 或 OAuth，再扩大开放。

这不是 OAuth 服务。客户端必须支持 Streamable HTTP 和自定义 Authorization 请求头；只有 OAuth 登录入口、无法配置请求头的客户端暂不适用。不得复用公开网页内的 `VITE_API_TOKEN`，也不接受 URL 的 `?token=`。该功能不改变现有 REST API 的公开访问范围。

## 首次安装

先按[主手册 §7](../txy部署手册.md#7-日常更新流程)提交、推送、通过 bundle 快进服务器 checkout。仅构建本二进制，不重启同步后端，不发布新版 App：

```bash
cd /root/workspace/weimo-douban
nice -n 10 cargo build --locked --release --manifest-path mcp/Cargo.toml --features http --bin weimo-douban-mcp-http -j 1
id weimo-mcp >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin weimo-mcp
install -d -m 755 /usr/local/libexec
install -m 755 mcp/target/release/weimo-douban-mcp-http /usr/local/libexec/weimo-douban-mcp-http
install -d -m 700 /etc/weimo-douban
# 首次生成；已有文件时保留，禁止每次部署隐式轮换。
test -f /etc/weimo-douban/mcp.token || (umask 077; openssl rand -hex 32 > /etc/weimo-douban/mcp.token)
install -m 600 ops/mcp/mcp.env.example /etc/weimo-douban/mcp.env
install -m 644 ops/mcp/weimo-douban-mcp.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now weimo-douban-mcp
curl --fail http://127.0.0.1:8021/healthz
```

检查并按实际情况修改 `mcp.env` 的数据目录、UID、Host；缺少范围或凭证必须启动失败。不要把凭证打印到终端、提交到 Git 或写进公开前端配置。`LoadCredential` 让 root 600 凭证只向服务用户提供只读副本；服务重启后才加载新凭证。

服务用户需要对数据目录的遍历权限以及 DB、`-wal`、`-shm` 的读取权限（txy 当前为目录 755、文件 644）。业务数据库在 systemd 中只读；**不要设置 SQLite `immutable=1`**，它会忽略活跃 WAL。服务在同步后端之后启动，并随其停止/重启，确保写入进程已创建 WAL 辅助文件；启动暂时失败时 5 秒重试。不要为了只读服务给数据目录添加写权限。

## Nginx

先备份 `/etc/nginx/sites-enabled/weimo.conf`。安装两个片段：

```bash
install -m 644 ops/mcp/nginx-http.conf /etc/nginx/conf.d/weimo-douban-mcp.conf
install -m 644 ops/mcp/nginx-location.conf /etc/nginx/snippets/weimo-douban-mcp.conf
```

仅在 `listen 443 ssl` 的 server 中加入以下内容，两个路径都直接转发，不做重定向，不落入 SPA HTML：

```nginx
location = /douban/mcp {
    include /etc/nginx/snippets/weimo-douban-mcp.conf;
}
location = /douban/mcp/ {
    include /etc/nginx/snippets/weimo-douban-mcp.conf;
}
```

执行 `nginx -t` 成功后才 `systemctl reload nginx`。不开放 8021 的公网防火墙端口；`/healthz` 只用于本机检查。保留 Host、Authorization、MCP 协议头；禁用缓存、响应缓冲；访问日志不记录请求参数和凭证。仅查询功能不要求持久会话或反向请求。

## 限额与验证

- 应用层：全凭证共享 60 请求/分钟、突发 10；最多 2 个 HTTP 请求和 2 个实际工具查询。实际查询许可在调用方断开后仍由查询持有，直到结束或取消。
- 工具 15 秒超时；读取请求体最多 5 秒，处理响应最多 20 秒。请求最多 16 KiB，结构化结果最多 256 KiB，HTTP 响应最多 1 MiB；超限明确报错，不截断统计。
- Nginx：单 IP 60 请求/分钟、总计 120 请求/分钟，最多 8 个传输连接；未认证流量也受限。应用层限额与 Nginx 限额独立。429/503 需稍后重试，不应连续快速重放。
- systemd：非 root 用户、文件系统只读、384 MiB 内存、1 核 CPU 上限。日志只记录运维错误，不记录参数、短评或凭证。

本地验证：

```bash
cargo test --locked --manifest-path backend/Cargo.toml --test reader_tests
cargo test --locked --manifest-path mcp/Cargo.toml --features http
cargo test --locked --manifest-path mcp/Cargo.toml --test stdio
```

部署后至少验证：不带/错误凭证为 401；HTTPS 初始化和工具发现成功；五工具均能调用；多参数搜索正确；list_users 不包含未开放用户；未开放 uid 和作品被拒绝；错误 Host/Origin 被拒绝；带和不带尾斜杠均返回协议响应；原网站、同步后端和其他服务保持健康。不要用生产写入或同步任务测试 MCP。

官方 Rust MCP 客户端测试包含 initialize 生命周期和 2026-07-28 discovery；原始 HTTP 检查包含 2025-11-25。实际 AI 客户端支持度需分别验证，不把协议测试描述为 ZCode/Claude/Cursor 全部已验证。ZCode + GLM 参数解析问题仍见[排查指南](../../mcp/troubleshooting.md)，换 HTTP 不会自动修复模型参数解析。

## 更新、凭证轮换与回滚

更新时先保留旧二进制和配置到 `/var/backups/weimo-douban/`，构建成功后用临时文件 + `mv` 原子替换 `/usr/local/libexec/weimo-douban-mcp-http`，仅重启 `weimo-douban-mcp`。不要覆盖现有 `mcp.env`，除非有明确的配置变更。

轮换时生成新的临时凭证文件、保持 root 600，原子替换 `mcp.token` 后重启 MCP，并通过私密渠道更新客户端请求头。首期单凭证没有逐用户吊销能力，轮换影响全部使用者。

回滚：恢复旧二进制/配置并重启 MCP。首次上线需要撤回时，先从 Nginx 删除两个精确 location，`nginx -t` 后 reload，再 `systemctl disable --now weimo-douban-mcp`；仅服务停止会让保留的代理返回 502。数据库没有迁移，无需回滚或复制数据库。主后端和桌面 App 不受二进制回滚影响。
