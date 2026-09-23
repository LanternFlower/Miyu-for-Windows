#!/usr/bin/env python3
"""关掉 TUI 之后，正在跑的那一轮还活着吗。

用户 09-21：「退掉 TUI，AI 的运行就被中断了，这也不合理，我明明设计的是即使
退掉 TUI 也不会中断 AI 的运行」。自己起的回合那条路（`one_shot.rs`）一直写着
「观众离席，戏照演」，而挂在**后台唤醒轮**上的那条（`wake.rs`）挂断时先发了一条
`Cancel` 再退——用户那个会话一直挂在唤醒轮上，撞的就是它。

这份走查把终端真的拔掉（关掉 PTY 主端，从端立刻 HUP），然后回头查库：那一轮
该是 `completed`，不是 `interrupted`。

    cargo build
    python3 testkit/tui/exit_keeps_turn.py

这些 TUI 走查只能一个一个跑（共用 MIYU_HOME 与桩模型端口）。
"""

import os
import sqlite3
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402


def conversation_db():
    found = sorted((h.HOME / "home").rglob("conversation.db"))
    return found[0] if found else None


def turn_rows():
    path = conversation_db()
    if path is None:
        return []
    # 只读副本：daemon 还开着这个库，直接读要连 wal 一起看。
    conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return conn.execute(
            "SELECT seq, status, display_content, assistant_content"
            "  FROM turns ORDER BY seq"
        ).fetchall()
    finally:
        conn.close()


def main():
    report = {}
    h.ENV["MIYU_LOG"] = "info"
    # 要撞的是**唤醒轮**那条路（`wake.rs::follow_wake_run`），所以后台任务得在
    # 主线回合**收工之后**才完成——否则 daemon 会把报告当 follow-up 排进正在跑
    # 的那一轮，走的是另一条代码路径。主线约 20 秒（一段长思考），后台 25 秒。
    stub, daemon, tui, master, sink = r.start({
        "STUB_REASONING": "1",
        "STUB_REASONING_TEXT": "想一想这件事该怎么做,得想上一会儿才想得明白。" * 30,
        "STUB_BACKGROUND_COMMAND": "sleep 25; echo bg-done",
        "STUB_CHUNK_CHARS": "12",
        "STUB_CHUNK_SLEEP": "0.08",
    })
    try:
        message = "STUB_BG 开工"
        os.write(master, message.encode())
        h.drain_until(master, sink, message, 5.0)
        os.write(master, b"\r")
        # 后台任务完成 → 它的报告出现（跟进轮或唤醒轮，哪条都行）。
        screen = r.wait_screen(
            master, sink,
            lambda s: any(
                ("命令完成" in line) or ("后台任务完成" in line) for line in s
            ),
            90.0,
        )
        report["ex01_wake_report_shows_up"] = screen is not None
        (h.OUT / "exitkeep-wake.txt").write_text(
            "\n".join(screen or r.LAST["screen"] or []) + "\n", encoding="utf-8"
        )
        # 这一轮还在跑（屏幕上有转轮）的时候把终端拔掉。
        screen = r.wait_screen(
            master, sink,
            lambda s: any(
                line.lstrip()[:1] in set("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
                for line in s if line.strip()
            ),
            30.0,
        )
        report["ex02_turn_still_running_when_pulled"] = screen is not None
        # 关掉主端 = 终端没了。TUI 那边 `terminal_hangup()` 立刻为真。
        os.close(master)
        deadline = time.time() + 15
        while time.time() < deadline and tui.poll() is None:
            time.sleep(0.2)
        report["ex03_tui_exited_by_itself"] = tui.poll() is not None
        # 戏照演：等它把剩下的轮跑完。
        deadline = time.time() + 60
        rows = []
        while time.time() < deadline:
            rows = turn_rows()
            if rows and all(row[1] != "running" for row in rows):
                break
            time.sleep(1.0)
        (h.OUT / "exitkeep-turns.txt").write_text(
            "\n".join(f"{row[0]} {row[1]} {row[2][:40]}" for row in rows) + "\n",
            encoding="utf-8",
        )
        # 真的走了唤醒轮那条路：库里多出一条 daemon 合成的轮。走成 follow-up
        # 的话只有一条轮，这份走查就没验到想验的代码。
        report["ex04_a_wake_turn_was_created"] = any(
            row[2].startswith("[后台任务完成]") for row in rows
        )
        report["ex05_no_turn_was_interrupted"] = bool(rows) and all(
            row[1] != "interrupted" for row in rows
        )
        report["ex06_every_turn_completed"] = bool(rows) and all(
            row[1] == "completed" for row in rows
        )
    finally:
        r.stop(tui, daemon, stub)

    passed = sum(1 for value in report.values() if value)
    for name, value in report.items():
        print(f"{'✅' if value else '❌'} {name}")
    print(f"{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    sys.exit(main())
