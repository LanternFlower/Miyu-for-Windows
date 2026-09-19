#!/usr/bin/env python3
"""正在滚动思考的时候点开它，屏幕不该出现两份。

用户 09-19：「AI 在滚动思考渲染，我再点击展开它，这个时候就开始鬼畜了——展开
后的在渲染，滚动区域也还在渲染。」

根因在缓冲那侧：转轮为了不闪，一帧只重写变了的行；滚动思考窗的第一行每帧都在
变（转轮、秒数）而后面几行常常没变，于是那一帧**只带块的开始标记、不带结束
标记**。缓冲照旧把跨度清成 `start..start`，块停在半开状态，展开层算出「要替换
0 行」，于是把展开内容插进去而不是替换掉——同一段思考出现两份，还随每帧长短
乱跳。

判据就按用户看到的写：**抬头只能有一处**、展开之后行数只多不少。

跑法：

    cargo build
    python3 testkit/tui/think_expand.py

产物在 ~/.cache/miyu-think-expand/。

**这些 TUI 走查只能一个一个跑**：共用同一个 `MIYU_HOME` 和桩模型端口。
"""

import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-think-expand"))
# 思考正文要比滚动窗（默认 10 行）长得多，而且吐得够慢，才点得进去。
LINES = [f"第 {i} 行思考：这一行是为了把滚动窗喂满而写的占位内容。" for i in range(1, 41)]
REASONING = "\n".join(LINES)
SCROLL_WINDOW = 10


def heads(rows):
    return sum(1 for line in rows if "思考中" in line)


def body_rows(rows):
    return sum(1 for line in rows if "行思考：" in line)


def find_row(rows, needle):
    for index, line in enumerate(rows):
        if needle in line:
            return index
    return None


def tap(master, head, gap=0.0):
    """原地点一下。`gap` = 按下到松开之间隔多久（秒），0 是瞬时。

    真人按一下大约 50–150ms，所以两种都要验：瞬时的和带间隔的。流式重画期间
    有间隔的点击一度被当成拖动（`selection_finish` 返回 None），那样一下都点
    不开——这条判据就是盯它的。
    """
    os.write(master, f"\x1b[<0;7;{head + 1}M".encode())
    if gap:
        time.sleep(gap)
    os.write(master, f"\x1b[<0;7;{head + 1}m".encode())


def main():
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    h.write_config()
    h.kill_stale_daemon()

    stub = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(h.STUB_PORT),
            STUB_REASONING="1",
            STUB_REASONING_TEXT=REASONING,
            STUB_CHUNK_CHARS="6",
            STUB_CHUNK_SLEEP="0.06",
        ),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = tui = None
    report = {}
    try:
        if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
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

        os.write(master, "讲讲你的思路".encode())
        h.drain(master, 0.5, sink)
        os.write(master, b"\r")

        # 等滚动窗铺开
        deadline = time.time() + 25
        head = None
        while time.time() < deadline:
            h.drain(master, 0.4, sink)
            rows = h.render(bytes(sink))
            head = find_row(rows, "思考中")
            if head is not None and find_row(rows, "第 6 行思考") is not None:
                break
        rows = h.render(bytes(sink))
        (OUT / "01-scrolling.txt").write_text("\n".join(rows), encoding="utf-8")
        report["滚动窗出来了"] = head is not None and body_rows(rows) > 0
        report["滚动窗不超过设定行数"] = body_rows(rows) <= SCROLL_WINDOW
        if head is None:
            raise SystemExit(1)

        tap(master, head)

        # 点开之后连看几帧：每一帧都只能有一个抬头，行数只多不少
        counts = []
        double = 0
        for shot in range(6):
            h.drain(master, 0.7, sink)
            rows = h.render(bytes(sink))
            (OUT / f"02-expanded-{shot}.txt").write_text("\n".join(rows), encoding="utf-8")
            double += heads(rows) > 1
            counts.append(body_rows(rows))
        report["展开后抬头只有一处"] = double == 0
        report["确实展开了"] = max(counts) > SCROLL_WINDOW
        report["行数不回跳"] = all(b >= a for a, b in zip(counts, counts[1:]))
        report["_行数"] = counts

        # 再点一次该收回去
        rows = h.render(bytes(sink))
        head = find_row(rows, "思考中")
        if head is not None:
            tap(master, head)
            h.drain(master, 1.2, sink)
            rows = h.render(bytes(sink))
            (OUT / "03-collapsed.txt").write_text("\n".join(rows), encoding="utf-8")
            report["再点一次收回滚动窗"] = (
                heads(rows) == 1 and body_rows(rows) <= SCROLL_WINDOW
            )

        # 真人手速：按下到松开隔 80ms。流式重画期间照样要点得开、也不能出两份。
        head = find_row(h.render(bytes(sink)), "思考中")
        if head is not None:
            tap(master, head, gap=0.08)
            h.drain(master, 1.2, sink)
            rows = h.render(bytes(sink))
            (OUT / "04-human-click.txt").write_text("\n".join(rows), encoding="utf-8")
            report["真人手速点得开"] = body_rows(rows) > SCROLL_WINDOW
            report["真人手速也不出两份"] = heads(rows) == 1
    finally:
        for process in (tui, daemon, stub):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()

    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    checks = {k: v for k, v in report.items() if not k.startswith("_")}
    passed = 0
    for name, ok in checks.items():
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
    print(f"\n{passed}/{len(checks)} passed  （正文行数：{report.get('_行数')}）")
    return 0 if passed == len(checks) else 1


if __name__ == "__main__":
    raise SystemExit(main())
