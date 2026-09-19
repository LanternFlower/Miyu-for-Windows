#!/usr/bin/env python3
"""把 opencode CLI 指到本地，抓它发往 Zen 的真实请求头。

用法:
    python3 capture_headers.py [端口]

它起一个假的 OpenAI 兼容端点（/v1/models 与 /v1/chat/completions），
把每次请求的全部头按到达顺序打印出来，然后回一条最短的流式应答让 CLI 正常收尾。

必须是 `ThreadingHTTPServer`：`protocol_version` 声明了 HTTP/1.1，连接默认
keep-alive，而单线程的 `HTTPServer` 会一直卡在第一条连接上，后面的请求全躺在
backlog 里。opencode 先用一条连接拉模型列表、再开一条发对话，正好踩死——
09-20 连续四次「一个请求都没抓到」就是这个。自检见 capture_headers_selftest.py。
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8791


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _dump(self):
        print(f"\n===== {self.command} {self.path} =====", flush=True)
        for name, value in self.headers.items():
            print(f"{name}: {value}", flush=True)

    def do_GET(self):
        self._dump()
        body = json.dumps(
            {"data": [{"id": "claude-sonnet-4-5", "object": "model"}]}
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        self._dump()
        try:
            print("body.model =", json.loads(raw).get("model"), flush=True)
        except Exception:
            pass
        chunks = [
            {
                "id": "c1",
                "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": {"content": "ok"}}],
            },
            {
                "id": "c1",
                "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            },
        ]
        body = "".join(f"data: {json.dumps(c)}\n\n" for c in chunks) + "data: [DONE]\n\n"
        body = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    print(f"listening on http://127.0.0.1:{PORT}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
