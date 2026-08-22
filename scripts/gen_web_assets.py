#!/usr/bin/env python3
"""从 `pics/` 的原图生成 WebUI 用的显示尺寸副本。

## 为什么要有副本

浏览器给一张图分配的 GPU 纹理是按**像素数**算的，跟 CSS 里写多大无关。
`pics/miyu-logo.png` 是 1254×1254，WebUI 里却只在侧栏显示 38 px、登录页 64 px；
`pics/miyuwallpaper.png` 是 3344×1882，看板框最大 330×178 px。直接用原图等于
花 30 MiB 显存去画两个缩略图，二进制和每次 HTTP 传输还各多背 7.2 MiB。

原图**不覆盖**——README、终端演示、外部链接都在引用它们。副本单独放在
`web/assets/`，`src/web/mod.rs` 的 `include_bytes!` 指向副本，路由名和
content-type 都不变（所以格式必须还是 PNG）。

## 尺寸怎么定的

按「最大显示尺寸 × DPR 余量」：

| 资源 | 最大显示 | 副本 | 余量 |
|---|---|---|---|
| logo | 64 px | 256×256 | 4x DPR |
| wallpaper | 330×178 | 1280×720 | 3.9x 宽 |

再往下压画质就开始掉了；按实际显示尺寸测过 PSNR（见下），当前尺寸在
45 dB 以上，肉眼不可辨的线是 40 dB。

## 用法

    python3 scripts/gen_web_assets.py            # 生成
    python3 scripts/gen_web_assets.py --verify   # 只校验画质，不写文件

依赖 Pillow；`pngquant`、`oxipng` 有就用，没有就跳过（只影响文件大小，
不影响显存——显存只看像素数）。
"""
import argparse
import math
import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent

# (原图, 副本, 覆盖框, 实际显示尺寸们)
ASSETS = [
    ("pics/miyu-logo.png", "web/assets/miyu-logo.png", (256, 256), [(38, 38), (64, 64)]),
    (
        "pics/miyuwallpaper.png",
        "web/assets/miyuwallpaper.png",
        (1280, 720),
        [(330, 178), (660, 356)],
    ),
]

# 低于这条线就该换更大的副本尺寸了。40 dB 是常用的「肉眼不可辨」经验线。
MIN_PSNR = 40.0


def downscale(source, box):
    """等比缩到**覆盖**目标框。

    看板图在 CSS 里是 `object-fit: cover` 裁切显示的，等比缩放不改变构图；
    强行拉成 box 的比例会让裁切位置对不上。
    """
    image = Image.open(source).convert("RGBA")
    ratio = max(box[0] / image.width, box[1] / image.height)
    size = (round(image.width * ratio), round(image.height * ratio))
    return image.resize(size, Image.LANCZOS)


def compress(path):
    for tool, args in (
        ("pngquant", ["--force", "--quality", "70-95", "--output", str(path), str(path)]),
        ("oxipng", ["-o", "4", "-q", "--strip", "safe", str(path)]),
    ):
        if shutil.which(tool):
            subprocess.run([tool, *args], check=False)


def psnr(source, copy, box, background=(20, 22, 28)):
    """在**真实显示尺寸**下比副本和原图。

    比对必须合成到不透明背景上再算：logo 带 alpha，量化后是调色板透明度，
    直接按 RGB 比会把透明区域的差算进去，得出 15 dB 这种毫无意义的数字。
    """

    def flatten(path):
        image = Image.open(path).convert("RGBA").resize(box, Image.LANCZOS)
        canvas = Image.new("RGB", box, background)
        canvas.paste(image, (0, 0), image)
        return canvas.load()

    left, right = flatten(source), flatten(copy)
    squared_error = 0
    for y in range(box[1]):
        for x in range(box[0]):
            for a, b in zip(left[x, y], right[x, y]):
                squared_error += (a - b) ** 2
    mse = squared_error / (box[0] * box[1] * 3)
    return 10 * math.log10(255 * 255 / mse) if mse else float("inf")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", action="store_true", help="只校验画质，不写文件")
    args = parser.parse_args()

    failures = []
    for source, copy, box, displays in ASSETS:
        source_path, copy_path = ROOT / source, ROOT / copy
        if not args.verify:
            copy_path.parent.mkdir(parents=True, exist_ok=True)
            downscale(source_path, box).save(copy_path, optimize=True)
            compress(copy_path)
        if not copy_path.exists():
            failures.append(f"{copy} 不存在，先不带 --verify 跑一遍")
            continue

        original, small = Image.open(source_path), Image.open(copy_path)
        print(
            f"\n  {source} {original.width}×{original.height}"
            f"  {source_path.stat().st_size / 1048576:.2f} MiB"
            f"  纹理 {original.width * original.height * 4 / 1048576:.1f} MiB"
        )
        print(
            f"  → {copy} {small.width}×{small.height}"
            f"  {copy_path.stat().st_size / 1048576:.2f} MiB"
            f"  纹理 {small.width * small.height * 4 / 1048576:.1f} MiB"
        )
        for display in displays:
            value = psnr(source_path, copy_path, display)
            mark = "✓" if value >= MIN_PSNR else "✗"
            print(f"    {mark} 显示 {display[0]}×{display[1]}：PSNR {value:.1f} dB")
            if value < MIN_PSNR:
                failures.append(
                    f"{copy} 在 {display[0]}×{display[1]} 下只有 {value:.1f} dB"
                    f"（下限 {MIN_PSNR} dB）"
                )

    if failures:
        print("\n画质不达标：")
        for item in failures:
            print(f"  ✗ {item}")
        return 1
    print("\n全部达标。改完记得重新 cargo build——图是 include_bytes! 编进去的。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
