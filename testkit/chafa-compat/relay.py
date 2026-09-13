#!/usr/bin/env python3
"""在真终端和被测程序之间插一层 PTY，把两个方向的字节都录下来。

用来回答「Miyu 打完图之后到底往终端写了什么」——sixel 发出去了、终端也画了，
可屏幕上却是空的，那就只能看字节。屏幕上的观感不变（本层是透明的），录下来
的 `relay-*.log` 交给分析脚本。

    testkit/chafa-compat/relay.py ~/.cache/miyu-chafa-sandbox/miyu-sb normal

录两份：
    relay-out.log   被测程序 → 终端（sixel、清屏、光标移动都在这里）
    relay-in.log    终端 → 被测程序（按键，以及 CPR/DA1 这些查询的应答）
每条记录带时间戳，好算终端应答花了多久。
"""
import os, pty, sys, select, signal, struct, fcntl, termios, tty, time, errno

# --send-after <秒> <一行文字>：到点自动敲进去，无人值守复现用。可给多组。
# --quit-after <秒>：到点结束录制。
SENDS = []
QUIT_AFTER = None
argv = sys.argv[1:]
while argv and argv[0].startswith("--"):
    flag = argv.pop(0)
    if flag == "--send-after":
        SENDS.append((float(argv.pop(0)), argv.pop(0)))
    elif flag == "--quit-after":
        QUIT_AFTER = float(argv.pop(0))
    else:
        print(f"不认识的选项 {flag}")
        raise SystemExit(2)
sys.argv = [sys.argv[0]] + argv

if len(sys.argv) < 2:
    print(__doc__)
    raise SystemExit(2)

OUT = open("relay-out.log", "wb")
IN = open("relay-in.log", "wb")
START = time.time()


def stamp(handle, data, tag):
    handle.write(f"\n--- {tag} +{time.time() - START:.3f}s {len(data)}B ---\n".encode())
    handle.write(data)
    handle.flush()


def winsize(fd):
    try:
        return fcntl.ioctl(fd, termios.TIOCGWINSZ, b"\0" * 8)
    except OSError:
        return struct.pack("HHHH", 24, 80, 0, 0)


pid, master = pty.fork()
if pid == 0:
    os.execvp(sys.argv[1], sys.argv[1:])

fcntl.ioctl(master, termios.TIOCSWINSZ, winsize(0))
saved = None
if os.isatty(0):
    saved = termios.tcgetattr(0)
    tty.setraw(0)
    # 输出端的换行翻译留着，和 Miyu 自己做的一样（restore_output_processing）
    attrs = termios.tcgetattr(0)
    attrs[1] |= termios.OPOST | termios.ONLCR
    termios.tcsetattr(0, termios.TCSANOW, attrs)

signal.signal(signal.SIGWINCH,
              lambda *_: fcntl.ioctl(master, termios.TIOCSWINSZ, winsize(0)))

pending = sorted(SENDS)
try:
    while True:
        now = time.time() - START
        while pending and pending[0][0] <= now:
            _, line = pending.pop(0)
            os.write(master, line.encode() + b"\r")
            stamp(IN, line.encode() + b"\r", "自动注入")
        if QUIT_AFTER is not None and now >= QUIT_AFTER:
            break
        try:
            ready, _, _ = select.select([0, master], [], [], 0.2)
        except InterruptedError:
            continue
        if master in ready:
            try:
                data = os.read(master, 65536)
            except OSError as err:
                if err.errno == errno.EIO:
                    break
                raise
            if not data:
                break
            os.write(1, data)
            stamp(OUT, data, "程序→终端")
        if 0 in ready:
            data = os.read(0, 65536)
            if not data:
                break
            os.write(master, data)
            stamp(IN, data, "终端→程序")
finally:
    if saved is not None:
        termios.tcsetattr(0, termios.TCSADRAIN, saved)
    OUT.close()
    IN.close()
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass
    print(f"\r\n录好了：{os.path.abspath('relay-out.log')} / relay-in.log")
