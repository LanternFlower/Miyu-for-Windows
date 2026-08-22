#!/usr/bin/env python3
"""四种滚动方式，哪一种不会在图片上留残影？

已确认：Miyu 活动区用 DECSTBM 受限滚动区 + 换行来滚正文，而 kitty 在这
种滚动下会给图片留下残影。真实会话里这个动作要发生几百次，残影堆起来就
是屏幕上那叠重复的切片。

这里把四种候选各跑一遍，每种都画一个蓝块、滚 12 次、然后停下来让你看。
蓝块本该整个滚出屏幕顶部；屏幕上还剩蓝色就是留了残影。

用法（必须在真实 kitty 窗口里，不要在 tmux 里）：

    python3 testkit/kitty-image/scroll_variants.py
"""
import base64
import fcntl
import os
import struct
import sys
import termios

PLACEHOLDER = "\U0010eeee"
ROW_DIACRITICS = ["̅", "̍", "̎", "̐"]
IMAGE_ROWS = 4
IMAGE_COLS = 12
IMAGE_TOP = 4
SCROLL_BY = 12

VARIANTS = [
    ("A", "受限区 + 换行（Miyu 现在的做法）"),
    ("B", "受限区 + ESC[S（滚动指令，不用换行）"),
    ("C", "受限区 + 换行，随后显式清掉新空行"),
    ("D", "整屏滚动，不设受限区"),
]


def out(text):
    sys.stdout.write(text)


def at(row, text=""):
    out(f"\x1b[{row + 1};1H\x1b[K{text}")


def cell_size():
    try:
        packed = fcntl.ioctl(sys.stdout.fileno(), termios.TIOCGWINSZ, b"\0" * 8)
    except OSError:
        return 10, 20, 24, 80
    rows, cols, xpixel, ypixel = struct.unpack("HHHH", packed)
    if not xpixel or not ypixel or not rows or not cols:
        return 10, 20, rows or 24, cols or 80
    return max(xpixel // cols, 1), max(ypixel // rows, 1), rows, cols


def draw_image(top, cell_w, cell_h, image_id):
    width, height = IMAGE_COLS * cell_w, IMAGE_ROWS * cell_h
    raw = bytes(b"\x1e\x64\xc8\xff") * (width * height)
    encoded = base64.standard_b64encode(raw)
    chunks = [encoded[i:i + 4096] for i in range(0, len(encoded), 4096)]
    at(top)
    for index, chunk in enumerate(chunks):
        more = 1 if index + 1 < len(chunks) else 0
        head = (
            f"\x1b_Gq=2,i={image_id},a=T,U=1,f=32,t=d,"
            f"s={width},v={height},c={IMAGE_COLS},r={IMAGE_ROWS},m={more};"
            if index == 0
            else f"\x1b_Gq=2,m={more};"
        )
        out(head + chunk.decode("ascii") + "\x1b\\")
    red, green, blue = (image_id >> 16) & 0xFF, (image_id >> 8) & 0xFF, image_id & 0xFF
    for row in range(IMAGE_ROWS):
        at(top + row)
        out(f"\x1b[38;2;{red};{green};{blue}m")
        for col in range(IMAGE_COLS):
            out(PLACEHOLDER + ROW_DIACRITICS[row] + ROW_DIACRITICS[col % 4])
        out(f"\x1b[39m ←── 蓝块第 {row + 1} 行")
    sys.stdout.flush()


def scroll_once(kind, region_bottom):
    if kind == "A":
        out(f"\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\n\x1b[r")
    elif kind == "B":
        out(f"\x1b[1;{region_bottom}r\x1b[S\x1b[r")
    elif kind == "C":
        out(f"\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\n")
        out(f"\x1b[{region_bottom};1H\x1b[2K\x1b[r")
    else:
        out(f"\x1b[{region_bottom};1H\n")


def run(kind, label, cell_w, cell_h, rows, cols, image_id):
    out("\x1b[2J\x1b[H")
    at(0, f"【{kind}】{label}")
    at(1, f"终端 {cols}×{rows} 单元格 {cell_w}×{cell_h}；蓝块滚 {SCROLL_BY} 次，本该整个滚出屏幕")
    at(2, "")
    at(3, "填充行 3")
    draw_image(IMAGE_TOP, cell_w, cell_h, image_id)
    region_bottom = max(rows - 6, IMAGE_TOP + IMAGE_ROWS + 3)
    for row in range(IMAGE_TOP + IMAGE_ROWS, region_bottom):
        at(row, f"填充行 {row}")
    for row in range(region_bottom, rows):
        at(row, f"【活动区】第 {row} 行")
    sys.stdout.flush()

    for _ in range(SCROLL_BY):
        scroll_once(kind, region_bottom)
    sys.stdout.flush()

    at(region_bottom + 1, f"\x1b[1m【{kind}】滚完了：屏幕上还看得到蓝色吗？\x1b[0m")
    at(region_bottom + 2, "  看不到 = 这种方式干净；还有蓝条 = 留了残影")
    at(region_bottom + 3, "  \x1b[2m（先往上滚看一眼也行，看完回车继续）\x1b[0m")
    at(rows - 1, "按回车继续 ▸ ")
    sys.stdout.flush()
    try:
        input()
    except EOFError:
        pass


def main():
    if os.environ.get("TERM") != "xterm-kitty":
        print(f"TERM={os.environ.get('TERM')!r}，必须在真实 kitty 窗口里直接跑。")
        return 1
    cell_w, cell_h, rows, cols = cell_size()
    for index, (kind, label) in enumerate(VARIANTS):
        run(kind, label, cell_w, cell_h, rows, cols, 0x001E64C8 + index * 0x010101)
    out("\x1b[2J\x1b[H")
    print("四种都跑完了。告诉我哪几种留了蓝色残影、哪几种干净：\n")
    for kind, label in VARIANTS:
        print(f"  {kind}. {label}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
