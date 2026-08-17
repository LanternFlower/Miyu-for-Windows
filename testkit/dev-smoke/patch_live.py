#!/usr/bin/env python3
"""真实对话验证 apply_patch 唯一编辑器语义:建→改→删。"""
import sys, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
from run import DevRepl, HOME, LOGS, daemon

def main():
    LOGS.mkdir(exist_ok=True)
    target = Path(__file__).parent / "live_note.txt"
    if target.exists(): target.unlink()
    print("start rc=", daemon("start").returncode)
    ok = True
    try:
        repl = DevRepl(HOME, LOGS / "patch-live.log")
        time.sleep(4)
        r1 = repl.chat("在当前工作目录创建 live_note.txt,内容只有一行 hello。做完告诉我。", timeout=240)
        created = target.exists() and "hello" in target.read_text()
        print("PASS 创建" if created else "FAIL 创建", "|", r1[:60].replace("\n"," "))
        ok &= created
        time.sleep(4)
        r2 = repl.chat("把 live_note.txt 里的 hello 改成 world。", timeout=240)
        edited = target.exists() and "world" in target.read_text()
        print("PASS 修改" if edited else "FAIL 修改", "|", r2[:60].replace("\n"," "))
        ok &= edited
        time.sleep(4)
        r3 = repl.chat("删掉 live_note.txt。", timeout=240)
        deleted = not target.exists()
        print("PASS 删除" if deleted else "FAIL 删除", "|", r3[:60].replace("\n"," "))
        ok &= deleted
        raw = (LOGS/"patch-live.log").read_bytes().decode("utf-8","replace")
        used_patch = "apply_patch" in raw or "编辑文件" in raw
        print("PASS 走了 apply_patch/编辑文件" if used_patch else "WARN 未见 apply_patch 痕迹(可能走了 bash)")
        repl.close()
    finally:
        print("stop rc=", daemon("stop").returncode)
    print("全部通过" if ok else "存在失败")
    return 0 if ok else 1

if __name__ == "__main__":
    sys.exit(main())
