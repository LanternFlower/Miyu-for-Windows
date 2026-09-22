#!/usr/bin/env python3
"""长任务跑着的时候，输入框右上角要有一行 `/goal …`。

目标跑起来之后屏幕上一直只有正文，看不出「它还在自己往前跑吗、第几轮了」
（用户 09-19）。这行常驻提示就管这一件事。

这里在真 PTY 里建一个目标、暂停、恢复、清掉，每一步都抓屏看那一行有没有出现
在**输入框第一行的右端**（不是 footer，footer 那儿已经挤着模型名和用量了），
以及跑着的时候秒数是不是真的在往上走。

桩模型被故意拖慢（`STUB_RESPONSE_DELAY`）：续轮一开就要在飞行中停住足够久，
否则「跑着」这个状态一闪而过，抓到的永远是它跑完之后停下来等人的样子。

跑法：

    cargo build
    python3 testkit/tui/goal_hint.py

产物在 ~/.cache/miyu-goal-hint/。

**这些 TUI 走查只能一个一个跑**：共用同一个 `MIYU_HOME` 和桩模型端口。
"""

import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-goal-hint"))
# 一轮续轮在飞行中停住多久。要够两次抓屏之间看出秒数在走。
ROUND_SECONDS = 9


def input_top_row(screen):
    """输入框第一行。

    从 footer（带模型名那一行）往上数连着的竖条行：输入框是「顶行 + 输入行若干
    + 底行 + footer」，再往上是 tail 每帧清空的那行空行，走到它就停。正文里也有
    竖条，所以不能整屏找。
    """
    footer = next(
        (index for index in range(len(screen) - 1, -1, -1) if "stub-model" in screen[index]),
        None,
    )
    if footer is None:
        return ""
    top = footer
    while top - 1 >= 0 and "┃" in screen[top - 1]:
        top -= 1
    return screen[top]


def hint_on(screen):
    """屏幕上那一行 `/goal …`（整屏找，位置另外单独判）。"""
    for line in screen:
        if "/goal" in line and ("running" in line or "paused" in line or "blocked" in line):
            return line.strip()
    return ""


def lobby_on(screen):
    """大厅还在不在：banner 的 `A G E N T` 副标题是它独有的。"""
    return any("A G E N T" in line for line in screen)


def hint_seconds(line):
    """提示里那个「跑了多少秒」。没有就 None。"""
    found = re.search(r"·\s*(\d+)s\b", line)
    return int(found.group(1)) if found else None


