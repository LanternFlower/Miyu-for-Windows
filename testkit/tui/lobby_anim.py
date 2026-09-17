#!/usr/bin/env python3
"""空会话大厅的动画在斜杠命令前后、以及面板开着的时候都要一直走。

每 0.4s 抓一次星空那几行(0..7),连续 6 次里看有几次和上一次不同。基线应 6/6;
/session /effort /models 面板开着时至少 4/6(以前是 0/6:面板循环从不推动画);
关掉面板、/config 按 q 退出后回到大厅都应回到 ≥5/6。

和 session_picker.py 同一套骨架:一次性 MIYU_HOME、桩模型、带光标应答的 PTY。
Run: python3 testkit/tui/lobby_anim.py --binary /absolute/path/to/miyu
"""

import argparse
import codecs
import os
import select
import socket
import tempfile
import time
from pathlib import Path


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    os.environ.pop("MIYU_DIRECT", None)
    sandbox = Path(tempfile.mkdtemp(prefix="miyu-lobby-anim-"))
    os.environ.update(
        MIYU_HOME=str(sandbox / "home"),
        MIYU_TUI_RUNTIME=str(sandbox / "run"),
        MIYU_TUI_PORT=str(free_port()),
        STUB_PORT=str(free_port()),
        OUT=str(sandbox / "out"),
    )
    import round26 as q
    import pyte

    h = q.h
    h.BIN = args.binary.resolve()
    h.kill_stale_daemon = lambda: None
    h.EDIT_FILE = sandbox / "edit.txt"
    h.COLS, h.ROWS = 100, 32
    processes = []
    try:
        stub, daemon, tui, master, sink = q.start({"STUB_REPLY": "BODY"})
        processes = [tui, daemon, stub]
        screen = pyte.Screen(h.COLS, h.ROWS)
        stream = pyte.Stream(screen)
        decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        stream.feed(decoder.decode(bytes(sink)))
        screen.write_process_input = lambda data: (
            os.write(master, data.encode()) if data.endswith("R") else None
        )

        def lines():
            return ["".join(screen.buffer[y][x].data for x in range(h.COLS)).rstrip()
                    for y in range(h.ROWS)]

        def pump(seconds):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.05)[0]:
                    chunk = os.read(master, 65536)
                    if not chunk:
                        break
                    sink.extend(chunk)
                    stream.feed(decoder.decode(chunk))

        def stars():
            return "\n".join(lines()[0:7])

        def state():
            actual = lines()
            lobby = any("██" in line for line in actual)
            return dict(
                lobby=lobby,
                menu=any("Enter" in line and ("取消" in line or "完成" in line or "确认" in line) for line in actual),
                config=any("MIYU 配置" in line for line in actual) and not lobby,
                footer=any("stub-model" in line for line in actual),
            )

        def wait_for(name, predicate, timeout=10):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                pump(0.1)
                if predicate(state()):
                    return
            (h.OUT / f"{name}.txt").write_text("\n".join(lines()))
            raise AssertionError(f"{name}: expected screen not reached. See {h.OUT}")

        def measure(name, at_least):
            previous = stars()
            changed = 0
            for _ in range(6):
                pump(0.4)
                current = stars()
                if current != previous:
                    changed += 1
                previous = current
            print(f"  {name}: {changed}/6 star frames changed")
            if changed < at_least:
                (h.OUT / f"{name}.txt").write_text("\n".join(lines()))
                raise AssertionError(f"{name}: animation stalled ({changed}/6 < {at_least}). See {h.OUT}")

        wait_for("lobby", lambda s: s["footer"] and s["lobby"])
        measure("baseline", 5)
        def measure_while_navigating(name, at_least):
            # 按住 j/k 在面板里换行:按键比 40ms 一拍还密,节拍要按时刻算才推得出帧
            # (以前按「等满 40ms 没按键」算,扫光一顿一顿——用户实测)。
            previous = stars()
            changed = 0
            for step in range(6):
                for _ in range(8):
                    os.write(master, b"j" if step % 2 == 0 else b"k")
                    pump(0.05)
                current = stars()
                if current != previous:
                    changed += 1
                previous = current
            print(f"  {name}: {changed}/6 star frames changed while navigating")
            if changed < at_least:
                (h.OUT / f"{name}.txt").write_text("\n".join(lines()))
                raise AssertionError(f"{name}: animation stalled while navigating ({changed}/6 < {at_least}). See {h.OUT}")

        for command in ("/session", "/effort", "/models"):
            os.write(master, b"\x15" + command.encode() + b"\r")
            wait_for(f"{command}-open", lambda s: s["menu"])
            pump(0.3)
            measure(f"{command}-open", 4)
            if command == "/session":
                measure_while_navigating("/session-navigating", 4)
            os.write(master, b"\x1b")
            wait_for(f"{command}-closed", lambda s: not s["menu"] and s["lobby"])
            pump(0.3)
            measure(f"{command}-closed", 5)
        os.write(master, b"\x15/config\r")
        wait_for("config-open", lambda s: s["config"])
        pump(0.3)
        os.write(master, b"q")
        wait_for("config-closed", lambda s: s["lobby"] and s["footer"] and not s["config"])
        pump(0.5)
        measure("after-config", 5)
        print(f"PASS: lobby animation keeps running through panels and /config. Artifacts: {h.OUT}")
    finally:
        if "sink" in locals():
            (h.OUT / "lobby-anim.raw").write_bytes(sink)
        q.stop(*processes)


if __name__ == "__main__":
    main()
