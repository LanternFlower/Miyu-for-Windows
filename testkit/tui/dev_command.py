#!/usr/bin/env python3
"""`/dev` 与跨模式的 `/session`（BUG-13）真机走查。

- 普通模式里 `/dev`：切到开发模式那条车道的会话（等同 `miyu dev`），footer 变「开发」；
- `/session` 两侧合并列出：开发模式里看得见「普通 · …」，普通模式里看得见「开发 · …」；
- 在菜单里挑另一侧的会话，车道跟着切回去（footer 变回「普通」）；
- 开发模式里再 `/dev` 只提示一句；`/normal` 切回普通车道，再 `/dev` 回到刚才那条开发会话。

    cargo build
    python3 testkit/tui/dev_command.py
"""

import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

STUB = {"STUB_CHUNK_SLEEP": "0.01"}


def footer_mode(screen):
    """footer 那行「┃ 开发 · 模型」。空会话时它在大厅中段，不在屏底，整屏找。"""
    for line in reversed(screen):
        if "┃ 开发 ·" in line or "┃ dev ·" in line:
            return "dev"
        if "┃ 普通 ·" in line or "┃ normal ·" in line:
            return "normal"
    return None


def ask(master, sink, text):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=1.2, timeout=40)
    return h.render(bytes(sink))


def command(master, sink, text, quiet=0.8):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=quiet, timeout=20)
    return h.render(bytes(sink))


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        screen = ask(master, sink, "普通模式第一句")
        report["starts_normal"] = footer_mode(screen) == "normal"
        # /dev：去开发车道
        screen = command(master, sink, "/dev")
        r.save("dev-after-dev", screen)
        report["dev_switches_footer"] = footer_mode(screen) == "dev"
        screen = ask(master, sink, "开发模式说一句")
        report["dev_turn_ran"] = any("分块吐出来" in line for line in screen)
        # 开发模式里再 /dev：只提示
        screen = command(master, sink, "/dev")
        report["dev_again_only_notes"] = footer_mode(screen) == "dev" and any(
            "已经在开发模式" in line or "already in dev" in line for line in screen
        )
        # /normal：回普通车道，再 /dev 回来（两条都要能来回）
        screen = command(master, sink, "/normal")
        r.save("dev-after-normal", screen)
        report["normal_switches_back"] = footer_mode(screen) == "normal" and any(
            "普通模式第一句" in line for line in screen
        )
        screen = command(master, sink, "/normal")
        report["normal_again_only_notes"] = any(
            "已经在普通模式" in line or "already in normal" in line for line in screen
        )
        screen = command(master, sink, "/dev")
        report["dev_returns_to_dev_session"] = footer_mode(screen) == "dev" and any(
            "开发模式说一句" in line for line in screen
        )
        # /session：两侧都列
        os.write(master, b"/session")
        h.drain_until(master, sink, "/session", 3.0)
        os.write(master, b"\r")
        h.settle(master, sink, quiet=0.6, timeout=20)
        picker = h.render(bytes(sink))
        r.save("dev-picker-from-dev", picker)
        # 行格式 09-18 定的是「模式 · 上下文 · 标题」，**空格点空格**不是冒号。
        # 判据一直停在旧写法上，于是从那天起这三条就一直红着（09-20 发现）。
        rows = [line for line in picker if "普通 ·" in line or "开发 ·" in line or "normal ·" in line or "dev ·" in line]
        report["picker_rows"] = [line.strip()[:60] for line in rows]
        has_normal = any("普通 ·" in line or "normal ·" in line for line in rows)
        has_dev = any("开发 ·" in line or "dev ·" in line for line in rows)
        report["picker_lists_both_modes"] = has_normal and has_dev
        # 当前是开发模式：开发的排前面、普通的排后面，不混排（用户 09-18 截图）。
        kinds = ["dev" if ("开发 ·" in line or "dev ·" in line) else "normal" for line in rows]
        first_normal = next((i for i, k in enumerate(kinds) if k == "normal"), len(kinds))
        report["picker_groups_current_lane_first"] = all(k == "normal" for k in kinds[first_normal:])
        # 把光标挪到「普通 · 」那一行再回车
        moved = False
        for _ in range(12):
            current = h.render(bytes(sink))
            selected = next(
                (line for line in current if "›" in line and ("普通 ·" in line or "开发 ·" in line or "normal ·" in line or "dev ·" in line)),
                None,
            )
            if selected is not None and ("普通 ·" in selected or "normal ·" in selected):
                moved = True
                break
            os.write(master, b"j")
            h.settle(master, sink, quiet=0.2, timeout=3)
        report["cursor_reached_normal_row"] = moved
        os.write(master, b"\r")
        h.settle(master, sink, quiet=1.0, timeout=30)
        screen = h.render(bytes(sink))
        r.save("dev-back-to-normal", screen)
        report["picking_normal_session_switches_lane_back"] = footer_mode(screen) == "normal"
        # 普通模式里 /session 也看得见开发会话
        os.write(master, b"/session")
        h.drain_until(master, sink, "/session", 3.0)
        os.write(master, b"\r")
        h.settle(master, sink, quiet=0.6, timeout=20)
        picker = h.render(bytes(sink))
        r.save("dev-picker-from-normal", picker)
        report["normal_picker_shows_dev_session"] = any(
            "开发 ·" in line or "dev ·" in line for line in picker
        )
        os.write(master, b"\x1b")
        h.settle(master, sink, quiet=0.4, timeout=5)
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    bad = [k for k, v in report.items() if v is False]
    print("通过" if not bad else f"红: {bad}")
    print("产物：", h.OUT)
