#!/usr/bin/env python3
"""受限滚动区里，kitty 会不会把图片跟着文本一起搬？

Miyu 的活动区固定在屏幕底部若干行，上面的正文区用 DECSTBM 受限滚动区
(`ESC[1;Nr`) 加换行来滚。轨迹显示全程只有这一种搬动方式，没有插入/删除
行。如果 kitty 在受限区里只搬文本、不搬图片，那正文一滚图就留在原地，
表现出来正是「滚上去看历史后新输出把内容顶上去就错位」。

画一个色块，每行右边贴一个编号标签。先停 2 秒让你看清「滚之前」，再在
受限区里滚 4 行。图和标签必须始终并排。

用法（必须在真实 kitty 窗口里，不要在 tmux 里）：

    python3 testkit/kitty-image/scroll_probe.py
"""
import base64
import fcntl
import os
import struct
import sys
import termios
import time

PLACEHOLDER = "\U0010eeee"
ROW_DIACRITICS = ["̅", "̍", "̎", "̐"]
IMAGE_ROWS = 4
IMAGE_COLS = 12
IMAGE_TOP = 4      # 图片起始行（0 基）
SCROLL_BY = 12


def out(text):
    sys.stdout.write(text)


def at(row, text=""):
    """定位到行首并清到行尾，避免旧内容从右边露出来。"""
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


def draw_image(top, cell_w, cell_h):
    width, height = IMAGE_COLS * cell_w, IMAGE_ROWS * cell_h
    raw = bytes(b"\x1e\x64\xc8\xff") * (width * height)
    encoded = base64.standard_b64encode(raw)
    chunks = [encoded[i:i + 4096] for i in range(0, len(encoded), 4096)]
    image_id = 0x001E64C8
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
    red, green, blue = 0x1E, 0x64, 0xC8
    for row in range(IMAGE_ROWS):
        at(top + row)
        out(f"\x1b[38;2;{red};{green};{blue}m")
        out(PLACEHOLDER + ROW_DIACRITICS[row] + ROW_DIACRITICS[0])
        out(PLACEHOLDER * (IMAGE_COLS - 1))
        out(f"\x1b[39m ←── 标签 {row + 1}（必须一直贴着蓝块）")
    sys.stdout.flush()


def main():
    if os.environ.get("TERM") != "xterm-kitty":
        print(f"TERM={os.environ.get('TERM')!r}，必须在真实 kitty 窗口里直接跑。")
        return 1
    cell_w, cell_h, rows, cols = cell_size()

    out("\x1b[2J\x1b[H")
    at(0, f"终端 {cols}×{rows}  单元格 {cell_w}×{cell_h}")
    at(1, f"蓝块在第 {IMAGE_TOP} 行占 {IMAGE_ROWS} 行；将在受限滚动区里逐行上滚 {SCROLL_BY} 次")
    at(2, "")
    for row in range(3, IMAGE_TOP):
        at(row, f"填充行 {row}")
    draw_image(IMAGE_TOP, cell_w, cell_h)
    region_bottom = max(rows - 6, IMAGE_TOP + IMAGE_ROWS + 3)
    for row in range(IMAGE_TOP + IMAGE_ROWS, region_bottom):
        at(row, f"填充行 {row}")
    # 底部 6 行当「活动区」，不参与滚动——和 Miyu 的布局一致。
    for row in range(region_bottom, rows):
        at(row, f"【活动区】第 {row} 行，这几行不该动")
    sys.stdout.flush()
    time.sleep(2.5)

    # 受限区滚动：正是 Miyu 活动区重绘用的那一套。真实会话里这个动作要
    # 发生几百次，一次只滚一行。滚一次看不出问题——残影要累积才显形，所
    # 以这里一行一行地滚，慢放让你看清每一步。
    for step in range(SCROLL_BY):
        out(f"\x1b[1;{region_bottom}r")
        out(f"\x1b[{region_bottom};1H")
        out("\n")
        out("\x1b[r")
        at(region_bottom + 1, f"\x1b[2m滚动中 {step + 1}/{SCROLL_BY}…\x1b[0m")
        sys.stdout.flush()
        time.sleep(0.45)

    # 判据写进「活动区」，那块没参与滚动，不会盖到图。
    at(region_bottom + 1, "\x1b[1m滚完了。蓝块本该一路上移、最后整个滚出屏幕。\x1b[0m")
    at(region_bottom + 2, "  屏幕上已经没有蓝色  → kitty 搬得干净，问题不在这里")
    at(region_bottom + 3, "  留下一条条蓝色残影  → 根因确认：受限区滚动不清图片")
    at(rows - 1, "")
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
