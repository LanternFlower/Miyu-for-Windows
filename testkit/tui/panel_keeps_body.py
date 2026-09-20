#!/usr/bin/env python3
"""面板开着的时候，**她刚刚吐的正文**还在不在屏幕上。

用户 09-20：「/models 或者 /session 面板开启时，当前输出内容会消失，面板关闭后
又出现。我记得之前只是顶上去而已啊？」补充：「她用问问题功能工具问我问题也会
导致她刚刚的输出消失。」

大走查里那条 `item10_question_keeps_body` 判的是**用户自己那句话**还在不在
（`line.startswith(BAR) and PROMPT in line`），没判她的输出——所以它一直绿着，
盖不住这件事。这里判的是她提问**之前**吐的那段正文（`STUB_ASK_PREFACE`）。

    MIYU_HOME=/tmp/miyu-panelbody/home MIYU_TUI_PORT=18495 STUB_PORT=18496 \\
      MIYU_TUI_RUNTIME=/tmp/mx-panelbody OUT=~/.cache/miyu-panelbody \\
      python3 testkit/tui/panel_keeps_body.py
"""

import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

# 她提问之前先吐这一段。每行带编号，便于看清是「整段没了」还是「滚上去了」。
# 提问前**一个字正文都不能有**。
#
# 这是复现的关键：用户截图里她思考完直接提问，时间线一路是「活的」、从没被切
# 进回放缓冲，所以面板一让屏整片就没了。只要中间吐了正文，正文开头就会把时间
# 线切掉、落进缓冲，那几行反而留得住——我头两版探针分别塞了 60 行和 3 行正文，
# 于是撤掉修复也照样全绿，等于没测（09-20）。
PREFACE = ""
STUB = {
    "STUB_ASK": "1",
    "STUB_ASK_PREFACE": PREFACE,
    "STUB_CHUNK_SLEEP": "0.08",
    # 开思考：用户 09-20 的截图里消失的正是**时间线那几行**（「已思考 · 334
    # 词元 · 3.4s」「准备问题 · 644ms」），不是正文段落。那几行属于活动区，
    # 还没落进回放缓冲；不开思考就造不出这个现场。
    "STUB_REASONING": "1",
    # 思考要**够长**：默认只有两小块（0.02 秒吐完），轮询根本抓不到那一行，
    # 「开面板前时间线在屏幕上」这条前提就立不住（09-20 实测）。
    "STUB_REASONING_TEXT": "这段思考是为了让「已思考」那一行稳稳出现在屏幕上。" * 20,
}


def squash(screen):
    return "".join(line.strip() for line in screen)


def has_timeline(joined):
    """时间线那一行在不在。中英两种界面都要认——沙箱渲染的是「› 1 thought」，
    真机中文界面是「已思考 · 334 词元 · 3.4s」（用户 09-20 截图）。"""
    return any(mark in joined for mark in ("思考", "thought", "准备问题"))


