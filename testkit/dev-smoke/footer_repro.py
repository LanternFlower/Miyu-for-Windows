#!/usr/bin/env python3
"""复现验收#23:/models 切换后 footer 是否掉思考程度段。"""
import os
import re
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from run import DevRepl, HOME, LOGS, daemon, env  # noqa: E402


def footer_lines(log: Path):
    raw = log.read_bytes().decode("utf-8", "replace")
    clean = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", raw)
    return [l.strip() for l in clean.splitlines() if "opencodego" in l or "·" in l]


def main():
    LOGS.mkdir(exist_ok=True)
    started = daemon("start")
    print("daemon start rc=", started.returncode, started.stderr.strip()[:120])
    try:
        log = LOGS / "footer-repro.log"
        repl = DevRepl(HOME, log)
        time.sleep(5)
        before = footer_lines(log)[-3:]
        print("== 启动 footer ==")
        for l in before:
            print("  ", l)
        repl.send("/models deepseek-v4-pro")
        time.sleep(5)
        after = footer_lines(log)[-4:]
        print("== 切换后 footer ==")
        for l in after:
            print("  ", l)
        repl.close()
    finally:
        print("stop rc=", daemon("stop").returncode)


if __name__ == "__main__":
    main()
