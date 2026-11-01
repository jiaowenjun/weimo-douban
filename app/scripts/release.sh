#!/usr/bin/env bash
# 发布桌面 App：构建（含 updater 签名产物）→ 拉取 Windows CI 产物 →
# 生成 latest.json → gh release 上传 → 同步 Gitee 国内镜像（源码双推 +
# 发行版附件 + app-latest 更新清单分支，大陆免梯下载/更新）。已装旧版的
# App 经启动自检/托盘「检查更新」自动升级（默认端点 Gitee 优先+GitHub
# 回退，见 src/main.rs UPDATER_ENDPOINTS）。
#
# 前置：
#   1. tauri.conf.json 的 version 已 bump 到目标版本
#   2. TAURI_SIGNING_PRIVATE_KEY 指向私钥（首次生成：
#      pnpm --dir app tauri signer generate -w ~/.tauri-keys/weimo-douban-updater.key --password "" --ci
#      私钥丢失则老版本无法升级到新版本，务必备份）
#   3. gh 已登录且有仓库发布权限；本机有 jq
#   4.（可选，发 Windows 版）目标版本已 push 到 main 且 app-windows
#      工作流（.github/workflows/app-windows.yml）已手动 dispatch 成功
#   5.（可选）Gitee 镜像：gitee remote 已配置 + ~/.gitee/token 或
#      GITEE_TOKEN 环境变量（私人令牌 https://gitee.com/profile/personal_access_tokens
#      勾 projects）。缺任一自动跳过镜像步骤，不影响 GitHub 发布
# 用法：TAURI_SIGNING_PRIVATE_KEY=~/.tauri-keys/weimo-douban-updater.key ./scripts/release.sh
#   仅补同步 Gitee（gh release edit 换详细日志后刷新版面；幂等，不构建）：
#       ./scripts/release.sh --gitee-only [v版本]   # 版本缺省读 tauri.conf.json
set -euo pipefail
cd "$(dirname "$0")/.."

REPO="jiaowenjun/weimo-douban"
VERSION=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' tauri.conf.json | head -1)

# ---- Gitee 国内镜像（源码双推 + 发行版附件，供大陆免梯克隆/下载） ----
GITEE_REPO="jiaowenjun/weimo-douban"
GITEE_API="https://gitee.com/api/v5/repos/$GITEE_REPO"

gitee_token() {
  if [ -n "${GITEE_TOKEN:-}" ]; then printf '%s' "$GITEE_TOKEN"; return 0; fi
  if [ -f "$HOME/.gitee/token" ]; then tr -d '[:space:]' < "$HOME/.gitee/token"; return 0; fi
  return 1
}

# 统一 JSON body 传令牌（GET 匿名即可）；非 2xx 时打印响应体再失败，
# 免得 Gitee 只报 400 却不说是哪个参数（建 release 必填 target_commitish
# 的坑就是这么趟出来的——tag 已存在也要求填）
gitee_api() { # $1=METHOD $2=path $3=JSON(可空)
  local resp code
  if [ -n "${3:-}" ]; then
    resp=$(curl -sS -w $'\n%{http_code}' -X "$1" "$GITEE_API$2" \
      -H "Content-Type: application/json" -d "$3")
  else
    resp=$(curl -sS -w $'\n%{http_code}' -X "$1" "$GITEE_API$2")
  fi
  code=$(printf '%s' "$resp" | tail -1)
  if [ "$code" -ge 200 ] && [ "$code" -lt 300 ]; then
    printf '%s' "$resp" | sed '$d'
    return 0
  fi
  echo "  !! Gitee API $1 $2 -> HTTP $code：$(printf '%s' "$resp" | sed '$d' | head -c 300)" >&2
  return 1
}

