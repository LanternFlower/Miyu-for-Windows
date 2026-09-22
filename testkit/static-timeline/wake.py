#!/usr/bin/env python3
"""shellhook 的后台任务跟进：daemon 往发起它的终端回写那一轮，长相要和
shellhook 自己那一轮一样（静态时间线），不是另一套手搓的行渲染。

pty 里跑一个真的 bash（daemon 的三道闸要它：shell 活着、挂在这个 tty、停在提示
符），在里面用 shellhook 的形态触发一轮（模型派一条后台命令），等提示符回来，
再等 daemon 把跟进那一轮写回来。

    python3 testkit/static-timeline/wake.py
"""
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import sys
import termios
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

PS1 = "WAKE-PROMPT> "


def drain(master, seconds, sink):
    deadline = time.time() + seconds
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.1)
        if not ready:
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError:
            return
        if not chunk:
            return
        sink.extend(chunk)


def wait_for(master, sink, predicate, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        drain(master, 0.2, sink)
        if predicate(bytes(sink)):
            return True
    return False


def main():
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    if h.HOME.exists():
        import shutil
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    h.OUT.mkdir(parents=True, exist_ok=True)
    h.write_config()
    h.kill_stale_daemon()
    stub = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(h.STUB_PORT),
            STUB_REASONING="1",
            STUB_BACKGROUND="1",
            # 命令里带 STUB_BG：跟进那一轮桩模型会再派一条（BG2），于是回写里
            # 有一步工具可看；第二次跟进才是纯正文。
            STUB_BACKGROUND_COMMAND="echo STUB_BG; sleep 2; echo BGDONE",
        ),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = None
    bash = None
    report = {}
    try:
        if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        daemon = subprocess.Popen(
            [str(h.BIN), "__daemon", "--port", str(h.PORT)],
            env=h.ENV, cwd=str(h.HOME),
            stdout=(h.OUT / "wake-daemon.log").open("a"), stderr=subprocess.STDOUT,
        )
        if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
            print("! daemon 没起来", file=sys.stderr)
            return 2
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", h.ROWS, h.COLS, 0, 0))

        def child_setup():
            os.setsid()
            fcntl.ioctl(1, termios.TIOCSCTTY, 0)

        bash = subprocess.Popen(
            ["bash", "--norc", "--noprofile", "-i"],
            stdin=slave, stdout=slave, stderr=slave,
            env=dict(h.ENV, PS1=PS1, TERM="xterm-256color", HOME=str(h.HOME)),
            cwd=str(h.HOME), preexec_fn=child_setup, close_fds=True,
        )
        os.close(slave)
        sink = bytearray()
        assert wait_for(master, sink, lambda b: PS1.encode() in b, 10.0), "bash 没给提示符"
        # shellhook 的形态：正文从管道进，stdout/stderr 是这个 tty，父进程是 bash。
        command = f"printf '%s' '{h.PROMPT}' | {h.BIN} --shell-intercept --shell bash --stdin\n"
        os.write(master, command.encode())
        # 第一轮跑完、提示符回来（提示符至少出现两次：开头一次、跑完一次）。
        ok = wait_for(master, sink, lambda b: b.count(PS1.encode()) >= 2 and "走查的回复".encode() in b, 60.0)
        report["first_turn_finished"] = ok
        # 后台命令 2 秒跑完 → daemon 唤醒一轮 → 写回这个 tty。
        ok = wait_for(master, sink, lambda b: "后台任务跟进".encode() in b, 60.0)
        report["writeback_header_seen"] = ok
        # 跟进那一轮：桩模型先想一句、再派一条后台命令、然后回正文。
        ok = wait_for(
            master, sink,
            lambda b: b.count("走查的回复".encode()) >= 2,
            60.0,
        )
        report["writeback_reply_seen"] = ok
        drain(master, 2.0, sink)
        screen = h.render(bytes(sink))
        (h.OUT / "wake-screen.txt").write_text("\n".join(screen) + "\n", encoding="utf-8")
        (h.OUT / "wake-raw.bin").write_bytes(bytes(sink))
        text = "\n".join(screen)
        header = next((i for i, l in enumerate(screen) if "后台任务跟进" in l), None)
        after = screen[header + 1:] if header is not None else []
        # 抬头和 REPL 里的一样：暗色齿轮打头，没有老样式的 `✦ Miyu` / `∴`。
        report["header_is_gear_line"] = header is not None and screen[header].startswith("⚙")
        report["no_legacy_markers"] = "✦ Miyu" not in text and "∴" not in text
        # 时间线：思考那一步、工具那一步（图标在第 2 列），正文另起一段。
        report["thought_step_in_timeline"] = any(
            l.startswith("  ") and "已思考" in l for l in after
        )
        # 跟进那一轮的两段正文各占各的行，没有粘在转轮那一行后面。
        report["reply_lines_clean"] = all(
            "思考中" not in l for l in after if "走查的回复" in l
        )
        report["reply_after_timeline"] = any("走查的回复" in l for l in after)
    finally:
        for process in (bash, daemon, stub):
            if process is None:
                continue
            try:
                process.terminate()
                process.wait(timeout=5)
            except Exception:
                try:
                    process.kill()
                except Exception:
                    pass
    (h.OUT / "wake-report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    failed = [key for key, value in report.items() if value is not True]
    for key, value in report.items():
        print(f"  {'✓' if value is True else '✗'} {key}: {value}")
    print(f"产物：{h.OUT}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
