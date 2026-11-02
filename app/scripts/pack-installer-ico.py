#!/usr/bin/env python3
"""从 icons/icon-windows.svg 生成 icons/icon-installer.ico（NSIS 安装器/卸载器专用）。

为什么需要单独一个 ico（v0.5.3/0.5.4 实测踩坑）：
  1. tauri-bundler 的 NSIS 模板只有显式配置 bundle > windows > nsis >
     installerIcon 时才向 makensis 传 MUI_ICON，不配则安装器本体图标
     = NSIS 内置默认圆徽章（与 app 图标无关）；
  2. makensis 对 PNG 压缩条目 ico 解析有坑（NSIS 社区已知问题，
     Vista 风格 256px PNG 条目会导致图标显示异常），故本 ico 所有条目
     一律 BMP（非压缩 DIB）编码——与 icons/icon.ico（PNG 条目，供 exe
     资源嵌入，embed-resource/rc 路径无此问题）刻意不同，勿互覆。

依赖：rsvg-convert（brew install librsvg）、Pillow（pip install pillow）。
用法：python3 app/scripts/pack-installer-ico.py（路径按脚本自身定位，任意 cwd 均可）
"""
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

SIZES = (16, 24, 32, 48, 64, 256)
APP_DIR = Path(__file__).resolve().parent.parent
SRC_SVG = APP_DIR / "icons" / "icon-windows.svg"
OUT_ICO = APP_DIR / "icons" / "icon-installer.ico"


def render_png(size: int) -> bytes:
    """rsvg-convert 渲染指定尺寸的 RGBA PNG（源稿自带 22.5% 圆角裁切）。"""
    proc = subprocess.run(
        ["rsvg-convert", "-w", str(size), "-h", str(size), str(SRC_SVG)],
        capture_output=True,
        check=True,
    )
    return proc.stdout


def dib_entry(rgba) -> bytes:
    """把 RGBA 图打包成单个 BMP（非压缩 DIB）ico 条目。

    布局：BITMAPINFOHEADER(40B, biHeight=2h) + 自底向上 BGRA 像素 + 1bpp
    AND 掩码（行按 4 字节对齐；alpha==0 处置 1，兼容只看掩码的老消费方）。
    """
    from PIL import Image  # noqa: PLC0415 仅此函数需要，缺依赖时报错更聚焦

    if isinstance(rgba, bytes):
        from io import BytesIO

        im = Image.open(BytesIO(rgba)).convert("RGBA")
    else:
        im = rgba.convert("RGBA")
    w, h = im.size
    pixels = im.load()

    xor = bytearray()
    for y in range(h - 1, -1, -1):  # 自底向上
        for x in range(w):
            r, g, b, a = pixels[x, y]
            xor += bytes((b, g, r, a))

    row_bytes = (w + 7) // 8
    row_pad = (-row_bytes) % 4
    mask = bytearray()
    for y in range(h - 1, -1, -1):
        row = bytearray(row_bytes)
        for x in range(w):
            if pixels[x, y][3] == 0:
                row[x // 8] |= 0x80 >> (x % 8)
        mask += row + b"\x00" * row_pad

    header = struct.pack(
        "<IiiHHIIiiII",
        40, w, h * 2, 1, 32, 0, len(xor) + len(mask), 0, 0, 0, 0
    )
    return bytes(header + xor + mask)


def main() -> None:
    try:
        import PIL  # noqa: F401
    except ImportError:
        sys.exit("缺 Pillow：python3 -m pip install pillow")

    entries = [dib_entry(render_png(s)) for s in SIZES]

    out = bytearray()
    out += struct.pack("<HHH", 0, 1, len(entries))
    offset = 6 + 16 * len(entries)
    for size, blob in zip(SIZES, entries):
        dim = 0 if size == 256 else size  # 256 以 0 编码
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
    for blob in entries:
        out += blob

    OUT_ICO.write_bytes(out)
    print(f"已生成 {OUT_ICO.relative_to(APP_DIR.parent)}：{len(out)}B，"
          f"条目 {'/'.join(map(str, SIZES))} 全 BMP 编码")


if __name__ == "__main__":
    main()
