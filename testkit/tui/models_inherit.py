#!/usr/bin/env python3
"""`/models` 里「继承全局模型池」的连带（BUG-10）：取消继承时因继承而勾上的模型
跟着取消；一个都没勾就回车要说清楚（不是「未做修改」）；勾回继承回到派生态。

    cargo build
    python3 testkit/tui/models_inherit.py
"""

import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

STUB = {"STUB_CHUNK_SLEEP": "0.01"}


def two_model_provider():
    return {
        "providers": [{
            "id": "stub",
            "display_name": "Stub",
            "base_url": f"http://127.0.0.1:{h.STUB_PORT}/v1",
            "protocol": "openai-chat",
            "api_key": "stub",
            "models": ["stub-model", "stub-model-2"],
        }]
    }


def rows_of(screen):
    """面板里的行：{文字: 勾没勾}。沙箱配置里还会混进别的默认供应商，按名字取。"""
    out = {}
    for line in screen:
        if "[*]" in line or "[ ]" in line:
            out[line.split("]", 1)[1].strip()] = "[*]" in line
    return out


INHERIT, M1, M2 = "继承全局模型池", "Stub / stub-model", "Stub / stub-model-2"


def ticks(rows):
    return (rows.get(INHERIT), rows.get(M1), rows.get(M2))


def open_models(master, sink):
    os.write(master, b"/models")
    h.drain_until(master, sink, "/models", 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=0.6, timeout=20)
    return h.render(bytes(sink))


def key(master, sink, seq, quiet=0.3):
    os.write(master, seq)
    h.settle(master, sink, quiet=quiet, timeout=10)
    return h.render(bytes(sink))


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB, config_extra=two_model_provider())
    try:
        # 先发一句，会话不空（菜单在会话里也一样，顺便让 footer 有模型名）。
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        h.settle(master, sink, quiet=1.2, timeout=40)

        screen = open_models(master, sink)
        r.save("models-open", screen)
        rows = rows_of(screen)
        report["initial_rows"] = ticks(rows)
        report["opens_inheriting_with_pool_ticked"] = ticks(rows) == (True, True, False)
        # 光标在第 0 行：Tab 取消继承 → 派生那一行跟着取消。
        screen = key(master, sink, b"\t")
        r.save("models-untick-inherit", screen)
        rows = rows_of(screen)
        report["untick_inherit_cascades"] = ticks(rows) == (False, False, False)
        # 一个没勾就回车：说清楚，仍继承。
        screen = key(master, sink, b"\r", quiet=0.8)
        report["enter_with_nothing_explains"] = any("一个模型都没勾" in l or "no model is ticked" in l for l in screen)
        screen = open_models(master, sink)
        rows = rows_of(screen)
        report["still_inheriting_after_empty_enter"] = ticks(rows) == (True, True, False)
        # 取消继承，挑第二个模型，回车 → 覆盖成 stub-model-2。
        key(master, sink, b"\t")
        key(master, sink, b"j")
        key(master, sink, b"j")
        screen = key(master, sink, b"\t")
        rows = rows_of(screen)
        report["pick_second_model_rows"] = ticks(rows)
        screen = key(master, sink, b"\r", quiet=1.0)
        r.save("models-override", screen)
        report["override_applied"] = any("会话模型已更新" in l or "已更新当前会话模型" in l or "session model" in l for l in screen)
        report["footer_shows_second_model"] = any("stub-model-2" in l and "┃" in l for l in screen)
        # 重开：继承没勾、第二个勾着；Tab 回继承 → 回到派生态（第一个勾、第二个不勾）。
        screen = open_models(master, sink)
        rows = rows_of(screen)
        report["reopen_shows_override"] = ticks(rows) == (False, False, True)
        screen = key(master, sink, b"\t")
        rows = rows_of(screen)
        report["retick_inherit_restores_pool"] = ticks(rows) == (True, True, False)
        screen = key(master, sink, b"\r", quiet=1.0)
        report["back_to_inherit_note"] = any("跟随全局" in l or "follows the global" in l or "会话模型已更新" in l for l in screen)
        report["footer_back_to_global_model"] = any("stub-model stub" in l and "┃" in l for l in screen)
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    bad = [k for k, v in report.items() if v is False]
    print("通过" if not bad else f"红: {bad}")
    print("产物：", h.OUT)
