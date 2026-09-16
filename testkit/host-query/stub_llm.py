#!/usr/bin/env python3
"""宿主查询黑盒测具的桩 LLM(OpenAI 兼容 SSE)。

用户消息含 HOSTPROBE 且 tools 里有 host_probe → 发一次 host_probe 工具调用;
下一请求的最后一条是 tool 结果 → 把结果原样当正文回出来;其余一律回「ok」。
"""
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("STUB_PORT", "18496"))


def content_text(message):
    content = message.get("content")
    if isinstance(content, list):
        return "".join(part.get("text", "") for part in content if isinstance(part, dict))
    return content or ""


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        messages = body.get("messages", [])
        tools = [t.get("function", {}).get("name") for t in body.get("tools", []) if isinstance(t, dict)]
        # Miyu 在用户消息后面还会追加运行时戳等 system 消息:从后往前找第一条非 system。
        last = next((m for m in reversed(messages) if m.get("role") != "system"), {})
        log = os.environ.get("STUB_LOG")
        if log:
            with open(log, "a", encoding="utf-8") as f:
                f.write(json.dumps({"roles": [m.get("role") for m in messages], "last": last.get("role"),
                                    "tools": tools, "last_text": content_text(last)[:80]}, ensure_ascii=False) + "\n")
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Connection", "close")
        self.end_headers()
        base = {"id": "stub", "object": "chat.completion.chunk", "created": 0, "model": body.get("model", "stub")}

        def sse(payload):
            self.wfile.write(b"data: " + json.dumps(payload, ensure_ascii=False).encode() + b"\n\n")

        # 人格提醒等注入也是 user 角色:暗号在任意 user 消息里找,工具结果看暗号之后有没有 tool 消息。
        probe_index = max((i for i, m in enumerate(messages) if m.get("role") == "user" and "HOSTPROBE" in content_text(m)), default=None)
        tool_result = next((m for m in reversed(messages[probe_index + 1:]) if m.get("role") == "tool"), None) if probe_index is not None else None
        if tool_result is not None:
            sse({**base, "choices": [{"index": 0, "delta": {"role": "assistant", "content": content_text(tool_result)}, "finish_reason": None}]})
            sse({**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
        elif probe_index is not None and "host_probe" in tools:
            sse({**base, "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [{
                "index": 0, "id": "call_probe", "type": "function",
                "function": {"name": "host_probe", "arguments": "{}"},
            }]}, "finish_reason": None}]})
            sse({**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})
        else:
            sse({**base, "choices": [{"index": 0, "delta": {"role": "assistant", "content": "ok"}, "finish_reason": None}]})
            sse({**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
