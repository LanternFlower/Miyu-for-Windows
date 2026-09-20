#!/usr/bin/env python3
"""目标驱动的续轮里提问，面板要弹出来。

用户 09-20：`/goal 试试用问问题工具随意问我一个问题` 之后永远卡在「准备问题」。
根因是渲染这类轮的泵少一个分支——目标续轮是 daemon 自己开的，REPL 靠
`follow_wake_run`（`src/cli/repl/wake.rs`）挂上去看，而那张手写的分发表里
没有 `question.requested`，事件落进 `_ => {}`。于是面板不弹、没人回答，
那一步就停在「工具名已解码、参数还在流」的显示上。

这里在真 PTY 里：建一个目标 → 驱动器开续轮 → 桩模型调 `ask_question`
（`STUB_ASK=1`）→ 看面板弹不弹、回答完这一轮走不走得下去。

跑法：

    cargo build
    python3 testkit/tui/goal_question.py

产物在 ~/.cache/miyu-goal-question/。

**这些 TUI 走查只能一个一个跑**：共用同一个 `MIYU_HOME` 和桩模型端口。
"""

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-goal-question"))
# 面板最多等多久。续轮要先起来、再跑一次模型请求，比普通回合慢。
PANEL_WAIT = 40.0


def preparing_on(screen):
    """屏幕上还挂着「准备问题」——那是「工具名已解码、参数还在流」的显示。"""
    return any("准备问题" in line for line in screen)


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
        # 参数分片吐：真模型就是这样，中间那段窗口才会画出「准备问题」——
        # 用户看到的正是它挂死在屏幕上。
        env=dict(
            os.environ,
            STUB_PORT=str(h.STUB_PORT),
            STUB_ASK="1",
            STUB_TOOL_ARG_CHUNK="24",
            # 拖慢应答开头：续轮起来之后 REPL 要先挂上去（挂轮是一秒一拍轮询的），
            # 挂上了才看得见「准备问题」那一帧。桩太快的话提问那一瞬早过去了，
            # 而挂轮不补历史（from_start:false），屏幕上只剩一个转轮。
            STUB_RESPONSE_DELAY="6",
        ),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    daemon = tui = None
    report = {}
    try:
        if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        daemon = subprocess.Popen(
            [str(h.BIN), "__daemon", "--port", str(h.PORT)],
            env=dict(h.ENV, MIYU_LOG="info"),
            cwd=str(h.HOME),
            stdout=(OUT / "daemon.log").open("w"),
            stderr=subprocess.STDOUT,
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

        # 建目标 → 自动武装 → 驱动器立刻开第一轮续轮
        os.write(master, "/goal 试试用问问题工具随意问我一个问题".encode())
        h.drain(master, 0.4, sink)
        os.write(master, b"\r")
        report["目标建起来了"] = h.drain_until(master, sink, "/goal running", 30.0)
        shot("00-goal-running")

        # 续轮里模型调 ask_question：面板必须弹出来。
        # 中途每 4 秒抓一帧存下来，卡住时好看清停在哪一步（产物 t-*.txt）。
        found = False
        for tick in range(int(PANEL_WAIT // 4)):
            if h.drain_until(master, sink, "走查用的问题", 4.0):
                found = True
                break
            shot(f"t-{tick * 4:02d}s")
        report["续轮里的提问弹出了面板"] = found
        screen = shot("01-panel")
        # 用户报的那个样子：面板没出来，那一步停在「准备问题」上不动了。
        report["没有卡在「准备问题」"] = not preparing_on(screen)

        if report["续轮里的提问弹出了面板"]:
            # 回车选第一项 → 答案回灌 → 这一轮接着往下走
            os.write(master, b"\r")
            report["回答后这一轮继续"] = h.drain_until(
                master, sink, "走查的回复", 30.0
            )
            shot("02-answered")
            # 面板退场后问答要留在正文里，否则等于没输出（09-19 同款要求）。
            # 正文里它是折叠成一步的（「已回答 N 个问题」+ 选中的那一项），
            # 原始问题文本要点开才看得到，所以按折叠后的样子判。
            after = h.render(bytes(sink))
            report["问答留在正文里"] = any("已回答" in line for line in after) and any(
                "甲选项" in line for line in after
            )
            report["答完不再挂着「准备问题」"] = not preparing_on(after)
        else:
            report["回答后这一轮继续"] = False
            report["问答留在正文里"] = False
            report["答完不再挂着「准备问题」"] = False
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
