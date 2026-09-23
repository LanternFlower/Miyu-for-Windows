#!/usr/bin/env python3
"""模型报错之后，footer 右边那五柱声波不该留在屏上（用户 09-21）。

`run.cancelled` 那一支 08-20 就为同一个现象补过 `stop_footer_spinner()`，
`run.failed` 那一支漏了：回合以报错收场时最后一帧波浪冻在 footer 上，直到
用户按任意键触发重绘才消失。

起一个只会回 429 的假供应商，让 TUI 真跑一轮撞上去，然后：
  1. 报错落屏后立刻抓一帧——footer 行里不该有波浪字符；
  2. 再按一下键触发重绘，确认波浪本来就会被交互抹掉（说明这是残影不是状态）。

跑法：

    cargo build
    python3 testkit/tui/error_footer_wave.py

产物在 ~/.cache/miyu-error-footer-wave/。

这一条自带家目录和端口（`/tmp/miyu-footer-wave`、18521/18522），可以和别的
TUI 走查同时跑。
"""

import json
import os
import shutil
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

# 自带一套家目录/端口再 import run(它在模块层就按 env 定好了常量)。
# 09-21 实录:照默认值跑会和别的会话正在跑的 TUI 走查抢同一个沙箱,而且开头
# 那下 rmtree 会把人家的家删掉。这条走查不依赖共享状态,就别去挤那一份。
os.environ.setdefault("MIYU_HOME", "/tmp/miyu-footer-wave/home")
os.environ.setdefault("MIYU_TUI_RUNTIME", "/tmp/mx-footer-wave")
os.environ.setdefault("MIYU_TUI_PORT", "18521")

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-error-footer-wave"))
PROMPT = "走查一句"
QUOTA_PORT = int(os.environ.get("QUOTA_PORT", "18522"))
WAVE_GLYPHS = "▁▂▃▄▅▆▇"


class Quota429(BaseHTTPRequestHandler):
    def do_POST(self):
        # 秒失败的话 footer 波浪根本没机会起来，这条走查就成了空断言。
        # 拖住几秒，让回合真的进入「运行中」再报错。
        time.sleep(float(os.environ.get("STALL_SECONDS", "4")))
        self.send_response(429)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"error":{"message":"rate limited","code":"rate_limit_exceeded"}}')

    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"data":[]}')

    def log_message(self, *args):
        pass


def write_config():
    (h.HOME / "config").mkdir(parents=True, exist_ok=True)
    provider = {
        "display_name": "没额度了",
        "base_url": f"http://127.0.0.1:{QUOTA_PORT}/v1",
        "protocol": "openai-chat",
        "api_key": "stub",
        "models": ["broke-model"],
    }
    config = {
        "active_provider": "brokeA",
        "active_provider_models": [{"provider_id": "brokeA", "model": "broke-model"}],
        "providers": [dict(provider, id="brokeA")],
        "memory": {"enabled": False},
    }
    (h.HOME / "config" / "config.jsonc").write_text(
        json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def footer_rows(screen):
    """footer 行：带模式标签和模型名的那一行（可能有多行残留，全都要看）。"""
    return [row for row in screen if "普通" in row and "broke-model" in row]


def main():
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    write_config()
    h.kill_stale_daemon()

    server = ThreadingHTTPServer(("127.0.0.1", QUOTA_PORT), Quota429)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    daemon = tui = None
    report = {}
    try:
        daemon = subprocess.Popen(
            [str(h.BIN), "__daemon", "--port", str(h.PORT)],
            env=h.ENV, cwd=str(h.HOME),
            stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
        )
        if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
            print("! daemon 没起来", file=sys.stderr)
            return 2

        tui, master = h.spawn_tui()
        sink = bytearray()
        h.drain(master, 3.0, sink)
        os.write(master, PROMPT.encode())
        h.drain_until(master, sink, PROMPT, 3.0)
        os.write(master, b"\r")
        # 先看回合中：波浪必须真的在动，否则后面那条断言是空的。
        h.drain(master, 2.0, sink)
        mid = h.render(bytes(sink))
        (OUT / "mid-turn.txt").write_text("\n".join(mid), encoding="utf-8")
        report["回合中 footer 上有声波(前提)"] = any(
            any(g in row for g in WAVE_GLYPHS) for row in footer_rows(mid)
        )
        # 等报错落屏。
        h.drain_until(master, sink, "错误", 60.0)
        h.drain(master, 2.5, sink)
        after_error = h.render(bytes(sink))
        (OUT / "after-error.txt").write_text("\n".join(after_error), encoding="utf-8")

        rows = footer_rows(after_error)
        report["找得到 footer 行"] = bool(rows)
        stuck = [row for row in rows if any(g in row for g in WAVE_GLYPHS)]
        report["报错后 footer 上没有残留的声波"] = not stuck
        if stuck:
            (OUT / "stuck-rows.txt").write_text("\n".join(stuck), encoding="utf-8")
            print("   残留行：" + repr(stuck[0].rstrip()))

        # 交互一下再看：用户说「交互一下它就没了」，这里把那一步也钉住，
        # 免得以后有人把修复做成「靠重绘兜底」。
        os.write(master, b"x")
        h.drain(master, 1.5, sink)
        after_key = h.render(bytes(sink))
        (OUT / "after-key.txt").write_text("\n".join(after_key), encoding="utf-8")
        rows2 = footer_rows(after_key)
        report["交互之后同样没有声波"] = not any(
            any(g in row for g in WAVE_GLYPHS) for row in rows2
        )
    finally:
        server.shutdown()
        for process in (tui, daemon):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()

    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    passed = 0
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
    print(f"\n{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    raise SystemExit(main())
