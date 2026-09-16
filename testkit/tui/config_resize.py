#!/usr/bin/env python3
"""Config menus must repaint on resize and retain editing/dialog state.

Uses an isolated MIYU_HOME, a real PTY, and no model requests or daemon.
Run: python3 testkit/tui/config_resize.py --binary /absolute/path/to/miyu
"""

import argparse
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time
from pathlib import Path

import pyte


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    sandbox = Path(tempfile.mkdtemp(prefix="miyu-config-resize-"))
    home = sandbox / "home"
    (home / "config").mkdir(parents=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{"id": "stub", "display_name": "Stub",
                       "base_url": "http://127.0.0.1:1/v1", "protocol": "openai-chat",
                       "api_key": "stub", "models": ["stub-model"]}],
        "display": {"language": "en"},
        "memory": {"enabled": False},
    }
    (home / "config/config.jsonc").write_text(json.dumps(config))
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 110, 0, 0))

    def setup():
        os.setsid()
        fcntl.ioctl(1, termios.TIOCSCTTY, 0)

    process = subprocess.Popen(
        [str(args.binary.resolve()), "config"], stdin=slave, stdout=slave, stderr=slave,
        cwd=sandbox,
        env=dict(os.environ, MIYU_HOME=str(home), XDG_RUNTIME_DIR=str(sandbox / "run"),
                 TERM="xterm-256color"),
        preexec_fn=setup,
    )
    os.close(slave)
    screen = pyte.Screen(110, 32)
    stream = pyte.ByteStream(screen)
    sink = bytearray()

    def check(name, required, since=0):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.05)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                sink.extend(chunk)
                stream.feed(chunk)
            text = "\n".join(screen.display)
            # A new full paint is required: old screen text is not proof that
            # the resize/key was handled. Config views flush after each frame.
            if b"\x1b[2J" in sink[since:] and all(word in text for word in required):
                if select.select([master], [], [], 0.05)[0]:
                    continue
                (sandbox / f"{name}.txt").write_text(text)
                return
        (sandbox / f"{name}.txt").write_text("\n".join(screen.display))
        raise AssertionError(f"{name}: no new frame containing {required}. See {sandbox}")

    def send(keys, name, required):
        mark = len(sink)
        os.write(master, keys)
        check(name, required, mark)

    def resize(cols, rows, name, required):
        mark = len(sink)
        screen.resize(lines=rows, columns=cols)
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        check(name, required, mark)

    try:
        check("main", ["MIYU CONFIG", "Global settings"])
        resize(78, 24, "main-smaller", ["MIYU CONFIG", "Global settings"])
        resize(130, 40, "main-larger", ["MIYU CONFIG", "Global settings"])
        # The plain message dialog must stay open on resize, then consume a
        # real key. This provider deliberately has no image model.
        send(b"jj\r", "message", ["No models support image input", "Press any key"])
        resize(92, 30, "message-resized", ["No models support image input", "Press any key"])
        send(b"x", "message-dismissed", ["MIYU CONFIG"])
        send(b"j" * 6 + b"\r", "settings", ["GLOBAL SETTINGS", "Maximum tool rounds"])
        send(b"j\r\x1b[H" + b"\x1b[3~" * 20 + b"resizecheck\x1b[D\x1b[D",
             "editing", ["resizecheck"])
        resize(110, 36, "editing-resized", ["GLOBAL SETTINGS", "resizecheck"])
        send(b"Z", "editing-cursor-retained", ["resizecheZck"])
        # Invalid numeric text triggers the separate boxed error dialog.
        send(b"\rq", "error", ["ERROR", "Press any key"])
        resize(88, 28, "error-resized", ["ERROR", "Press any key"])
        send(b"x", "error-dismissed", ["MIYU CONFIG"])
        print(f"PASS: menu resize, message/error stay open, editing text/cursor retained. {sandbox}")
    finally:
        (sandbox / "config.raw").write_bytes(sink)
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        os.close(master)


if __name__ == "__main__":
    main()
