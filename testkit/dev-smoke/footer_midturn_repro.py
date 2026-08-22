#!/usr/bin/env python3
"""回合中途 footer 是否刷新：连 daemon 的终端此前要等整轮结束才动。

daemon 一直在发 chat.round_usage，但 CLI 的 IPC 分发表里没有对应分支，
事件在那一段掉地上。WebUI 自己解 SSE 所以有，终端直连模式走本地事件也
有，唯独日常的「终端连 daemon」没有。

判据：一轮里要有多次模型请求（提示词强制先调工具再回答），期间每 0.4s
抓一次 footer 的 token 数字。修好了就该在回合结束前看到中间值。
绝不触碰线上 8300 daemon：隔离 home + 独立端口。
"""
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from run import DevRepl, HOME, LOGS, build_home, daemon  # noqa: E402

ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07")
# footer 的 token 段形如 "1.9k/1M(0.2%) · Σ5.2k(C48%)"；右边紧挨着编辑框
# 的竖线，别把它一起吃进来。
NUMBERS = re.compile(r"\d[\d.]*[kKmM]?/[^\s┃]+\s+·\s+Σ[^\s┃]+")


def footer_numbers(log: Path) -> str | None:
    raw = log.read_bytes().decode("utf-8", "replace")
    clean = ANSI.sub("", raw)
    hits = NUMBERS.findall(clean)
    return hits[-1] if hits else None


def main() -> int:
    LOGS.mkdir(parents=True, exist_ok=True)
    build_home()
    started = daemon("start")
    print("daemon start rc=", started.returncode, started.stderr.strip()[:160])
    if started.returncode != 0:
        return 1
    try:
        log = LOGS / "footer-midturn.log"
        repl = DevRepl(HOME, log)
        time.sleep(6)
        baseline = footer_numbers(log)
        print("回合前 footer:", baseline)

        # 强制多轮：先跑一个工具，再据结果回答 → 至少两次模型请求。
        repl.send("用 run_command 执行 `echo hello`，然后告诉我输出是什么")

        samples: list[tuple[float, str]] = []
        start = time.time()
        while time.time() - start < 75:
            time.sleep(0.4)
            value = footer_numbers(log)
            if value and (not samples or samples[-1][1] != value):
                samples.append((round(time.time() - start, 1), value))

        print(f"\n回合中 footer 变化 {len(samples)} 次：")
        for at, value in samples:
            print(f"  +{at:>5}s  {value}")
        repl.close()

        distinct = {value for _, value in samples}
        if len(distinct) >= 2:
            print("\n通过：回合结束前 footer 就动过了")
            return 0
        print("\n未通过：整轮只有一个数字，说明仍在等回合结束")
        return 1
    finally:
        print("stop rc=", daemon("stop").returncode)


if __name__ == "__main__":
    raise SystemExit(main())
