#!/usr/bin/env python3
"""面板开着的时候，**她刚刚吐的正文**还在不在屏幕上。

用户 09-20：「/models 或者 /session 面板开启时，当前输出内容会消失，面板关闭后
又出现。我记得之前只是顶上去而已啊？」补充：「她用问问题功能工具问我问题也会
导致她刚刚的输出消失。」

大走查里那条 `item10_question_keeps_body` 判的是**用户自己那句话**还在不在
（`line.startswith(BAR) and PROMPT in line`），没判她的输出——所以它一直绿着，
盖不住这件事。这里判的是她提问**之前**吐的那段正文（`STUB_ASK_PREFACE`）。

后半段是同一天的另一半：回合跑着时反复开 `/models` / `/session`，思考不该被切成
好几行「Worked for … 1 thought」（面板寄宿在回合循环里，不分离），`/models` 选完
就地落地，`/session` 挑了别的会话真的切过去。

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
    # 「开面板前时间线在屏幕上」这条前提就立不住（09-20 实测）。后半段回合中
    # 要连开三次面板（每次约 4 秒），思考得撑过那十几秒，不然第三次开在回合
    # 结束之后，测的就不是回合中的面板了。
    "STUB_REASONING_TEXT": "这段思考是为了让「已思考」那一行稳稳出现在屏幕上。" * 40,
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
        # ── 回合跑着时反复开 /models、/session：思考不该被切成好几行 ──
        #
        # 用户 09-20 截图：每敲一次 `/models` 就多一行「Worked for 3.6s ·
        # 1 thought」，三次就三行。根因是「分离 → 开面板 → 挂回来」——每次分离
        # 当前渲染器收尾定稿吐一行小结，挂回来又是全新的渲染器重新计时。修法是
        # 面板**寄宿在回合循环里**跑（`repl/midturn_panel.rs`），全程同一个渲染
        # 器，所以这一轮说完只该有**一行**小结。
        #
        # 计数只看**这一轮用户那句话之下**的小结行：上面几轮的小结还留在屏上，
        # 整屏数永远不止一行（修前的 4 行里就有 1 行是上一轮的）。
        for label, command, keys in (
            ("models", "/models", [b"\x1b"]),
            ("session", "/session", [b"\x1b"]),
            # 真选一次：↓ → Tab（取消继承）→ Tab（把 stub 模型勾回来）→ Enter，
            # 落成会话覆盖，回执走通知条。选完回合照跑、小结照样只有一行。
            ("models-apply", "/models", [b"\x1b[B", b"\t", b"\t", b"\r"]),
        ):
            prompt = f"再长思考一次{label}"
            os.write(master, prompt.encode())
            h.drain_until(master, sink, prompt, 5.0)
            os.write(master, b"\r")
            h.drain(master, 3.0, sink)
            times = 1 if label.endswith("apply") else 3
            opened = 0
            midturn = 0
            for attempt in range(times):
                os.write(master, command.encode())
                h.drain_until(master, sink, command, 5.0)
                os.write(master, b"\r")
                h.drain(master, 2.5, sink)
                screen = h.render(bytes(sink))
                r.save(f"panelbody-{label}-midturn-open{attempt + 1}", screen)
                if any(
                    "选择模型" in line or "选择会话" in line or "Select" in line
                    for line in screen
                ):
                    opened += 1
                # 面板弹出来那一刻这一轮还没说完：正文还没出现在她那句话下面。
                if REPLY_MARK not in "".join(below_prompt(screen, prompt)):
                    midturn += 1
                for key in keys:
                    os.write(master, key)
                    h.drain(master, 1.0, sink)
                h.drain(master, 0.5, sink)
            if label.endswith("apply"):
                report[f"回合中 {command} 选完有回执"] = wait_text(
                    master, sink, ("会话模型已更新", "session model", "已更新当前会话模型"), 4.0
                )
            h.settle(master, sink, quiet=2.0, timeout=180)
            screen = h.render(bytes(sink))
            r.save(f"panelbody-{label}-repeat", screen)
            rows = summary_rows(below_prompt(screen, prompt))
            report[f"回合中 {command} 每次都开出面板"] = opened == times
            report[f"回合中 {command} 每次都开在回合中途"] = midturn == times
            report[f"回合中 {command} 之后思考只收成一行"] = len(rows) == 1
            report[f"回合中 {command} 之后回复照常说完"] = REPLY_MARK in "".join(
                below_prompt(screen, prompt)
            )
            report[f"_回合中 {command} 之后的小结行"] = [line.strip()[:44] for line in rows]

        # ── 回合跑着时 /session 真的换走：不是取消，是挑了另一条 ──
        #
        # 换会话要动 footer / 历史 / 车道，回合循环自己做不了，得把选好的那条
        # 带回 `RemoteRepl`（`SuspendedAction::SwitchSession`）。换走之后这一轮
        # 在 daemon 里继续跑（属于原来那条会话），屏幕上是另一条会话的回放。
        os.write(master, b"/new")
        h.drain_until(master, sink, "/new", 5.0)
        os.write(master, b"\r")
        h.drain(master, 2.5, sink)
        done_before = completed_turns()
        prompt = "再长思考一次switch"
        os.write(master, prompt.encode())
        h.drain_until(master, sink, prompt, 5.0)
        os.write(master, b"\r")
        # 新会话的第一轮桩模型会先反问（`STUB_ASK` 按「这条会话还没有工具结果」
        # 判）：等提问面板出来、回车选默认项，她才开始那段长思考。第一版没等，
        # `/session` 那几个键全敲进了提问面板里（09-20 实测）。
        report["切会话场：新会话第一轮先弹了提问面板"] = wait_text(
            master, sink, ("走查用的问题",), 60.0
        )
        os.write(master, b"\r")
        h.drain(master, 3.0, sink)
        os.write(master, b"/session")
        h.drain_until(master, sink, "/session", 5.0)
        os.write(master, b"\r")
        h.drain(master, 2.5, sink)
        screen = h.render(bytes(sink))
        r.save("panelbody-switch-open", screen)
        # 面板里挑**另一条**：高亮在第一行就往下，否则往上。
        rows = picker_rows(screen)
        highlighted = [index for index, line in enumerate(rows) if line.startswith("›")]
        report["_切会话面板里的会话行"] = [line[:40] for line in rows]
        report["回合中 /session 面板列出了两条以上会话"] = len(rows) >= 2
        os.write(master, b"\x1b[B" if highlighted == [0] else b"\x1b[A")
        h.drain(master, 0.8, sink)
        os.write(master, b"\r")
        report["回合中 /session 挑别的会话真的切过去了"] = wait_text(
            master, sink, ("已切换到会话", "switched to session"), 6.0
        )
        screen = h.render(bytes(sink))
        r.save("panelbody-switch-done", screen)
        # 换走的那一轮在 daemon 里照样跑完。
        deadline = time.time() + 90
        while time.time() < deadline and completed_turns() <= done_before:
            h.drain(master, 0.5, sink)
        report["换走之后那一轮在 daemon 里照样跑完"] = completed_turns() > done_before
        report["_换会话前后完成的轮数"] = [done_before, completed_turns()]
        return report
    finally:
        r.stop(tui, daemon, stub)


# 桩模型的正文开头（`stub_llm.py` 的默认 REPLY）：出现在她那句话下面就是这一轮说到正文了。
REPLY_MARK = "好的,收到"


def below_prompt(screen, prompt):
    """用户那句话（回显进正文的那一行）之下的所有行。"""
    index = max((i for i, line in enumerate(screen) if prompt in line), default=-1)
    return screen[index + 1 :]


def summary_rows(lines):
    """收缩成「› Worked for … · 1 thought」的小结行。中英两种界面都认。"""
    return [
        line
        for line in lines
        if line.strip().startswith("›") and ("thought" in line or "思考" in line)
    ]


def picker_rows(screen):
    """会话面板里的条目行（去掉左侧竖条），从「选择会话」抬头到帮助行之间。"""
    header = next(
        (i for i, line in enumerate(screen) if "选择会话" in line or "Select session" in line),
        None,
    )
    if header is None:
        return []
    rows = []
    for line in screen[header + 1 :]:
        text = line.strip().lstrip("┃").strip()
        if not text or "Esc" in text:
            break
        if " · " in text:
            rows.append(text)
    return rows


def wait_text(master, sink, needles, timeout):
    """等屏幕上出现任一句话（通知条几秒就过期，得边读边找）。"""
    deadline = time.time() + timeout
    while time.time() < deadline:
        h.drain(master, 0.2, sink)
        joined = squash(h.render(bytes(sink)))
        if any(needle in joined for needle in needles):
            return True
    return False


def completed_turns():
    import sqlite3

    candidates = sorted(Path(h.HOME).glob("home/*/conversation.db"))
    if not candidates:
        return 0
    connection = sqlite3.connect(f"file:{candidates[0]}?mode=ro", uri=True)
    try:
        return connection.execute(
            "SELECT COUNT(*) FROM turns WHERE status = 'completed'"
        ).fetchone()[0]
    finally:
        connection.close()


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
