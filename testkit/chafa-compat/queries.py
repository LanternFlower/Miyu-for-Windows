#!/usr/bin/env python3
"""chafa 到底发了哪些能力查询？谁把它们抑制掉了？

不应答任何查询，只记录 chafa 写到终端的控制序列，看参数怎么影响探测。
"""
import os, sys, re
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pty_probe as P

BASE = os.path.dirname(os.path.abspath(__file__))
IMG = P.ensure_image()

QUERY_NAMES = [
    (rb"\x1b\[c", "DA1"),
    (rb"\x1b\[>0?c", "DA2"),
    (rb"\x1b\[>q", "XTVERSION"),
    (rb"\x1b\[\?[12];1;0S", "sixel-geometry"),
    (rb"\x1b_Gi=\d+[^\x1b]*\x1b\\\\", "kitty-query"),
    (rb"\x1b\[16t", "cell-px"),
    (rb"\x1b\[14t", "win-px"),
    (rb"\x1b\[6n", "DSR-cursor"),
    (rb"\x1b\[\?25l", "hide-cursor"),
    (rb"\x1bP\+q", "XTGETTCAP"),
]

CASES = [
    ("裸调用", []),
    ("--polite on", ["--polite", "on"]),
    ("--probe-mode ctty", ["--probe-mode", "ctty"]),
    ("--probe-mode ctty --polite on --relative off  ← 线上现状",
     ["--probe-mode", "ctty", "--polite", "on", "--relative", "off"]),
    ("--probe-mode stdio", ["--probe-mode", "stdio"]),
    ("--probe on", ["--probe", "on"]),
]

os.environ["TERM"] = "xterm-256color"
for k in ("KITTY_WINDOW_ID", "KITTY_PID", "TERM_PROGRAM", "KITTY_INSTALLATION_DIR"):
    os.environ.pop(k, None)

# 关掉应答，纯观察 chafa 发什么
P.respond_orig = P.respond
P.respond = lambda chunk: b""

print(f"{'参数':<52} {'选中格式':<9} {'发出的查询'}")
print("-" * 110)
for label, extra in CASES:
    code, out = P.run(["chafa"] + extra + ["--size", "20x10", IMG],
                      stdin_mode="null", timeout=12)
    head = out[:4000]
    found = [name for pat, name in QUERY_NAMES if re.search(pat, head)]
    print(f"{label:<52} {P.classify(out):<9} {', '.join(found) if found else '(无)'}")
