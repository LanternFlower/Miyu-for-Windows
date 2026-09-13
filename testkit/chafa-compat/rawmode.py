#!/usr/bin/env python3
"""复刻 REPL 的真实形态：终端处于 raw 模式时，chafa 还能探测出 sixel 吗？

Miyu 打图的时刻，终端已经被 REPL 设成 raw（crossterm enable_raw_mode 改的
是这个终端设备的 termios）。chafa 用 ctty 探测时要自己设 termios 再恢复。
这一层此前没测过。
"""
import os, pty, sys, select, subprocess, fcntl, termios, struct, time, re, tty

BASE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, BASE)
from pty_probe import ensure_image  # noqa: E402

IMG = ensure_image()


def respond(chunk, sixel=True, slow=0.0):
    out = b""
    if b"\x1b[c" in chunk or b"\x1b[0c" in chunk:
        out += b"\x1b[?63;1;2;4;6;9;15;22c" if sixel else b"\x1b[?62;1;2;6;9;15;22c"
    if b"\x1b[>c" in chunk or b"\x1b[>0c" in chunk:
        out += b"\x1b[>41;389;0c"
    if b"\x1b[>q" in chunk:
        out += b"\x1bP>|XTerm(389)\x1b\\"
    if b";1;0S" in chunk:
        out += b"\x1b[?1;0;1000;1000S" if sixel else b"\x1b[?1;3;0S"
    if b"\x1b[6n" in chunk:
        out += b"\x1b[1;1R"
    if b"\x1b[16t" in chunk:
        out += b"\x1b[6;20;10t"
    if b"\x1b[14t" in chunk:
        out += b"\x1b[4;480;800t"
    if out and slow:
        time.sleep(slow)
    return out


def run(argv, raw=False, sixel=True, slow=0.0, timeout=12):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 800, 480))
    if raw:
        tty.setraw(slave)                      # ← REPL 的 enable_raw_mode
    devnull = os.open("/dev/null", os.O_RDONLY)

    def setup():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    t0 = time.time()
    proc = subprocess.Popen(argv, stdin=devnull, stdout=slave, stderr=slave,
                            preexec_fn=setup, close_fds=True)
    os.close(slave); os.close(devnull)
    buf = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        r, _, _ = select.select([master], [], [], 0.1)
        if r:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            buf += chunk
            reply = respond(chunk, sixel=sixel, slow=slow)
            if reply:
                try:
                    os.write(master, reply)
                except OSError:
                    pass
        elif proc.poll() is not None:
            r, _, _ = select.select([master], [], [], 0.2)
            if not r:
                break
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill(); proc.wait()
    elapsed = time.time() - t0
    # raw 还在吗？（chafa 恢复 termios 是否把 REPL 的 raw 弄丢）
    try:
        attrs = termios.tcgetattr(master)
        still_raw = not (attrs[3] & termios.ICANON) and not (attrs[3] & termios.ECHO)
    except Exception:
        still_raw = None
    os.close(master)
    return proc.returncode, buf, elapsed, still_raw


def classify(out):
    if re.search(rb"\x1bP[0-9;]*q", out): return "sixel"
    if b"\x1b_G" in out: return "kitty"
    if b"\x1b]1337;File=" in out: return "iterm"
    if b"\x1b[48;2;" in out or b"\x1b[48;5;" in out: return "symbols"
    return "?"


os.environ["TERM"] = "xterm-256color"
for k in ("KITTY_WINDOW_ID", "KITTY_PID", "TERM_PROGRAM", "KITTY_INSTALLATION_DIR"):
    os.environ.pop(k, None)

NOW = ["--probe-mode", "ctty", "--polite", "on", "--relative", "off"]
CASES = [
    ("线上现状参数   终端非 raw   xterm 报 sixel", NOW, False, True, 0.0),
    ("线上现状参数   终端 RAW     xterm 报 sixel", NOW, True,  True, 0.0),
    ("裸调用         终端 RAW     xterm 报 sixel", [],  True,  True, 0.0),
    ("--polite on    终端 RAW     xterm 报 sixel", ["--polite", "on"], True, True, 0.0),
    ("线上现状参数   终端 RAW     xterm 不报 sixel", NOW, True, False, 0.0),
    ("线上现状参数   终端 RAW     终端应答慢 0.3s", NOW, True, True, 0.3),
    ("线上现状参数   终端 RAW     终端完全不应答", NOW, True, None, 0.0),
]

print(f"{'场景':<46}{'格式':>8}{'耗时s':>8}{'退出':>6}  raw是否还在")
print("-" * 84)
for label, extra, raw, sixel, slow in CASES:
    if sixel is None:
        saved = globals()["respond"]
        globals()["respond"] = lambda c, **k: b""
        code, out, el, still = run(["chafa"] + extra + ["--size", "20x10", IMG],
                                   raw=raw, timeout=14)
        globals()["respond"] = saved
    else:
        code, out, el, still = run(["chafa"] + extra + ["--size", "20x10", IMG],
                                   raw=raw, sixel=sixel, slow=slow)
    print(f"{label:<46}{classify(out):>8}{el:>8.2f}{code:>6}  {still}")
