#!/usr/bin/env python3
"""在一个会应答能力查询的假终端里跑 chafa，看它选了什么输出格式。

复刻 src/tools/vision/print.rs 的调用形态：stdout/stderr 继承到 PTY，
stdin 可选 /dev/null（Miyu 现状）或 PTY 本身（控制终端）。
父进程扮演终端：收到查询就按「xterm + sixel」的身份应答。
"""
import os, pty, sys, select, subprocess, fcntl, termios, struct, time, re, zlib

KITTY = os.environ.get("FAKE_KITTY") == "1"


def ensure_image(path="big.png", size=64):
    """造一张有渐变的测试图，省掉对外部素材的依赖。"""
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), path)
    if os.path.exists(path):
        return path

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    raw = b""
    for y in range(size):
        row = bytes()
        for x in range(size):
            row += bytes([(x * 255) // size, (y * 255) // size, 128])
        raw += b"\x00" + row
    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw))
           + chunk(b"IEND", b""))
    with open(path, "wb") as handle:
        handle.write(png)
    return path


def respond(chunk: bytes) -> bytes:
    out = b""
    if b"\x1b[c" in chunk or b"\x1b[0c" in chunk:
        out += b"\x1b[?63;1;2;4;6;9;15;22c"   # 4 = sixel
    if b"\x1b[>c" in chunk or b"\x1b[>0c" in chunk:
        out += b"\x1b[>41;389;0c"
    if b"\x1b[>q" in chunk:
        out += b"\x1bP>|XTerm(389)\x1b\\"
    if b";1;0S" in chunk:
        out += b"\x1b[?1;0;1000;1000S"
    if b"\x1b_G" in chunk:
        m = re.search(rb"\x1b_G[^\x1b]*i=(\d+)", chunk)
        ident = m.group(1) if m else b"1"
        if KITTY:
            out += b"\x1b_Gi=" + ident + b";OK\x1b\\"
    if b"\x1b[6n" in chunk:
        out += b"\x1b[1;1R"
    if b"\x1b[16t" in chunk:            # cell size in pixels
        out += b"\x1b[6;20;10t"
    if b"\x1b[14t" in chunk:            # window size in pixels
        out += b"\x1b[4;480;800t"
    return out


def run(args, stdin_mode="null", cols=80, rows=24, xpixel=800, ypixel=480, timeout=8):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ,
                struct.pack("HHHH", rows, cols, xpixel, ypixel))
    if stdin_mode == "null":
        stdin_fd = os.open("/dev/null", os.O_RDONLY)
    else:                                # "tty": 和 stdout 同一个 PTY
        stdin_fd = os.dup(slave)

    def child_setup():
        os.setsid()
        fcntl.ioctl(0 if stdin_mode == "tty" else slave, termios.TIOCSCTTY, 0)

    proc = subprocess.Popen(
        args, stdin=stdin_fd, stdout=slave, stderr=slave,
        preexec_fn=child_setup, close_fds=True,
    )
    os.close(slave)
    os.close(stdin_fd)

    buf = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        r, _, _ = select.select([master], [], [], 0.15)
        if r:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            buf += chunk
            reply = respond(chunk)
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
        proc.kill()
        proc.wait()
    os.close(master)
    return proc.returncode, buf


def classify(out: bytes) -> str:
    """按真图/退化 两类判定输出格式。"""
    if re.search(rb"\x1bP[0-9;]*q", out):
        return "sixel"
    if b"\x1b_G" in out:
        return "kitty"
    if b"\x1b]1337;File=" in out:
        return "iterm"
    if b"\x1b[48;2;" in out or b"\x1b[48;5;" in out or any(
        ch in out for ch in "▀▄█░▒▓".encode()
    ):
        return "symbols"
    return "?"
