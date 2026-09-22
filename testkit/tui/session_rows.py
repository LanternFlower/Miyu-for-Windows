#!/usr/bin/env python3
"""`/session` 列表每行是「模式 · 当前上下文 · 标题」（用户 09-18）：上下文只要当前
会话的数（12k 这种短写），不带模型窗口；标题就是会话名。

    cargo build
    python3 testkit/tui/session_rows.py
"""

import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

STUB = {"STUB_CHUNK_SLEEP": "0.01"}
# 全屏面板的行长这样:「┃ › * 普通 · 129 · 标题」(› 是光标,* 是当前会话)
ROW = re.compile(r"^\s*┃?\s*›?\s*(\*?)\s*(普通|开发) · (\d+(?:\.\d+)?[kM]?) · (.+?)\s*$")


def send(master, sink, text, quiet=1.2, timeout=40):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=quiet, timeout=timeout)
    return h.render(bytes(sink))


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        send(master, sink, "第一句走查")
        os.write(master, b"/session\r")
        screen = r.wait_screen(master, sink, lambda s: any("Select session" in l or "选择会话" in l for l in s), 15.0)
        h.settle(master, sink, quiet=0.6, timeout=5.0)
        screen = h.render(bytes(sink))
        r.save("session-rows", screen)
        rows = [ROW.match(l) for l in screen]
        rows = [m for m in rows if m]
        report["rows_have_mode_context_title"] = bool(rows)
        # 当前会话(标 *)的上下文是个正数,不带 /168k 那种窗口
        m = next((m for m in rows if m.group(1) == "*"), None)
        report["current_row_context_positive"] = bool(m) and m.group(3) not in ("0",)
        report["no_window_in_rows"] = all("/" not in m.group(3) for m in rows)
        # 没聊过的会话(终端集成会话)不该挂着一万多的空会话估算
        idle = [m for m in rows if "终端集成会话" in m.group(4)]
        report["idle_session_shows_zero"] = all(m.group(3) == "0" for m in idle)
        print(json.dumps({"rows": [m.group(0).strip() for m in rows][:5]}, ensure_ascii=False))
        os.write(master, b"\x1b")
    finally:
        r.stop(tui, daemon, stub)
    for key, ok in report.items():
        print(f"{'✅' if ok else '❌'} {key}")
    passed = sum(1 for v in report.values() if v)
    print(f"\n{passed}/{len(report)} passed")
    sys.exit(0 if passed == len(report) else 1)


if __name__ == "__main__":
    main()
