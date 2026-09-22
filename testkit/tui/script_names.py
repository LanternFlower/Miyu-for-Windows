#!/usr/bin/env python3
"""脚本工具在全屏 TUI 时间线上要显示成头部里的「显示名称」，不是裸 id。

脚本的显示名只在 daemon 里登记过，TUI / `miyu "…"` 那条路是另一个进程，表是空的
——WebUI 用的是事件里 daemon 算好的 `display_name`，所以只有它对（用户 09-17）。
现在客户端从 `tool.preparing` / `tool.started` 事件里把名字学过来。

    cargo build
    python3 testkit/tui/script_names.py
"""

import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

SCRIPT_ID = "walk_hello"
DISPLAY = "打个招呼走查"
STUB = {
    "STUB_EXTRA_CALLS": json.dumps([
        {"name": "load_tools", "arguments": {"names": [SCRIPT_ID]}},
        {"name": SCRIPT_ID, "arguments": {}},
    ], ensure_ascii=False),
    "STUB_CHUNK_SLEEP": "0.02",
}


def write_script():
    scripts = h.HOME / "config" / "scripts"
    scripts.mkdir(parents=True, exist_ok=True)
    path = scripts / f"{SCRIPT_ID}.sh"
    path.write_text(
        "#!/bin/bash\n"
        f"# 显示名称：{DISPLAY}\n"
        "# 描述：走查用，打印一句问候\n"
        "# 权限：read-only\n"
        "echo '{\"greeting\": \"你好\"}'\n",
        encoding="utf-8",
    )
    path.chmod(0o755)


def main():
    report = {}
    # 脚本要在 daemon 起来之前就在目录里：借写配置那一步顺手放进去。
    original = h.write_config

    def write_config_and_script():
        original()
        write_script()

    h.write_config = write_config_and_script
    stub, daemon, tui, master, sink = r.start(STUB, config_extra={"display": {"fold_timeline": False}})
    try:
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        h.settle(master, sink, quiet=1.5, timeout=60)
        screen = h.render(bytes(sink))
        r.save("script-names", screen)
        steps = [line.strip() for line in screen if DISPLAY in line or SCRIPT_ID in line]
        report["steps"] = steps
        report["script_step_uses_display_name"] = any(
            DISPLAY in line and SCRIPT_ID not in line for line in steps
        )
        report["load_step_uses_display_name"] = any(
            ("加载" in line or "Load" in line) and DISPLAY in line for line in steps
        )
        report["no_bare_script_id_on_timeline"] = not any(SCRIPT_ID in line for line in steps)
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    ok = report.get("script_step_uses_display_name") and report.get("no_bare_script_id_on_timeline")
    print("通过" if ok else "红")
    print("产物：", h.OUT)
