#!/usr/bin/env python3
"""上键历史里不该有 daemon 自己合成的轮（BUG-04：后台任务完成报告漏进历史输入）。

后台任务跑完，daemon 用一条 `<background-job-report>…` 开头的「用户消息」唤醒会话
跟进；它在库里是普通的 user 行，换一次会话（历史重载）之后按上键就翻出整段报告。
走查：跑一条后台命令 → 等唤醒那一轮 → `/new` 再切回来（触发历史重载）→ 上键，
输入框里只该出现自己敲过的话。

    cargo build
    python3 testkit/tui/history_no_synthetic.py            # 修后
    MIYU_BIN=<旧二进制> python3 testkit/tui/history_no_synthetic.py   # A/B
"""

import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

STUB = {
    "STUB_BACKGROUND": "1",
    "STUB_BACKGROUND_COMMAND": "echo 后台第 1 行; echo 后台第 2 行",
    "STUB_CHUNK_SLEEP": "0.01",
}
PROMPT = "走查一句 跑个后台任务"


def input_rows(screen):
    """输入框那几行：竖条开头、在 footer（含「·」的模式行）之上。"""
    rows = []
    for line in screen:
        if line.startswith("┃") and "·" not in line:
            text = line[1:].strip()
            if text:
                rows.append(text)
    return rows


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        os.write(master, PROMPT.encode())
        h.drain_until(master, sink, PROMPT, 3.0)
        os.write(master, b"\r")
        # 等后台任务跑完、daemon 唤醒这条会话跟进：唤醒那一轮会再回一句，屏上出现
        # 第二段回复（那一行 ⚙ 提示可能被收进 Worked for 里，不拿它当判据）。
        woke = r.wait_screen(
            master, sink,
            lambda rows: sum(1 for line in rows if "分块吐出来" in line) >= 2,
            timeout=90,
        )
        report["wake_turn_seen"] = woke is not None
        h.settle(master, sink, quiet=1.5, timeout=60)
        # 换一次会话再换回来：历史是在换会话时从对话记录重建的。
        os.write(master, b"/new")
        h.drain_until(master, sink, "/new", 3.0)
        os.write(master, b"\r")
        h.settle(master, sink, quiet=1.0, timeout=30)
        os.write(master, b"/session")
        h.drain_until(master, sink, "/session", 3.0)
        os.write(master, b"\r")
        h.settle(master, sink, quiet=0.6, timeout=20)
        os.write(master, b"j")
        h.settle(master, sink, quiet=0.3, timeout=5)
        picker = h.render(bytes(sink))
        selected = next((l for l in picker if "›" in l and ("普通：" in l or "normal:" in l)), "")
        report["picker_selected_old_session"] = "*" not in selected and bool(selected)
        os.write(master, b"\r")
        h.settle(master, sink, quiet=1.2, timeout=40)
        back = h.render(bytes(sink))
        report["switched_back"] = any(PROMPT in line for line in back)
        # 上键翻历史：翻三格，每一格都不能是合成的报告。
        seen = []
        for _ in range(3):
            os.write(master, b"\x1b[A")
            h.settle(master, sink, quiet=0.3, timeout=5)
            screen = h.render(bytes(sink))
            seen.extend(input_rows(screen))
        r.save("history-after-up", h.render(bytes(sink)))
        report["history_seen"] = [row[:70] for row in seen][:6]
        leaked = [row for row in seen if "background-job-report" in row or "已执行完毕" in row or "系统自动触发" in row]
        report["leaked_rows"] = [row[:60] for row in leaked]
        report["own_prompt_in_history"] = any(PROMPT in row for row in seen)
        report["no_synthetic_report_in_history"] = not leaked
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    ok = report.get("wake_turn_seen") and report.get("no_synthetic_report_in_history") and report.get("own_prompt_in_history")
    print("通过" if ok else "红")
    print("产物：", h.OUT)
