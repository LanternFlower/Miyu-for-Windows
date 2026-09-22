#!/usr/bin/env python3
"""`capture_headers.py` 能不能同时伺候多条连接。

09-20 实录：抓 opencode 2.0.8 的请求时连续四次「一个请求都没收到」，以为是
配置覆盖没生效、是 standalone 起不来、是 opencode 不认 http，全不是——
`capture_headers.py` 用的是单线程 `HTTPServer`，而 `protocol_version` 又声明了
HTTP/1.1，于是连接默认 keep-alive：客户端第一条连接不主动关，`serve_forever`
就一直卡在那条连接上，后面的请求全躺在 backlog 里等到超时。opencode 会先用
一条连接拉模型列表、再开一条发对话，正好踩死。

这个自检把那个场景复现出来：A 连接发完请求**不关**，B 连接再发一条，看 B 能不能
在 3 秒内拿到应答。

    python3 capture_headers_selftest.py
"""

import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
PORT = 8787
TIMEOUT = 3.0


def send(port, body=None, keep_open=False, timeout=TIMEOUT):
    """发一条请求，返回状态行；拿不到就返回 None。keep_open=True 时不关套接字。"""
    sock = socket.create_connection(("127.0.0.1", port), timeout=timeout)
    sock.settimeout(timeout)
    if body is None:
        head = f"GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    else:
        head = (
            f"POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
            f"Content-Type: application/json\r\nContent-Length: {len(body)}\r\n\r\n{body}"
        )
    sock.sendall(head.encode())
    try:
        first = sock.recv(64).decode("utf-8", "replace").splitlines()[0]
    except (socket.timeout, IndexError):
        first = None
    if not keep_open:
        sock.close()
        return first, None
    return first, sock


def main():
    server = subprocess.Popen(
        [sys.executable, str(HERE / "capture_headers.py"), str(PORT)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        stdin=subprocess.DEVNULL,
    )
    held = None
    passed = 0
    total = 2
    try:
        # 端口起来之前别开测。
        for _ in range(50):
            try:
                socket.create_connection(("127.0.0.1", PORT), timeout=0.2).close()
                break
            except OSError:
                time.sleep(0.1)

        first_a, held = send(PORT, body='{"model":"m","messages":[]}', keep_open=True)
        ok_a = bool(first_a and "200" in first_a)
        print(f"{'✅' if ok_a else '❌'} A 连接（keep-alive，不关）拿到应答：{first_a}")
        passed += ok_a

        first_b, _ = send(PORT)
        ok_b = bool(first_b and "200" in first_b)
        print(
            f"{'✅' if ok_b else '❌'} B 连接在 A 还开着时也拿到应答：{first_b}"
            + ("" if ok_b else "  ← 单线程 + keep-alive 自锁")
        )
        passed += ok_b
    finally:
        if held:
            held.close()
        server.terminate()
        server.wait(timeout=5)

    print(f"\n{passed}/{total} passed")
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
