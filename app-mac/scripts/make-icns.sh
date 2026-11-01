#!/usr/bin/env bash
# 由 icon.svg 再生 icon.icns：rsvg-convert 渲染 1024 母版 → sips 出全套
# 尺寸 → iconutil 打包。产物 icon.icns 由 build-app.sh 拷入 bundle。
set -euo pipefail
cd "$(dirname "$0")/../icons"

rsvg-convert -w 1024 -h 1024 icon.svg -o icon-1024.png

rm -rf icon.iconset && mkdir icon.iconset
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" icon-1024.png --out "icon.iconset/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) icon-1024.png --out "icon.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns icon.iconset -o icon.icns
rm -rf icon.iconset icon-1024.png
echo "✓ icons/icon.icns 已再生"
