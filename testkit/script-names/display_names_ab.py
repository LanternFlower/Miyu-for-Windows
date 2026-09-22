#!/usr/bin/env python3
"""工具与脚本显示名的中英 A/B(真二进制,隔离 MIYU_HOME,不起 daemon)。

BIN=target/debug/miyu python3 testkit/script-names/display_names_ab.py

同一套工具分别用 `MIYU_LANG=zh` 和 `MIYU_LANG=en` 跑 `miyu tool-call --list`,
对比每个工具的显示名。样本各验一条规则：

  1. 脚本只写了中文名        → 中文界面出中文名、英文界面按 id 兜底,**不回退中文**
  2. 脚本自带英文名          → 两边各出本语言的名字
  3. 脚本一个名都没写        → 两边都按 id 兜底(`no_name_tool` → `No name tool`)
  4. 内建工具               → 两边各自出本语言的名字(双语表,不再是中文单槽)

显示名只需要中文:工具 id 本来就是英文,英文界面按 id 折一个就够,不另维护英文文案。

再加两道全量闸：英文目录里不该剩中文显示名、中文目录里不该剩纯 ASCII 显示名
(品牌词白名单除外)。最后扫一遍源码,`with_display_name` 不许再写单语字面量——
只在 QQ 等场景注册的平台工具进不了 `--list`,只能这么锁。

对照组(改动前的二进制与脚本树)：
  BIN=/usr/bin/miyu SYS=<旧 src/scripts> MODE=before OUT=~/.cache/x python3 ...
"""
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
BIN = Path(os.environ.get("BIN", REPO / "target/debug/miyu")).expanduser().resolve()
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-script-names")).expanduser()
# 对照组用改动前的二进制与脚本树时传 SYS=<旧 src/scripts> MODE=before(只打表不断言)。
SYS = Path(os.environ.get("SYS", REPO / "src/scripts")).expanduser()
MODE = os.environ.get("MODE", "after")
HOME = OUT / "home"
RUNTIME = OUT / "runtime"

# 只写中文名 / 一个名都没写:用户自己写的脚本几乎都是这两种。
ONLY_CHINESE = """#!/usr/bin/env bash
# 显示名称：卧室吸顶灯
# Description: Switch the bedroom ceiling light on or off.
echo ok
"""
NO_NAME = """#!/usr/bin/env bash
# Description: A script whose header carries no display name at all.
echo ok
"""

# 中文界面下允许纯 ASCII 的显示名:整体是品牌/协议名硬翻反而难认,或本来就是
# 「一个名都没写」的兜底(`no_name_tool` 是本测具自己造的样本)。
ASCII_OK_IN_ZH = {"Reddit search", "No name tool"}

CJK = re.compile(r"[一-鿿]")

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(("PASS " if ok else "FAIL ") + name + (f"  [{detail}]" if detail else ""), flush=True)


def catalog(lang):
    """`tool-call --list` 的 `id\tdisplay` 表。"""
    env = dict(
        os.environ,
        MIYU_HOME=str(HOME),
        XDG_RUNTIME_DIR=str(RUNTIME),
        MIYU_SYSTEM_SCRIPTS_DIR=str(SYS),
        MIYU_ADMIN_USER="admin",
        MIYU_LANG=lang,
    )
    proc = subprocess.run(
        [str(BIN), "tool-call", "--list"],
        env=env,
        cwd=str(OUT),
        capture_output=True,
        text=True,
        timeout=120,
    )
    names = {}
    for line in proc.stdout.splitlines():
        if "\t" in line:
            tool, display = line.split("\t", 1)
            names[tool.strip()] = display.strip()
        elif line.strip():
            names[line.strip()] = ""
    return names, proc.stderr.strip()


def scan_source():
    """源码里还剩多少个单语的 `with_display_name("...")` 字面量。

    只扫第一个 `#[cfg(test)]` 之前的正文:测试里的桩工具(`battery_care_probe_test`)
    爱叫什么叫什么,不该算进产品面。
    """
    hits = []
    for path in sorted(REPO.joinpath("src").rglob("*.rs")):
        lines = path.read_text(encoding="utf-8").splitlines()
        for number, line in enumerate(lines, 1):
            if line.startswith("#[cfg(test)]"):
                break
            match = re.search(r'with_display_name\("([^"]*)"\)', line)
            if match:
                hits.append(f"{path.relative_to(REPO)}:{number} {match.group(1)}")
    return hits