# 同步一个已存在于 GitHub 的 Release 到 Gitee：双推 main+tags → 建/更
# 发行版（标题与描述取自 gh，compare 链接改写成 gitee 同构路径）→ 附件
# 从 GitHub release 下载后逐个上传（已在镜像的按文件名跳过）→ latest.json
# 改写下载直链后推 app-latest 孤儿分支（updater 的 Gitee 端点数据源）。
# 幂等，gh release edit 换详细日志后补跑即刷新版面与清单。
sync_gitee() { # $1=tag（如 v0.5.0）
  local tag="$1" token id title body names tmp f base
  if ! token=$(gitee_token); then
    echo "  !! 缺 Gitee 令牌（GITEE_TOKEN 或 ~/.gitee/token，gitee.com/profile/personal_access_tokens 勾 projects），跳过镜像同步"
    echo "     配置后补跑：./scripts/release.sh --gitee-only $tag"
    return 0
  fi
  git remote get-url gitee >/dev/null 2>&1 || {
    echo "  !! 缺 gitee remote（git remote add gitee git@gitee.com:$GITEE_REPO.git），跳过镜像同步"
    return 0
  }

  echo "==> Gitee 镜像：双推 main + tags"
  git fetch origin --tags   # 拿回 gh release create 刚在远端建的 tag
  if [ "$(git rev-parse main)" = "$(git rev-parse origin/main)" ]; then
    git push gitee main
    git push gitee --tags
  else
    echo "  !! 本地 main 与 origin/main 不一致（发版前应先推 GitHub），跳过双推；推送后补跑 --gitee-only $tag"
  fi

  echo "==> Gitee 镜像：发行版 $tag"
  id=$(gitee_api GET "/releases/tags/$tag" | jq -r '.id // empty')
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/gitee-sync.XXXXXX")
  gh release download "$tag" --repo "$REPO" --dir "$tmp"
  title=$(gh release view "$tag" --repo "$REPO" --json name --jq .name)
  body=$(gh release view "$tag" --repo "$REPO" --json body --jq .body \
    | sed "s|https://github.com/$REPO/compare|https://gitee.com/$GITEE_REPO/compare|g")
  if [ -n "$id" ]; then
    gitee_api PATCH "/releases/$id" "$(jq -cn --arg t "$token" --arg tag "$tag" \
      --arg n "$title" --arg b "$body" \
      '{access_token:$t, tag_name:$tag, name:$n, body:$b, target_commitish:"main"}')" >/dev/null
    names=$(gitee_api GET "/releases/tags/$tag" | jq -r '.assets[].name')
  else
    id=$(gitee_api POST "/releases" "$(jq -cn --arg t "$token" --arg tag "$tag" \
      --arg n "$title" --arg b "$body" \
      '{access_token:$t, tag_name:$tag, name:$n, body:$b, target_commitish:"main"}')" | jq -r '.id')
    names=""
  fi
  for f in "$tmp"/*; do
    [ -e "$f" ] || continue
    base=$(basename "$f")
    if printf '%s\n' "$names" | grep -qxF "$base"; then
      echo "  · $base 已在镜像（跳过）"
      continue
    fi
    curl -fsS -X POST "$GITEE_API/releases/$id/attach_files?access_token=$token" \
      -F "file=@$f" >/dev/null && echo "  + $base 已上传"
  done

  # app-latest 分支 = App updater Gitee 端点的数据源（raw 直链稳定无版本号，
  # 弥补 Gitee 没有 latest/download 别名的缺口；App 端 UPDATER_ENDPOINTS[0]
  # 指向它）：取本 release 的 latest.json，把下载直链改写成 Gitee 发行版后，
  # 以孤儿提交推到 origin/gitee 两端（plumbing 直构，不动工作树不染 main
  # 历史）。放在附件循环之后——清单指向的直链此时已可下载
  if [ -f "$tmp/latest.json" ]; then
    sed "s|https://github.com/$REPO/releases/download|https://gitee.com/$GITEE_REPO/releases/download|g" \
      "$tmp/latest.json" > "$tmp/latest-gitee.json"
    blob=$(git hash-object -w "$tmp/latest-gitee.json")
    tree=$(printf '100644 blob %s\tlatest.json\n' "$blob" | git mktree)
    commit=$(git commit-tree "$tree" -m "App updater manifest ${tag#v}")
    # 孤儿提交无父祖裔，远端分支不可能 fast-forward，必须 -f（该分支是
    # 脚本独占的「最新版指针」信道，无并发推送方，强推即正解）
    git push -f origin "$commit:refs/heads/app-latest"
    git push -f gitee "$commit:refs/heads/app-latest"
    echo "  · app-latest 清单分支已强推（v${tag#v}）"
  else
    echo "  !! 本 release 缺 latest.json 资产，跳过 app-latest 分支推送"
  fi
  rm -rf "$tmp"
  echo "==> Gitee 镜像完成：https://gitee.com/$GITEE_REPO/releases/tag/$tag"
}

# --gitee-only：仅同步既有 Release 到 Gitee，不构建、不要求签名私钥
if [ "${1:-}" = "--gitee-only" ]; then
  sync_gitee "${2:-v$VERSION}"
  exit 0
fi

: "${TAURI_SIGNING_PRIVATE_KEY:?需要设置 TAURI_SIGNING_PRIVATE_KEY（私钥路径或内容），密钥丢失将无法推送更新}"
BUNDLE=target/release/bundle
# 本地产物名随 productName（微墨·豆瓣，含 CJK）；上传前统一复制为 ASCII 名——
# gh release 上传会剥掉资产名里的非 ASCII 前缀（实测 CJK dmg 变成
# "_0.4.0_aarch64.dmg"），latest/download 直链与 updater URL 全部落空，
# 故 dmg/tgz/sig 三件全用 weimo-douban 前缀的稳定 ASCII 名
APP_TGZ="$BUNDLE/macos/微墨·豆瓣.app.tar.gz"
DMG_ASCII="$BUNDLE/dmg/weimo-douban_${VERSION}_aarch64.dmg"
TGZ_ASCII="$BUNDLE/macos/weimo-douban.app.tar.gz"

echo "==> 构建 v$VERSION"
pnpm tauri build

echo "==> 复制产物为 ASCII 资产名"
cp "$BUNDLE/dmg/微墨·豆瓣_${VERSION}_aarch64.dmg" "$DMG_ASCII"
cp "$APP_TGZ" "$TGZ_ASCII"
cp "$APP_TGZ.sig" "$TGZ_ASCII.sig"

# ---- Windows 产物：mac 无法交叉编译，由 app-windows 工作流出
# windows-latest 构建并存为 run artifact（exe 安装包 + exe.sig 更新
# 签名 + version.txt）。拉最新成功运行，版本一致才并入本次发布；
# 缺失或版本不符则仅发 macOS（Windows 条目留待下次发布补上）。
WIN_DIR="$BUNDLE/windows-ci"
WIN_PRESENT=0
RUN_ID=$(gh run list --repo "$REPO" --workflow app-windows.yml --status success --limit 1 --json databaseId --jq '.[0].databaseId // empty' 2>/dev/null || true)
if [ -n "$RUN_ID" ]; then
  rm -rf "$WIN_DIR" && mkdir -p "$WIN_DIR"
  gh run download "$RUN_ID" --repo "$REPO" --name windows-app --dir "$WIN_DIR" \
    || { echo "  !! 拉取 Windows 产物失败（run $RUN_ID）"; exit 1; }
  CI_VERSION=$(cat "$WIN_DIR/version.txt")
  if [ "$CI_VERSION" = "$VERSION" ]; then
    WIN_PRESENT=1
    echo "==> Windows 产物已就绪（run ${RUN_ID}，v${CI_VERSION}）"
  else
    echo "==> 跳过 Windows 产物：CI 版本 v$CI_VERSION 与本地 v$VERSION 不一致（push 后请重新 dispatch app-windows 工作流）"
  fi
else
  echo "==> 未找到 app-windows 成功运行，本次仅发布 macOS 产物"
fi

DATE=$(date -u +%Y-%m-%dT%H:%M:%SZ)
SIG=$(cat "$APP_TGZ.sig")
ASSET_URL="https://github.com/$REPO/releases/download/v$VERSION/weimo-douban.app.tar.gz"
WIN_SIG=""
WIN_URL=""
if [ "$WIN_PRESENT" = 1 ]; then
  WIN_SIG=$(cat "$WIN_DIR/weimo-douban_x64-setup.exe.sig")
  # Tauri v2 NSIS updater 直接指向 setup.exe 本体（对其 minisign 签名，
  # 无 v1 时代的 .nsis.zip 包装），URL 用 ASCII 资产名
  WIN_URL="https://github.com/$REPO/releases/download/v$VERSION/weimo-douban_${VERSION}_x64-setup.exe"
fi
python3 - "$ASSET_URL" "$VERSION" "$DATE" "$SIG" "$WIN_SIG" "$WIN_URL" << 'EOF'
import json, sys
url, version, date, sig, win_sig, win_url = sys.argv[1:7]
platforms = {"darwin-aarch64": {"signature": sig, "url": url}}
if win_sig:
    platforms["windows-x86_64"] = {"signature": win_sig, "url": win_url}
json.dump({
    "version": version,
    "notes": f"微墨·豆瓣桌面 App v{version}",
    "pub_date": date,
    "platforms": platforms,
}, open("latest.json", "w"), indent=2)
EOF

echo "==> 创建 GitHub Release v$VERSION"
ASSETS=("$DMG_ASCII" "$TGZ_ASCII" "$TGZ_ASCII.sig" latest.json)
if [ "$WIN_PRESENT" = 1 ]; then
  ASSETS+=(
    "$WIN_DIR/weimo-douban_${VERSION}_x64-setup.exe"
    "$WIN_DIR/weimo-douban_x64-setup.exe.sig"
  )
fi
# 注意：紧邻全角字符的版本变量必须写成 ${VERSION}——macOS 自带 bash 3.2
# 的多字节解析 bug 会把后续 CJK 字符的字节吞进变量名（unbound variable）
PLATFORMS="darwin-aarch64"
[ "$WIN_PRESENT" = 1 ] && PLATFORMS="$PLATFORMS / windows-x64"
gh release create "v$VERSION" "${ASSETS[@]}" \
  --title "微墨·豆瓣 App v$VERSION" \
  --notes "微墨·豆瓣桌面 App v${VERSION}（${PLATFORMS}）。已装旧版者经托盘「检查更新」或启动自检自动升级；新安装请下载 dmg（macOS）或 x64-setup.exe（Windows）。"
echo "==> 发布完成：v${VERSION}（latest.json 已随 release 上传，默认端点即可发现新版本）"

# Gitee 国内镜像：GitHub 已发布，此步失败不影响主流程，可随时补跑。
# 注意必须以子壳调用——函数直接放 || 右侧会令 set -e 在函数体内整体失效
# （v0.5.1 实录：push 被拒后仍打印「已推送」），子壳内失败即中止并如实
# 冒泡非零
( sync_gitee "v$VERSION" ) \
  || echo "  !! Gitee 镜像未完成；GitHub Release 不受影响，可补跑：./scripts/release.sh --gitee-only v$VERSION"
