#!/usr/bin/env python3
"""`/undo` 之后画面要跟着变（用户 09-18）：撤掉的那一轮从屏上消失、footer 的上下文
读数当场变、输入框回填被撤的那句；而前面的轮和往上翻的历史原样留着（整段回放
会把历史丢掉，不行）。撤到空会话回大厅。

    cargo build
    python3 testkit/tui/undo_redraw.py
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
FIRST = "第一句走查"
SECOND = "第二句走查"


def context_reading(screen):
    for line in reversed(screen):
        if "┃" in line and "·" in line:
            m = re.search(r"(\d+(?:\.\d+)?k?)/(?:~)?\d+k", line)
            if m:
                return m.group(1)
    return None


def send(master, sink, text, quiet=1.2, timeout=40):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=quiet, timeout=timeout)
    return h.render(bytes(sink))


def body_rows(screen):
    """正文行：footer（含「·」的竖条行）往上数到输入框顶上那一行为止之前的部分。
    用户气泡也是竖条行，靠位置区分：输入框是屏底那一块连续的竖条行。"""
    end = len(screen)
    for i in range(len(screen) - 1, -1, -1):
        if screen[i].startswith("┃") and "·" in screen[i]:
            end = i
            break
    # 从 footer 往上跳过输入框那几行（连续的竖条行）。
    while end > 0 and screen[end - 1].startswith("┃"):
        end -= 1
    return screen[:end]


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        send(master, sink, FIRST)
        screen = send(master, sink, SECOND)
        r.save("undo-before", screen)
        before = body_rows(screen)
        report["both_turns_on_screen"] = any(FIRST in l for l in before) and any(SECOND in l for l in before)
        replies_before = sum(1 for l in before if "分块吐出来" in l)
        ctx_before = context_reading(screen)
        # /undo
        screen = send(master, sink, "/undo", quiet=1.0, timeout=30)
        r.save("undo-after", screen)
        after = body_rows(screen)
        replies_after = sum(1 for l in after if "分块吐出来" in l)
        report["replies_before_after"] = [replies_before, replies_after]
        report["second_turn_gone_from_body"] = not any(SECOND in l for l in after) and replies_after == replies_before - 1
        report["first_turn_still_there"] = any(FIRST in l for l in after) and any("分块吐出来" in l for l in after)
        report["undone_note_shown"] = any("已撤销消息数" in l or "undone messages" in l for l in screen)
        report["prompt_refilled_into_editor"] = any(line.startswith("┃") and SECOND in line for line in screen)
        ctx_after = context_reading(screen)
        report["context_reading_before_after"] = [ctx_before, ctx_after]
        report["context_reading_refreshed"] = ctx_before is not None and ctx_after is not None and ctx_after != ctx_before
        # 往上翻：第一轮的提问还翻得到（滚动历史没丢）。
        os.write(master, b"\x1b[5~")
        h.settle(master, sink, quiet=0.4, timeout=5)
        paged = h.render(bytes(sink))
        os.write(master, b"\x1b[6~")
        h.settle(master, sink, quiet=0.4, timeout=5)
        report["scrollback_kept"] = any(FIRST in l for l in paged) or any(FIRST in l for l in after)
        # 输入框里回填了被撤的那句：Ctrl+C 清空输入，再撤一次 → 空会话回大厅。
        os.write(master, b"\x03")
        h.settle(master, sink, quiet=0.4, timeout=5)
        screen = send(master, sink, "/undo", quiet=1.0, timeout=30)
        r.save("undo-twice", screen)
        # 大厅回来了、正文里没有回复了（输入框里回填的「第一句走查」是预期的）。
        report["second_undo_returns_to_lobby"] = any("Tab 切换" in l for l in screen) and not any(
            "分块吐出来" in l for l in screen
        )
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    keys = ["both_turns_on_screen", "second_turn_gone_from_body", "first_turn_still_there", "undone_note_shown",
            "prompt_refilled_into_editor", "context_reading_refreshed", "scrollback_kept", "second_undo_returns_to_lobby"]
    bad = [k for k in keys if not report.get(k)]
    print("通过" if not bad else f"红: {bad}")
    print("产物：", h.OUT)
