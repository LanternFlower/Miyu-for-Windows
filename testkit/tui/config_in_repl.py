#!/usr/bin/env python3
"""全屏 TUI 里的 `/config`：设置界面借用 REPL 的备用屏，退出时要原样还回去。

独立 `miyu config` 自己进备用屏，这条路不是——`run_embedded` 只擦屏不进屏，
交接稍有差池就会看到 shell 画面闪一下、或者退出后留一片设置界面的残影。

    cargo build
    python3 testkit/tui/config_in_repl.py

跟别的 TUI 走查一样**只能单独跑**：共用同一个 MIYU_HOME 与端口。
"""

import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

results = []


def check(ok, name, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{(' — ' + detail) if detail and not ok else ''}")


def wait_screen(master, sink, *words, timeout=10.0, settle=0.8):
    """等屏幕出现这些词。设置界面每 30ms 一帧，等不到「输出停下来」。"""
    deadline = time.time() + timeout
    while time.time() < deadline:
        h.drain(master, 0.1, sink)
        screen = "\n".join(h.render(bytes(sink)))
        if all(word in screen for word in words):
            h.drain(master, settle, sink)
            return "\n".join(h.render(bytes(sink)))
    return None


def main():
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    h.OUT.mkdir(parents=True, exist_ok=True)
    h.write_config()
    h.kill_stale_daemon()

    daemon = subprocess.Popen(
        [str(h.BIN), "__daemon", "--port", str(h.PORT)],
        env=h.ENV, cwd=str(h.HOME),
        stdout=(h.OUT / "config-in-repl-daemon.log").open("w"),
        stderr=subprocess.STDOUT,
    )
    tui = None
    master = None
    try:
        if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
            print("! daemon 没起来", file=sys.stderr)
            return 2
        tui, master = h.spawn_tui()
        sink = bytearray()
        h.drain(master, 3.0, sink)
        check(b"\x1b[?1049h" in bytes(sink), "REPL 进了备用屏")

        before = "\n".join(h.render(bytes(sink)))
        (h.OUT / "config-in-repl-lobby.txt").write_text(before)

        # ── 开设置 ──
        os.write(master, b"/config\r")
        screen = wait_screen(master, sink, "供应商和模型", "保存并退出")
        check(screen is not None, "/config 开得出设置界面")
        screen = screen or "\n".join(h.render(bytes(sink)))
        (h.OUT / "config-in-repl-open.txt").write_text(screen)
        check("█" in screen, "设置界面带 banner")
        check("◉ 配置" in screen, "设置界面带面包屑")
        # 借来的备用屏：不许再进一次，否则中间会闪一下 shell 画面。
        opens = bytes(sink).count(b"\x1b[?1049h")
        check(opens == 1, "没有再进一次备用屏", f"进了 {opens} 次")

        # ── 动画在这条路上也得动 ──
        first = "\n".join(h.render(bytes(sink)))
        h.drain(master, 0.8, sink)
        check(first != "\n".join(h.render(bytes(sink))), "嵌在 REPL 里也在动")

        # ── 退设置：REPL 自己整屏画回来 ──
        os.write(master, b"\x1b")
        screen = wait_screen(master, sink, "普通模式", timeout=10.0)
        check(screen is not None, "Esc 退得回 REPL")
        screen = screen or "\n".join(h.render(bytes(sink)))
        (h.OUT / "config-in-repl-back.txt").write_text(screen)
        check("保存并退出" not in screen and "◉ 配置" not in screen,
              "退出后没留下设置界面的残影")
        closes = bytes(sink).count(b"\x1b[?1049l")
        check(closes == 0, "没把 REPL 的备用屏退掉", f"退了 {closes} 次")
    finally:
        if tui is not None:
            tui.terminate()
            try:
                tui.wait(timeout=5)
            except subprocess.TimeoutExpired:
                tui.kill()
        if master is not None:
            os.close(master)
        daemon.terminate()
        try:
            daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            daemon.kill()

    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed  ({h.OUT})")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
