#!/usr/bin/env python3
"""读 relay.py 录下来的字节，回答「图被谁擦了」。

按时间顺序列出关键事件：sixel 图进来、光标查询发出去、应答回来（以及花了多久）、
清屏/清行、滚动区设置、光标绝对跳转。图被擦的话，擦它的那条序列就在 sixel 之后。
"""
import re, sys, os

OUT = sys.argv[1] if len(sys.argv) > 1 else "relay-out.log"
IN = sys.argv[2] if len(sys.argv) > 2 else "relay-in.log"

REC = re.compile("\n--- (程序→终端|终端→程序|自动注入) \\+([\\d.]+)s (\\d+)B ---\n".encode("utf-8"))


def records(path):
    if not os.path.exists(path):
        return []
    blob = open(path, "rb").read()
    marks = list(REC.finditer(blob))
    out = []
    for index, mark in enumerate(marks):
        end = marks[index + 1].start() if index + 1 < len(marks) else len(blob)
        out.append((float(mark.group(2)), mark.group(1).decode(), blob[mark.end():end]))
    return out


EVENTS = [
    (rb'\x1bP[0-9;]*q"(\d+);(\d+);(\d+);(\d+)', lambda m: f"■ sixel 图 {int(m.group(3))}x{int(m.group(4))}px"),
    (rb"\x1b_G[^;]*;", lambda m: "■ kitty 图形协议"),
    (rb"\x1b\[6n", lambda m: "? 问光标位置 (CPR)"),
    (rb"\x1b\[(\d+);(\d+)R", lambda m: f"< 光标应答 行{m.group(1).decode()} 列{m.group(2).decode()}"),
    (rb"\x1b\[2J", lambda m: "!! 清整屏"),
    (rb"\x1b\[3J", lambda m: "!! 清 scrollback"),
    (rb"\x1b\[(\d*)J", lambda m: f"!! 清屏幕剩余 (ED{m.group(1).decode() or '0'})"),
    (rb"\x1b\[(\d*)K", lambda m: f"× 清行 (EL{m.group(1).decode() or '0'})"),
    (rb"\x1b\[(\d+);(\d+)r", lambda m: f"□ 滚动区 {m.group(1).decode()}..{m.group(2).decode()}"),
    (rb"\x1b\[r", lambda m: "□ 滚动区复位"),
    (rb"\x1b\[(\d+);(\d+)H", lambda m: f"→ 跳到 行{m.group(1).decode()} 列{m.group(2).decode()}"),
    (rb"\x1b\[(\d+)L", lambda m: f"+ 插入 {m.group(1).decode()} 行"),
    (rb"\x1b\[(\d+)M", lambda m: f"- 删除 {m.group(1).decode()} 行"),
    (rb"\x1b\[\?2026h", lambda m: "( 同步更新开始"),
    (rb"\x1b\[\?2026l", lambda m: ") 同步更新结束"),
]

merged = sorted(records(OUT) + records(IN), key=lambda row: row[0])
print(f"{'时刻':>9}  {'方向':<10} 事件")
print("-" * 78)
last_query = None
for when, direction, data in merged:
    hits = []
    for pattern, render in EVENTS:
        for match in re.finditer(pattern, data):
            hits.append((match.start(), render(match)))
    if not hits:
        continue
    for _, text in sorted(hits):
        extra = ""
        if text.startswith("?"):
            last_query = when
        elif text.startswith("<") and last_query is not None:
            extra = f"   （等了 {when - last_query:.3f}s）"
            last_query = None
        print(f"{when:>8.3f}s  {direction:<10} {text}{extra}")

print()
print("读法：sixel 那一行之后，如果紧跟着「清屏/清行/跳到某行」，图就是被它擦的。")
print("      CPR 问了却没有应答行，说明 crossterm 等超时了 —— Miyu 会退回旧光标位置，")
print("      于是把输入框画回图所在的地方。")
