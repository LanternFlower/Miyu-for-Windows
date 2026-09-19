#!/usr/bin/env python3
"""同一个会话开两个全屏 TUI，现在会发生什么。

用户 09-19 的问题：「A 在流式输出时再开一个 B 进同一个会话，我期望两边看到
一样的内容；两边同时发消息会怎么样？AI 收到的是什么？」

这个脚本**不下结论、只取证**：把现状一条条量出来，供设计取舍。量四件事：

1. B 开起来时，A 正在跑的那一轮，B 看得见吗（正文？用户那句话？还是一片空）；
2. B 在 A 跑着的时候发一条消息，daemon 是**排队**还是**另起一轮**；
3. 最后库里落了几轮、每轮的用户输入是什么——也就是「AI 收到的是什么」；
4. A 那边知不知道 B 发过消息。

跑法（先 cargo build）：

    python3 testkit/tui/two_views.py

产物在 ~/.cache/miyu-two-views/。

**别和别的 TUI 走查并行跑**：共用 MIYU_HOME 和桩端口。
"""

import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("MIYU_BIN", ROOT / "target" / "debug" / "miyu"))
SMOKE = ROOT / "testkit" / "repl-smoke"
HOME = Path(os.environ.get("MIYU_HOME", "/tmp/miyu-two-views/home"))
RUNTIME = os.environ.get("MIYU_TV_RUNTIME", "/tmp/mx-two-views")
PORT = int(os.environ.get("MIYU_TV_PORT", "18461"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18489"))
BASE = f"http://127.0.0.1:{PORT}"
OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-two-views"))
COLS, ROWS = 100, 40
BAR = "┃"
# 等待转轮的字形（`wait_spinner.rs` 的 BRAILLE_FRAMES）。
SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

PROMPT_A = "这是A发的第一句"
PROMPT_B = "这是B发的第二句"
PROMPT_C = "这是A发的第三句"
PROMPT_SHELL = "这是终端一说的"
PROMPT_SHELL2 = "这是终端二说的"
# 回复要够长、够慢：B 得赶在 A 还在流的时候开起来。
# 要长到 A 在 B 提交那一刻**还在流**。第一版 25 段只流 31 秒，B 发请求时
# A 早就流完了，等于没造出重叠（09-19 白跑三轮）。
SEGMENTS = int(os.environ.get("SEGMENTS", "60"))
# 正文是**按行**落地的：一整段没有换行的长文本会被攒到收尾才出现，量出来就像
# 「根本不流式」（09-19 我拿它当过证据，是测试数据不对）。真回复是分段的。
NEWLINE_EVERY = int(os.environ.get("NEWLINE_EVERY", "4"))
REPLY = "".join(
    f"这是第{i}段回复正文。" + ("\n\n" if i % NEWLINE_EVERY == 0 else "")
    for i in range(1, SEGMENTS + 1)
)
LAST = f"这是第{SEGMENTS}段回复正文"


def tui_module():
    sys.path.insert(0, str(ROOT / "testkit" / "tui"))
    import run as tui  # noqa: E402

    tui.HOME = HOME
    tui.PORT = PORT
    tui.STUB_PORT = STUB_PORT
    tui.BIN = BIN
    tui.COLS, tui.ROWS = COLS, ROWS
    tui.ENV = dict(
        os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=RUNTIME, MIYU_TUI="1"
    )
    return tui


def wait_http(url, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except urllib.error.HTTPError:
            return True
        except Exception:
            time.sleep(0.3)
    return False


class View:
    """一个 TUI 的屏幕，**自带读取线程**。

    两个终端要同时盯，就不能在主线程里轮流读：等 B 的时候不读 A，A 的 PTY
    缓冲一满，那个进程当场**阻塞**——画面不再前进，量到的一切都是假的
    （09-19 栽在这儿，`末段=None` 就是这么来的）。所以每个终端各自一根线程
    一直读，主线程只看快照。

    屏幕是**增量**喂的：流到几百 KB 之后重放整个 sink 要几百毫秒，等待循环
    会被拖到几十秒一拍。
    """

    def __init__(self, master, name):
        import codecs
        import threading
        import pyte

        self.master = master
        self.name = name
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.Stream(self.screen)
        self.decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        self.lock = threading.Lock()
        self.sink = bytearray()
        self.alive = True
        self.thread = threading.Thread(target=self._pump, daemon=True)
        self.thread.start()

    def _pump(self):
        import select

        while self.alive:
            try:
                ready, _, _ = select.select([self.master], [], [], 0.1)
            except (OSError, ValueError):
                return
            if not ready:
                continue
            try:
                chunk = os.read(self.master, 65536)
            except OSError:
                return
            if not chunk:
                return
            with self.lock:
                self.sink.extend(chunk)
                text = self.decoder.decode(chunk)
                if text:
                    self.stream.feed(text)

    def stop(self):
        self.alive = False

    def lines(self):
        with self.lock:
            return [line.rstrip() for line in self.screen.display]

    def text(self):
        return "\n".join(self.lines())

    def flat(self):
        """去掉所有空白再找标记：终端按宽度折行，标记常被切成两行。"""
        return re.sub(r"\s+", "", self.text())

    def last_segment(self):
        """屏上**看得见**的最大段号。

        别拿「第3段」这种固定标记判「看没看见正文」：流着流着视口就滚过去了，
        早期的段落根本不在窗口里。两边内容一不一致，比的是同一时刻各自的末段。
        """
        found = [int(m) for m in re.findall(r"这是第(\d+)段回复正文", self.flat())]
        return max(found) if found else None

    def nonblank(self):
        return len([line for line in self.lines() if line.strip()])


def settle(seconds):
    time.sleep(seconds)


def wait_until(check, timeout, poll=0.2):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if check():
            return True
        time.sleep(poll)
    return False


def db_rows():
    """会话库里最后落了什么。db/wal/shm 三件都在，只读打开。"""
    db = HOME / "home" / "shorin" / "conversation.db"
    if not db.exists():
        return []
    connection = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        return connection.execute(
            "select session_id, seq, status, user_content,"
            " substr(assistant_content, 1, 40) from turns order by rowid"
        ).fetchall()
    finally:
        connection.close()


def queued_rows():
    db = HOME / "home" / "shorin" / "conversation.db"
    if not db.exists():
        return []
    connection = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        return connection.execute(
            "select session_id, seq, status, substr(content, 1, 40) from queued_prompts"
            " order by rowid"
        ).fetchall()
    except sqlite3.OperationalError as error:
        return [("<无此表>", str(error), "", "")]
    finally:
        connection.close()


T0 = time.time()


def mark(label):
    print(f"  [{time.time() - T0:5.1f}s] {label}", flush=True)


def ensure_my_stub(port, sentinel):
    """确认在跟**自己起的**桩说话。

    端口被别人占着时 `bind` 会失败，而探针只会看到一个「能应答的」服务——
    于是拿着别人的回复跑完全程，量到的一切都是假的（09-19 撞到另一个会话
    残留的桩，白跑两轮）。所以发一句话，看回的正文里有没有自己的暗号。
    """
    import json
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/chat/completions",
        data=json.dumps({
            "model": "stub", "stream": True,
            "messages": [{"role": "user", "content": "ping"}],
        }).encode(),
        headers={"content-type": "application/json"},
    )
    try:
        said = []
        with urllib.request.urlopen(request, timeout=30) as response:
            for line in response:
                if not line.startswith(b"data:"):
                    continue
                payload = line[5:].strip()
                if payload == b"[DONE]":
                    break
                try:
                    delta = json.loads(payload)["choices"][0]["delta"]
                except Exception:
                    continue
                said.append(str(delta.get("content") or ""))
    except Exception as error:
        print(f"! 桩模型探活失败：{error}", file=sys.stderr)
        return False
    # 暗号要在**拼起来**的正文里找：SSE 是几个字一块发的，原始响应里那串字
    # 是被切开的（09-19 第一版就栽在这儿）。
    if sentinel in "".join(said):
        return True
    print(
        f"! {port} 上的不是我起的桩（回的正文里没有暗号 {sentinel!r}）——"
        "多半是别的进程占着这个端口，换一个再跑",
        file=sys.stderr,
    )
    return False


def main():
    if not BIN.exists():
        print(f"! 先 cargo build：{BIN} 不存在", file=sys.stderr)
        return 2
    if HOME.exists():
        shutil.rmtree(HOME)
    Path(RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "requests.jsonl").unlink(missing_ok=True)
    tui = tui_module()
    tui.write_config()

    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(STUB_PORT),
            STUB_REASONING="1",
            STUB_REPLY=REPLY,
            # 一段一段慢慢吐，给 B 留出开起来的时间。
            STUB_CHUNK_SLEEP="0.35",
            # 「AI 收到的是什么」只有请求日志看得见。
            STUB_REQUEST_LOG=str(OUT / "requests.jsonl"),
        ),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    daemon = None
    facts = {}
    try:
        if not wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        if not ensure_my_stub(STUB_PORT, "这是第1段回复正文"):
            return 2
        daemon = subprocess.Popen(
            [str(BIN), "__daemon", "--port", str(PORT)],
            env=dict(tui.ENV, MIYU_LOG="debug"),
            cwd=str(HOME),
            stdout=(OUT / "daemon.log").open("w"),
            stderr=subprocess.STDOUT,
        )
        if not wait_http(f"{BASE}/api/config"):
            print("! daemon 没起来", file=sys.stderr)
            return 2

        # ── A 开起来，发第一句 ──
        a_process, a_master = tui.spawn_tui()
        a = View(a_master, "A")
        settle(3.0)
        os.write(a_master, f"{PROMPT_A}\r".encode())
        mark("A 已发出")
        streaming = wait_until(lambda: a.last_segment() is not None, 90)
        mark(f"A 开始流正文 = {streaming}（末段 {a.last_segment()}）")
        (OUT / "a-when-b-opens.txt").write_text(a.text(), encoding="utf-8")
        facts["A 正在流（还没到末段）"] = streaming and a.last_segment() != SEGMENTS

        # ── 趁 A 还在流，B 开起来 ──
        b_process, b_master = tui.spawn_tui()
        b = View(b_master, "B")
        mark("B 起进程")
        blanks = []
        for seconds in (2.0, 3.0, 5.0):
            settle(seconds)
            blanks.append((round(sum((2.0, 3.0, 5.0)[:len(blanks) + 1]), 1), b.nonblank()))
            mark(f"B 开着 {blanks[-1][0]}s：非空行 {blanks[-1][1]}")
        facts["_B 开着 N 秒时的非空行数"] = blanks
        (OUT / "b-on-open.txt").write_text(b.text(), encoding="utf-8")
        facts["B 一开起来看得见 A 那句话"] = PROMPT_A in b.flat()
        # 挂上去时先起转轮、再从回放里画用户消息的话，转轮会被孤零零留在用户
        # 消息**上面**（用户 09-19 截图，稳定复现）。判据：用户那句话之前不该
        # 有只剩一个转轮字形的行。
        body = [line for line in b.lines() if line.strip()]
        said_at = next(
            (i for i, line in enumerate(body) if PROMPT_A in re.sub(r"\s+", "", line)),
            None,
        )
        orphan = [
            line
            for line in body[:said_at or 0]
            if line.strip() and all(ch in SPINNER + " " for ch in line.strip())
        ]
        facts["_B 用户消息之前的孤立转轮行"] = orphan
        facts["B 挂上来没有孤立的转轮"] = not orphan
        facts["B 开在大厅（以为是空会话）"] = "Tab" in b.text()

        # 挂上去多久能看到正在流的正文，以及两边内容对不对得上。
        attached = time.time()
        saw = wait_until(lambda: b.last_segment() is not None, 30)
        facts["B 看得见 A 正在流的正文"] = saw
        facts["_B 看到正文用了几秒"] = round(time.time() - attached, 1) if saw else None
        settle(1.0)
        a_tail, b_tail = a.last_segment(), b.last_segment()
        facts["_同一时刻 A/B 的末段"] = [a_tail, b_tail]
        facts["两边看到的内容对得上"] = (
            a_tail is not None and b_tail is not None and abs(a_tail - b_tail) <= 8
        )
        mark(f"B 看到正文={saw}；同一时刻 A 末段={a_tail} B 末段={b_tail}")

        # ── B 在 A 还跑着的时候发一句 ──
        facts["_B 提交那一刻 A 还在流"] = a.last_segment() != SEGMENTS
        os.write(b_master, f"{PROMPT_B}\r".encode())
        mark(f"B 已发出（A 还在流={a.last_segment() != SEGMENTS}）")
        settle(5.0)
        (OUT / "b-after-send.txt").write_text(b.text(), encoding="utf-8")
        facts["B 那边显示排队中"] = any(
            word in b.flat() for word in ("排队", "queued", "Queued")
        )
        facts["A 那边看得见 B 排的队"] = PROMPT_B in a.flat()
        mark(f"B 显示排队={facts['B 那边显示排队中']}；A 看得见={facts['A 那边看得见 B 排的队']}")

        # ── 跑完 ──
        done = wait_until(lambda: a.last_segment() == SEGMENTS, 240)
        mark(f"A 流完 = {done}")
        settle(20.0)
        (OUT / "a-final.txt").write_text(a.text(), encoding="utf-8")
        (OUT / "b-final.txt").write_text(b.text(), encoding="utf-8")
        facts["A 那边最终看得见 B 那句"] = PROMPT_B in a.flat()
        facts["B 那边最终看得见 B 那句"] = PROMPT_B in b.flat()

        # ── 边界：发起方中断，跟随方该跟着停 ──
        #
        # A 按 Ctrl+C 停的是**这一轮**。B 挂在同一轮上，那一轮没了，B 不该继续
        # 转圈、也不该卡在跟随态里出不来。
        os.write(a_master, f"{PROMPT_C}\r".encode())
        if wait_until(lambda: a.last_segment() is not None, 90):
            settle(2.0)
            os.write(a_master, b"\x03")
            settle(6.0)
            (OUT / "b-after-interrupt.txt").write_text(b.text(), encoding="utf-8")
            # B 回到能打字的状态 = 输入框还在、没有卡死
            facts["A 中断后 B 还能用"] = BAR in b.text()
            facts["_A 中断后 B 的末几行"] = [
                line for line in b.lines() if line.strip()
            ][-3:]
            mark(f"A 中断后 B 还能用={facts['A 中断后 B 还能用']}")
        else:
            facts["A 中断后 B 还能用"] = False
            mark("A 第二句没开始流，中断这一场跳过")

        # ── 两个终端各说一句自然语言 ──
        #
        # 注意**不是**「TUI + 一个终端」：TUI 走的是按人格的 REPL 会话
        # （`repl_session_persona:*`），shellhook 走 `TurnSession::Current`，
        # 两者本来就不在一个会话里，不会撞（09-19 实测，我原先说会撞是错的）。
        # 真会撞的是两个 shellhook：它们都落在 current_session 上。
        def shellhook(text):
            return subprocess.Popen(
                [str(BIN), "--shell-intercept", "--shell", "fish", "--stdin"],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                env=dict(tui.ENV, MIYU_TUI="0"), cwd=str(HOME), text=True,
            ), text

        first, first_text = shellhook(PROMPT_SHELL)
        first.stdin.write(first_text + "\n")
        first.stdin.close()
        mark("终端一说了一句，等它开始答")
        settle(12.0)
        second, second_text = shellhook(PROMPT_SHELL2)
        second.stdin.write(second_text + "\n")
        second.stdin.close()
        mark("终端二说了一句")
        try:
            second_out = second.communicate(timeout=180)[0]
        except subprocess.TimeoutExpired:
            second.kill()
            second_out = ""
        try:
            first.communicate(timeout=180)
        except subprocess.TimeoutExpired:
            first.kill()
        (OUT / "shellhook.txt").write_text(second_out, encoding="utf-8")
        facts["第二个终端被告知排进了正在进行的对话"] = "排进" in second_out
        facts["第二个终端没有报「忙」"] = "busy" not in second_out.lower()
        mark(f"终端二排队={facts['第二个终端被告知排进了正在进行的对话']}")

        for master in (a_master, b_master):
            os.write(master, b"\x03")
        settle(1.0)
        a.stop()
        b.stop()
        for process in (a_process, b_process):
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
        settle(1.5)
        facts["_库里的轮"] = db_rows()
        facts["_排队表"] = queued_rows()
    finally:
        for process in (daemon, stub):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()

    (OUT / "facts.json").write_text(
        json.dumps(facts, ensure_ascii=False, indent=2, default=str), encoding="utf-8"
    )
    # 期望值。`False` 的那几条是「不该发生」的。
    expected = {
        "A 正在流（还没到末段）": True,
        "B 一开起来看得见 A 那句话": True,
        "B 挂上来没有孤立的转轮": True,
        "B 开在大厅（以为是空会话）": False,
        "B 看得见 A 正在流的正文": True,
        "两边看到的内容对得上": True,
        "B 那边显示排队中": True,
        "A 那边看得见 B 排的队": True,
        "A 那边最终看得见 B 那句": True,
        "B 那边最终看得见 B 那句": True,
        "A 中断后 B 还能用": True,
        "第二个终端被告知排进了正在进行的对话": True,
        "第二个终端没有报「忙」": True,
    }
    passed = 0
    checks = [(k, v) for k, v in facts.items() if not k.startswith("_")]
    for name, value in checks:
        want = expected.get(name)
        ok = value == want if want is not None else None
        passed += bool(ok)
        mark_char = "✅" if ok else ("❌" if ok is False else "·")
        print(f"  {mark_char} {name} = {'是' if value else '否'}")
    print(f"\n{passed}/{len(checks)} passed")
    print("\n库里的轮：")
    for row in facts["_库里的轮"]:
        print(f"  会话 {row[0][-8:]}  第{row[1]}轮  {row[2]}  用户说「{row[3]}」")
    print("\n排队表：")
    for row in facts["_排队表"] or [("(空)",)]:
        print(f"  {row}")
    log = OUT / "requests.jsonl"
    if log.exists():
        print("\n模型实际收到的（每次请求的消息列表）：")
        entries = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
        first_at = entries[0]["at"] if entries else 0
        for index, entry in enumerate(entries, 1):
            users = [m["content"] for m in entry["messages"] if m["role"] == "user"]
            assistants = [m["content"] for m in entry["messages"] if m["role"] == "assistant"]
            tail = [m for m in entry["messages"] if m["role"] == "assistant"]
            print(f"  第{index}次请求 +{entry['at'] - first_at:6.1f}s：user 末两条={users[-2:]}")
            if tail:
                print(f"            assistant 末条：全长 {tail[-1]['len']} 字 = {tail[-1]['content'][:50]}…")
    print(f"\n产物：{OUT}")
    return 0 if passed == len(checks) else 1


if __name__ == "__main__":
    raise SystemExit(main())