def main():
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    h.write_config()
    h.kill_stale_daemon()

    stub = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(h.STUB_PORT),
            STUB_RESPONSE_DELAY=str(ROUND_SECONDS),
        ),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = tui = None
    report = {}
    try:
        if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        daemon = subprocess.Popen(
            [str(h.BIN), "__daemon", "--port", str(h.PORT)],
            env=h.ENV, cwd=str(h.HOME),
            stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
        )
        if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
            print("! daemon 没起来", file=sys.stderr)
            return 2

        tui, master = h.spawn_tui()
        sink = bytearray()
        h.drain(master, 3.0, sink)

        def shot(tag):
            screen = h.render(bytes(sink))
            (OUT / f"{tag}.txt").write_text("\n".join(screen), encoding="utf-8")
            return screen

        def send(line, wait):
            os.write(master, line.encode())
            # 打完再回车。回显不另外等：PTY 里字节是有序的，编辑器一定先读到
            # 这行字再读到回车。（想等回显也等不准——「/goal pause」这几个字
            # 正好是提示那一行的前缀。）
            h.drain(master, 0.4, sink)
            os.write(master, b"\r")
            return h.drain_until(master, sink, wait, 30.0)

        # 1. 还没有目标：那一行不该出现
        h.drain(master, 1.0, sink)
        report["没目标时不画"] = not hint_on(shot("00-no-goal"))

        # 2. 建一个目标 → 自动武装 → 驱动器立刻开第一轮 → running
        appeared = send("/goal 走查一个长任务目标", "/goal running")
        screen = shot("01-running")
        line = hint_on(screen)
        report["建了目标就出现"] = appeared and bool(line)
        report["显示 running"] = "running" in line
        report["贴在右端"] = bool(line) and any(
            "/goal" in row and row.index("/goal") > len(row) // 2 for row in screen
        )
        report["在输入框第一行上"] = "/goal" in input_top_row(screen)
        # 空会话里建目标要撤大厅：续轮马上就要往屏幕上写正文，而大厅那层星空
        # 盖在正文之上，不撤的话整轮跑完屏幕上还是一片星空（用户 09-19）。
        report["建目标后退出大厅"] = not lobby_on(screen)
        # pyte 把颜色吃掉了，所以配色只能回原始流里验：这行用的是当前模式
        # 的高亮色（普通 = 粗体蓝 `1;34`，和左侧那根粗线、左下角的模式标签
        # 同一个），不是暗灰（用户 09-19：「这个是值得高亮的内容」）。
        report["用模式高亮色画"] = b"\x1b[1m\x1b[34m/goal running" in bytes(sink)
        report["不在 footer 上"] = not any(
            "/goal" in row and ("Σ" in row or "stub-model" in row) for row in screen
        )

        # 3. 秒数自己往上走：这一轮还在飞，隔几秒再看一眼。轮次也在这儿看
        #    ——上一张抓的是「刚武装、驱动器还没认领这一轮」那一瞬（第 0 轮
        #    不显示，见 `goal_hint_text`）。
        first = hint_seconds(line)
        h.drain(master, 3.5, sink)
        line = hint_on(shot("01b-ticking"))
        later = hint_seconds(line)
        report["说第几轮"] = "第 1 轮" in line
        report["跑着的时候有秒数"] = first is not None
        report["秒数自己在走"] = None not in (first, later) and later > first

        # 4. 已经有目标了再建一个：这条会被拒，而被拒是**没有可见后果**的
        #    （流照跑、提示行不变），所以必须打一句（用户 09-19 实测：一点
        #    反应都没有）。这会儿续轮正跑着，走的是附着期间那条静默路径。
        send("/goal 另一个目标", "已有未完成的目标")
        screen = shot("01c-duplicate")
        report["重复建目标有提示"] = any("已有未完成的目标" in row for row in screen)

        # 5. 跑着的时候暂停（这条走的是「附着期间静默执行」那条路，没有回执
        #    文案，那行提示就是唯一的反馈）→ paused，且不再显示秒数
        paused = send("/goal pause", "/goal paused")
        line = hint_on(shot("02-paused"))
        report["暂停后显示 paused"] = paused and "paused" in line
        report["暂停了也一样高亮"] = b"\x1b[1m\x1b[34m/goal paused" in bytes(sink)
        report["暂停后不显示秒数"] = hint_seconds(line) is None

        # 6. 恢复 → 驱动器接着开一轮 → 回到 running
        resumed = send("/goal resume", "/goal running")
        report["恢复后回到 running"] = resumed and "running" in hint_on(shot("03-resumed"))
        # 恢复那一瞬轮次还是旧的（先武装，驱动器随后才认领下一轮），等它认领。
        report["轮次跟着涨"] = h.drain_until(master, sink, "第 2 轮", 20.0)
        shot("03b-round-2")
        # 续轮的正文得真的看得见——大厅那层星空盖在正文之上，用户报的
        # 「建了目标什么都没发生」就是这么来的。等这一轮说完话。
        report["续轮正文看得见"] = h.drain_until(master, sink, "走查的回复", 30.0)
        shot("03c-round-2-reply")

        # 7. 清掉 → 那一行消失
        send("/goal clear", "目标已清除")
        h.drain(master, 1.5, sink)
        report["清掉后不画"] = not hint_on(shot("04-cleared"))
    finally:
        for process in (tui, daemon, stub):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()

    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    passed = 0
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
    print(f"\n{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    raise SystemExit(main())
