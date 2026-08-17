#!/usr/bin/env python3
"""驱动 /models 选择器:Tab 勾选→Enter,查 DB 覆盖与 footer。"""
import os
import re
import sqlite3
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from run import DevRepl, HOME, LOGS, daemon  # noqa: E402


def db(sql, args=()):
    conn = sqlite3.connect(f"file:{HOME/'state'/'conversation.db'}?mode=ro", uri=True)
    try:
        return conn.execute(sql, args).fetchall()
    finally:
        conn.close()


def clean(raw: bytes) -> str:
    return re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", raw.decode("utf-8", "replace"))


def main():
    LOGS.mkdir(exist_ok=True)
    print("start rc=", daemon("start").returncode)
    try:
        log = LOGS / "models-picker.log"
        repl = DevRepl(HOME, log)
        time.sleep(4)
        sid = db("SELECT session_id FROM sessions WHERE persona='dev' ORDER BY updated_at DESC LIMIT 1")
        sid = sid[0][0] if sid else None
        print("dev session:", sid)
        print("override before:", db("SELECT model_override FROM sessions WHERE session_id=?", (sid,)) if sid else "-")
        # 打开选择器
        os.write(repl.master, b"/models\r")
        time.sleep(2)
        # 勾第一行,j 下移,勾第二行,回车确认
        os.write(repl.master, b"\t")
        time.sleep(0.6)
        os.write(repl.master, b"j")
        time.sleep(0.6)
        os.write(repl.master, b"\t")
        time.sleep(0.6)
        os.write(repl.master, b"\r")
        time.sleep(3)
        after = db("SELECT model_override FROM sessions WHERE session_id=?", (sid,)) if sid else []
        print("override after:", after)
        text = clean(open(log, "rb").read())
        for marker in ("未做修改", "已更新当前会话模型", "当前会话模型", "错误", "error"):
            if marker in text:
                print("saw:", marker)
        tail = [l for l in text.splitlines() if "opencodego" in l or "·" in l]
        print("footer tail:", tail[-1][:120] if tail else "-")
        repl.close()
    finally:
        print("stop rc=", daemon("stop").returncode)


if __name__ == "__main__":
    main()