def main():
    if not BIN.is_file():
        sys.exit(f"二进制不存在: {BIN}(先 cargo build --bin miyu)")
    if OUT.exists():
        shutil.rmtree(OUT)
    user_scripts = HOME / "data/scripts"
    user_scripts.mkdir(parents=True)
    RUNTIME.mkdir(parents=True)
    for name, body in (("ceiling_light", ONLY_CHINESE), ("no_name_tool", NO_NAME)):
        path = user_scripts / name
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)

    zh, zh_err = catalog("zh")
    en, en_err = catalog("en")
    check("两种语言都列出了工具", bool(zh) and bool(en), f"zh={len(zh)} en={len(en)} {zh_err[:60]}{en_err[:60]}")

    rows = [
        # (工具 id, 中文界面应显示, 英文界面应显示, 这一行验的是什么)
        ("bangumi", "番组日历", "Bangumi", "内置脚本·只写中文→英文按 id 兜底"),
        ("xhs_search", "小红书搜索", "Xhs search", "内置脚本·只写中文→英文按 id 兜底"),
        ("procusage", "查询进程占用", "Procusage", "内置脚本·只写中文→英文按 id 兜底"),
        ("codec", "编解码", "Encode/decode", "内置脚本·自带英文名(09-11 迁移那批)"),
        ("ceiling_light", "卧室吸顶灯", "Ceiling light", "用户脚本·只写中文→英文按 id 兜底"),
        ("no_name_tool", "No name tool", "No name tool", "用户脚本·无名→两边都兜底"),
        ("glob", "查找文件", "Find files", "内建工具·双语表"),
        ("aur", "AUR 查询", "AUR query", "内建工具·双语表"),
        ("alarm", "闹钟", "Alarms", "内建工具·双语表"),
        ("manage_script", "管理脚本", "Manage scripts", "内建工具·双语表"),
    ]
    print()
    print(f"{'工具 id':<16} {'中文界面':<14} {'英文界面':<22} 验的是")
    print("-" * 88)
    for tool, want_zh, want_en, what in rows:
        got_zh, got_en = zh.get(tool), en.get(tool)
        print(f"{tool:<16} {str(got_zh):<14} {str(got_en):<22} {what}")
        if MODE == "after":
            check(f"{tool} 中文界面 = {want_zh}", got_zh == want_zh, f"实际 {got_zh}")
            check(f"{tool} 英文界面 = {want_en}", got_en == want_en, f"实际 {got_en}")

    cjk_in_en = {tool: name for tool, name in en.items() if CJK.search(name)}
    ascii_in_zh = {
        tool: name
        for tool, name in zh.items()
        if name and not CJK.search(name) and name not in ASCII_OK_IN_ZH
    }
    literals = scan_source()
    print()
    print(f"英文目录里的中文显示名: {len(cjk_in_en)} 个")
    if cjk_in_en:
        print("   ", dict(sorted(cjk_in_en.items())[:10]))
    print(f"中文目录里的纯 ASCII 显示名: {len(ascii_in_zh)} 个")
    if ascii_in_zh:
        print("   ", dict(sorted(ascii_in_zh.items())[:10]))
    print(f"源码里单语的 with_display_name 字面量: {len(literals)} 处")
    for hit in literals[:10]:
        print("   ", hit)

    if MODE == "after":
        check("英文界面没有中文显示名", not cjk_in_en, str(sorted(cjk_in_en))[:200])
        check("中文界面没有纯英文显示名", not ascii_in_zh, str(sorted(ascii_in_zh))[:200])
        check("源码里没有单语 with_display_name", not literals, str(literals[:5]))

    failed = [name for name, ok, _ in results if not ok]
    print()
    print(f"{len(results) - len(failed)}/{len(results)} 通过")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
