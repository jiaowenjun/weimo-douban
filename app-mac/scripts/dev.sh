#!/usr/bin/env bash
# 开发直跑：debug 构建 + 直接执行 Swift 二进制（免组装 bundle）。
# 无 bundle id ⇒ 通知降级为日志、单实例判重与开机自启跳过，其余全功能。
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --manifest-path server/Cargo.toml
swift build

export WEIMO_MAC_SERVER_BIN="$PWD/server/target/debug/weimo-douban-mac-server"
exec .build/debug/WeimoDoubanMac
