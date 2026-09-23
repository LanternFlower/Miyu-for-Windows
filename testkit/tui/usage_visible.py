#!/usr/bin/env python3
"""她能不能查到这个会话烧了多少 token。

用户 09-22：「AI 没有办法得知这个会话的 token 消耗情况」。footer 上一直有那几个
数，但那是画给人看的，从不进提示词——模型手上一个字都没有。

做法是给现成的 `query_token_usage` 加一个 `scope=session`（用户提议：别往提示词
里塞常驻字段）。工具面里不多一件工具，成本只是描述里多出的几十个 token，而且
进的是缓存前缀。

这份走查让桩模型真的去调一次那件工具，再从它收到的下一个请求里把**工具返回的
正文**捞出来，检查本会话那几个数在不在。

    cargo build
    python3 testkit/tui/usage_visible.py

TUI 走查只能一个一个跑。
"""

import json
import os
import sqlite3
import time
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

LOG = h.OUT / "usage-requests.jsonl"


def completed_turns():
    """库里跑完的轮数。"""
    found = sorted((h.HOME / "home").rglob("conversation.db"))
    if not found:
        return 0
    conn = sqlite3.connect(f"file:{found[0]}?mode=ro", uri=True)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM turns WHERE status = 'completed'"
        ).fetchone()[0]
    except Exception:
        return 0
    finally:
        conn.close()


def tool_outputs():
    """请求里带回去的 tool 结果正文。模型看到的就是这些。"""
    if not LOG.exists():
        return []
    found = []
    for line in LOG.read_text(encoding="utf-8").splitlines():
        try:
            payload = json.loads(line)
        except Exception:
            continue
        for message in payload.get("messages", []):
            if message.get("role") == "tool":
                found.append(str(message.get("content")))
    return found


def main():
    report = {}
    h.ENV["MIYU_LOG"] = "info"
    if LOG.exists():
        LOG.unlink()
    stub, daemon, tui, master, sink = r.start({
        "STUB_REASONING": "1",
        "STUB_REASONING_TEXT": "想一下。",
        "STUB_REQUEST_LOG": str(LOG),
        # 用量按请求体积算（像真供应商那样随上下文涨），回复放长，第一轮跑完
        # 就有像样的占用数。
        "STUB_USAGE_BY_SIZE": "1",
        "STUB_REPLY": "这是一段够长的回复，用来把上下文撑起来。" * 120,
        "STUB_CHUNK_CHARS": "80",

        "STUB_CHUNK_SLEEP": "0.02",
    })
    try:
        h.drain_until(master, sink, "A G E N T", 15.0)
        h.drain(master, 1.0, sink)
        for index in range(2):
            # 第二句带记号：桩看到它就去查本会话用量。第一轮不查——
            # `token_context_end` 要等整轮最后一次请求结束才落库。
            message = "第一句" if index == 0 else "STUB_USAGE 查一下用量"
            os.write(master, message.encode())
            h.drain_until(master, sink, message, 5.0)
            os.write(master, b"\r")
            deadline = time.time() + 120
            while time.time() < deadline and completed_turns() <= index:
                h.drain(master, 0.3, sink)
            h.drain(master, 0.5, sink)
        report["uv01_turns_ran"] = completed_turns() >= 2

        outputs = [out for out in tool_outputs() if "本会话" in out]
        (h.OUT / "usage-tool-output.txt").write_text(
            "\n---\n".join(outputs) + "\n", encoding="utf-8"
        )
        report["uv02_she_called_the_tool"] = bool(outputs)
        text = "\n".join(outputs)
        # 三个数都要有：当前上下文、累计、轮数。
        report["uv03_reports_context"] = "当前上下文" in text and "占上下文窗口" in text
        report["uv04_reports_spent"] = "这个会话累计" in text
        report["uv05_reports_turns"] = "已经聊了" in text
        # 数字得是真的——不能是占位或零。
        found = re.search(r"当前上下文 \*\*([0-9.]+)([kKmM])\*\*", text)
        report["uv06_numbers_are_real"] = bool(found) and float(found.group(1)) > 0
    finally:
        r.stop(tui, daemon, stub)

    passed = sum(1 for value in report.values() if value)
    for name, value in report.items():
        print(f"{'✅' if value else '❌'} {name}")
    print(f"{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    sys.exit(main())
