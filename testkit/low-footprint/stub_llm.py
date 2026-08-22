#!/usr/bin/env python3
"""低占用专项的桩 LLM:OpenAI 兼容 SSE,固定回若干 content chunk。

每个请求在 log 文件里记两行 jsonl(start/end + 时间戳),测量脚本据此
划定"流式进行中"的窗口。不做任何路由智能——本测具只量占用,不量语义。
"""
import json
import os
import sys
import time
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("STUB_PORT", "18491"))
CHUNKS = int(os.environ.get("STUB_CHUNKS", "60"))
DELAY_MS = int(os.environ.get("STUB_DELAY_MS", "100"))
LOG = os.environ.get("STUB_LOG", "stub-requests.jsonl")
_lock = threading.Lock()


def log_line(obj):
    with _lock:
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(json.dumps(obj, ensure_ascii=False) + "\n")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        _body = self.rfile.read(length)
        log_line({"event": "start", "t": time.time(), "path": self.path})
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Connection", "close")
        self.end_headers()

        def sse(payload):
            self.wfile.write(b"data: " + json.dumps(payload).encode() + b"\n\n")
            self.wfile.flush()

        base = {
            "id": "stub",
            "object": "chat.completion.chunk",
            "model": "stub-model",
        }
        try:
            for i in range(CHUNKS):
                sse({
                    **base,
                    "choices": [{
                        "index": 0,
                        "delta": {"content": f"词元{i} "},
                        "finish_reason": None,
                    }],
                })
                time.sleep(DELAY_MS / 1000)
            sse({
                **base,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 100, "completion_tokens": CHUNKS},
            })
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except BrokenPipeError:
            pass
        log_line({"event": "end", "t": time.time()})


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"stub llm on 127.0.0.1:{PORT}", flush=True)
    server.serve_forever()
