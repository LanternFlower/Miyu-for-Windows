#!/usr/bin/env python3
"""Kitty 图片占位诊断：在真实 kitty 里跑，回答两个问题。

  1. 终端上报的单元格像素尺寸是多少？Miyu 用它算网格，取不到就退回
     (10, 20)——退错了整张图的宽高比就是错的。
  2. 图片实际画出来占几行？Miyu 声明 r=N，如果 kitty 画得比 N 行高，
     后面的文本就会被压在图上（截图里看到的正是这个）。

用法（必须在真实 kitty 窗口里，不要在 tmux 里）：

    python3 testkit/kitty-image/probe.py

看输出里的 8 行标尺：全部落在图**下方**就是对的；有任何一行叠在图上，
把叠了几行告诉我。
"""
import base64
import fcntl
import os
import struct
import sys
import termios

PLACEHOLDER = "\U0010eeee"
# Kitty 用这张表给占位符编行号/列号。
ROW_DIACRITICS = [
    "̅", "̍", "̎", "̐", "̒", "̽", "̾", "̿",
    "͆", "͊", "͋", "͌", "͐", "͑", "͒", "͗",
    "͛", "ͣ", "ͤ", "ͥ", "ͦ", "ͧ", "ͨ", "ͩ",
]


def winsize():
    try:
        packed = fcntl.ioctl(sys.stdout.fileno(), termios.TIOCGWINSZ, b"\0" * 8)
    except OSError as err:
        return None, str(err)
    rows, cols, xpixel, ypixel = struct.unpack("HHHH", packed)
    return (rows, cols, xpixel, ypixel), None


def solid_rgba(width, height, rgb):
    return bytes(bytes(rgb) + b"\xff") * (width * height)


def emit(image_id, cols, rows, cell_w, cell_h):
    """照 Miyu src/tools/kitty_image.rs::write_image 的写法逐字节复刻。"""
    width, height = cols * cell_w, rows * cell_h
    raw = solid_rgba(width, height, (220, 60, 60))
    encoded = base64.standard_b64encode(raw)
    chunk_size = 4096
    chunks = [encoded[i:i + chunk_size] for i in range(0, len(encoded), chunk_size)]
    out = []
    for index, chunk in enumerate(chunks):
        more = 1 if index + 1 < len(chunks) else 0
        if index == 0:
            out.append(
                f"\x1b_Gq=2,i={image_id},a=T,U=1,f=32,t=d,"
                f"s={width},v={height},c={cols},r={rows},m={more};"
            )
        else:
            out.append(f"\x1b_Gq=2,m={more};")
        out.append(chunk.decode("ascii"))
        out.append("\x1b\\")
    red, green, blue = (image_id >> 16) & 0xFF, (image_id >> 8) & 0xFF, image_id & 0xFF
    for row in range(rows):
        out.append(f"\x1b[38;2;{red};{green};{blue}m")
        out.append(PLACEHOLDER + ROW_DIACRITICS[row] + ROW_DIACRITICS[0])
        out.append(PLACEHOLDER * (cols - 1))
        out.append("\x1b[39m\n")
    sys.stdout.write("".join(out))
    sys.stdout.flush()


def main():
    term = os.environ.get("TERM", "")
    if term != "xterm-kitty":
        print(f"TERM={term!r}，不是真实 kitty 窗口。")
        print("这个探针必须在 kitty 里直接跑，不能在 tmux/screen 或管道里，")
        print("否则图形转义序列会被原样吐成几十 KB 乱码。")
        return 1
    size, err = winsize()
    print("=== 终端上报 ===")
    if err:
        print(f"  TIOCGWINSZ 失败: {err}")
        cell_w, cell_h = 10, 20
    else:
        rows, cols, xpixel, ypixel = size
        print(f"  {cols} 列 × {rows} 行，像素 {xpixel} × {ypixel}")
        if xpixel == 0 or ypixel == 0:
            print("  ⚠ 像素为 0 → Miyu 会退回假定的 (10, 20)，宽高比必然算错")
            cell_w, cell_h = 10, 20
        else:
            cell_w, cell_h = max(xpixel // cols, 1), max(ypixel // rows, 1)
            print(f"  推出单元格 {cell_w} × {cell_h} px（宽高比 1:{cell_h / cell_w:.2f}）")
            if not 1.6 <= cell_h / cell_w <= 3.0:
                print("  ⚠ 这个比例不寻常，fit_cells 的等比换算会失真")
    print(f"  TERM={os.environ.get('TERM')}  "
          f"KITTY_WINDOW_ID={os.environ.get('KITTY_WINDOW_ID')}")

    grid_cols, grid_rows = 20, 6
    print(f"\n=== 声明 c={grid_cols} r={grid_rows} 的红块，下面 8 行标尺 ===")
    emit(0x00C81E1E, grid_cols, grid_rows, cell_w, cell_h)
    for i in range(1, 9):
        print(f"标尺 {i} ← 这 8 行都该在红块下方")
    print("\n若有标尺叠在红块上，说明画出来比声明的 r 行高，告诉我叠了几行。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