def visible_lines(screen):
    """屏幕上看得见的正文行编号。"""
    import re

    return sorted({int(n) for n in re.findall(r"正文第(\d{2})行", squash(screen))})


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        os.write(master, "她会先说一段再提问".encode())
        h.drain_until(master, sink, "她会先说一段再提问", 5.0)
        os.write(master, b"\r")
        # 先坐实：面板弹出来**之前**，时间线那几行确实在屏幕上。不然「消失」
        # 无从谈起。
        deadline = time.time() + 60
        opened = False
        before_panel = ""
        while time.time() < deadline:
            h.drain(master, 0.1, sink)
            now = squash(h.render(bytes(sink)))
            if has_timeline(now) and not before_panel:
                before_panel = now
                r.save("panelbody-before-panel", h.render(bytes(sink)))
            if "走查用的问题" in now:
                opened = True
                break
        report["_开面板前的时间线"] = [
            mark
            for mark in ("已思考", "思考", "thought", "准备问题")
            if mark in before_panel
        ]
        report["开面板前时间线在屏幕上"] = has_timeline(before_panel)
        screen = h.render(bytes(sink))
        r.save("panelbody-question-open", screen)
        report["提问面板弹出来了"] = opened
        during = visible_lines(screen)
        report["_面板开着时看得见的正文行"] = during
        # 用户 09-20 截图里真正消失的东西：时间线那几行。
        joined = squash(screen)
        report["_面板开着时的时间线"] = [
            mark
            for mark in ("已思考", "思考", "thought", "准备问题")
            if mark in joined
        ]
        report["面板开着时思考那行还在"] = has_timeline(joined)

        # 回答掉，面板收起。
        os.write(master, b"\r")
        h.settle(master, sink, quiet=1.5, timeout=60)
        screen = h.render(bytes(sink))
        r.save("panelbody-question-answered", screen)
        report["_答完之后看得见的正文行"] = visible_lines(screen)
        report["答完之后时间线还在"] = has_timeline(squash(screen))

        # ── /models 与 /session：回合已经说完，平时开面板 ──
        for label, command in (("models", "/models"), ("session", "/session")):
            before = visible_lines(h.render(bytes(sink)))
            os.write(master, command.encode())
            h.drain_until(master, sink, command, 5.0)
            os.write(master, b"\r")
            h.drain(master, 2.5, sink)
            screen = h.render(bytes(sink))
            r.save(f"panelbody-{label}-open", screen)
            opened = any(
                "选择模型" in line or "选择会话" in line or "Select" in line
                for line in screen
            )
            during = visible_lines(screen)
            report[f"_{label} 开面板前/开着时"] = [len(before), during[:3], during[-3:]]
            report[f"{label} 面板开出来了"] = opened
            report[f"{label} 面板开着时时间线还在"] = has_timeline(squash(screen))
            os.write(master, b"\x1b")
            h.drain(master, 2.0, sink)
            screen = h.render(bytes(sink))
            r.save(f"panelbody-{label}-closed", screen)
            report[f"{label} 收掉面板时间线还在"] = has_timeline(squash(screen))
        # ── 回合跑着时反复开 /models：思考不该被切成好几行 ──
        #
        # 用户 09-20 截图：每敲一次 `/models` 就多一行「Worked for 3.6s ·
        # 1 thought」，三次就三行。根因是「分离 → 开面板 → 挂回来」——每次分离
        # 当前渲染器收尾定稿吐一行小结，挂回来又是全新的渲染器重新计时。
        os.write(master, "再长思考一次".encode())
        h.drain_until(master, sink, "再长思考一次", 5.0)
        os.write(master, b"\r")
        h.drain(master, 3.0, sink)
        for _ in range(3):
            os.write(master, b"/models")
            h.drain_until(master, sink, "/models", 5.0)
            os.write(master, b"\r")
            h.drain(master, 2.5, sink)
            os.write(master, b"\x1b")
            h.drain(master, 1.5, sink)
        h.settle(master, sink, quiet=2.0, timeout=120)
        screen = h.render(bytes(sink))
        r.save("panelbody-models-repeat", screen)
        rows = [line for line in screen if "thought" in line or "思考" in line]
        # **已知未修**，所以只量不判（不能把红的断言留在仓库里）。
        #
        # 每敲一次 `/models` 就多一行「Worked for 3.6s · 1 thought」：这两条命令
        # 走「分离 → 开面板 → 挂回来」，每分离一次就开一个新的渲染段，N 次面板
        # 就是 N 段。试过分离时不收尾定稿，结果更糟——每次会在屏幕上留一个冻住
        # 的「思考中」块。要从根上解决得让面板**不分离**（寄宿在回合循环里）。
        # 写在 ~/Documents/MiyuPlan/2026-09-20-midturn-panel-timeline.md。
        report["_反复开面板后的思考行（已知未修）"] = [
            line.strip()[:44] for line in rows
        ]
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    checks = {k: v for k, v in report.items() if not k.startswith("_")}
    for name, ok in checks.items():
        print(f"{'✅' if ok else '❌'} {name}")
    print(f"\n{sum(1 for v in checks.values() if v)}/{len(checks)} passed")
    for name, value in report.items():
        if name.startswith("_"):
            print(f"   {name[1:]}: {value}")
    print("产物：", h.OUT)
