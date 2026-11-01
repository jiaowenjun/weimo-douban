#!/usr/bin/env bash
# 组装 .app：前端 dist → Rust server（release）→ Swift（release）→ bundle → ad-hoc 签名
set -euo pipefail
cd "$(dirname "$0")/.."

# 1) 前端 dist（server 的 rust-embed 编译期依赖；构建产物与仓库其余部分零耦合）
pnpm --dir ../frontend/web build

# 2) 无头服务器（release）
cargo build --release --manifest-path server/Cargo.toml

# 3) Swift 原生壳（release）
swift build -c release

# 4) 组装 bundle（图标复用 Tauri 版资产，只读不改）
APP="build/WeimoDoubanNative.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp .build/release/WeimoDoubanMac "$APP/Contents/MacOS/WeimoDoubanMac"
cp server/target/release/weimo-douban-mac-server "$APP/Contents/Resources/weimo-douban-mac-server"
cp ../app/icons/icon.icns "$APP/Contents/Resources/icon.icns"
cp Info.plist "$APP/Contents/Info.plist"

# 5) ad-hoc 签名（本机自用；分发需 Developer ID 签名 + 公证，同 Tauri 版口径）
codesign --force --sign - "$APP"

echo "✓ $APP"
echo "  open $PWD/$APP"
