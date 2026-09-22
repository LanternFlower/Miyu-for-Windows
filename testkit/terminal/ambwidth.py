#!/usr/bin/env python3
"""量一下：这个终端把「歧义宽度」字符画成几列。

Miyu 量宽用 unicode-width，歧义字符一律算 1 列。但 East Asian Ambiguous 这类
（`┃` `·` `▄` `█` `●`…）在 CJK locale 下不少终端按 2 列画——输入框左边那根竖线、
footer 的分隔点、声波、logo 的方块全在这一类里，一旦按 2 列画，整行就会错位。

做法：打一个字符，用 ESC[6n 问终端光标落在第几列。差值就是它认的宽度。
不改任何东西，只读。

    python3 ambwidth.py
"""
import os
import re
import sys
import termios
import tty
import unicodedata

SAMPLES = [
    ("┃", "输入框左侧竖线"),
    ("·", "footer 分隔点"),
    ("▄", "footer 声波"),
    ("█", "启动 logo"),
    ("●", "模式圆点"),
    ("✦", "大厅星星(非歧义,做对照)"),
    ("你", "CJK(必为 2,做对照)"),
]


def cursor_column(fd):
    """问终端：光标现在在第几列（1 起）。"""
    os.write(fd, b"\x1b[6n")
    answer = b""
    while not answer.endswith(b"R"):
        chunk = os.read(fd, 32)
        if not chunk:
            break
        answer += chunk
    found = re.search(rb"\x1b\[(\d+);(\d+)R", answer)
    return int(found.group(2)) if found else None


def main():
    if not sys.stdin.isatty():
        print("要在真终端里跑（不能管道）", file=sys.stderr)
        return 2
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    out = sys.stdout.fileno()
    results = []
    try:
        tty.setraw(fd)
        for ch, note in SAMPLES:
            os.write(out, b"\r\x1b[2K")          # 回到行首、清掉这一行
            before = cursor_column(fd)
            os.write(out, ch.encode())
            after = cursor_column(fd)
            os.write(out, b"\r\x1b[2K")
            width = (after - before) if (before and after) else None
            results.append((ch, note, unicodedata.east_asian_width(ch), width))
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, saved)
        sys.stdout.write("\r\x1b[2K")

    print(f"TERM={os.environ.get('TERM')}  LANG={os.environ.get('LANG')}")
    print(f"{'字符':<4}{'类别':<5}{'实测列宽':<10}说明")
    ambiguous_wide = False
    for ch, note, eaw, width in results:
        mark = ""
        if eaw == "A" and width == 2:
            mark = "  ← 按双宽画"
            ambiguous_wide = True
        print(f"  {ch}   {eaw:<5}{str(width):<10}{note}{mark}")
    print()
    if ambiguous_wide:
        print("结论：这个终端把歧义字符按 2 列画——Miyu 按 1 列算，所以会错位。")
        print("      临时规避：MIYU_ASCII=1 miyu")
    else:
        print("结论：这个终端把歧义字符按 1 列画，和 Miyu 的假定一致，不是这个原因。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
