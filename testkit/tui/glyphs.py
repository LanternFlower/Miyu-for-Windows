#!/usr/bin/env python3
"""把时间线的图标表打到终端上，看看哪个该换。

字形只有在**装了 Nerd Font 的终端里**才看得出长什么样——截图和 markdown 都会骗人
（我就把 atom 那个方框当成缺字，白改了一轮）。所以这张表只能在你自己的终端里看。

表是从 `src/render/stream/timeline.rs` 里现读的，不是手抄的：改了代码这儿就跟着变。

    python3 testkit/tui/glyphs.py           # 完整表
    python3 testkit/tui/glyphs.py --ascii   # 连 MIYU_TUI_ASCII=1 的退路一起看
    python3 testkit/tui/glyphs.py --plain   # 不上色（重定向到文件时用）
"""

import argparse
import re
import shutil
import sys
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "src" / "render" / "stream" / "timeline.rs"

DIM = "\x1b[2m"
BOLD = "\x1b[1m"
RESET = "\x1b[0m"


def unescape(raw):
    """Rust 字面量 → (字形, 码位说明)。"""
    match = re.fullmatch(r'"\\u\{([0-9a-f]+)\}"', raw)
    if match:
        point = int(match.group(1), 16)
        return chr(point), "U+%04X" % point
    text = raw.strip('"')
    if len(text) == 1 and ord(text) > 0x7F:
        return text, "U+%04X" % ord(text)
    return text, "字面量"


def block(source, head):
    """从 `head` 起，按花括号配对切出一整块。

    不按"最后一条分支长什么样"来切——那等于把码位写死两份，换个图标脚本就崩
    （第一版就是这么崩的）。
    """
    start = source.index(head)
    depth = 0
    for index in range(start + len(head) - 1, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    return source[start:]


def glyph_blocks(source):
    """`tool_glyph` 里的两张表：(ASCII 退路, Nerd Font)。"""
    body = block(source, "fn tool_glyph(name: &str) -> &'static str {")
    fallback = block(body, "return match name {")
    rest = body[body.index(fallback) + len(fallback) :]
    nerd = block(rest, "match name {")
    return fallback, nerd


def default_arm(body):
    """`_ => "字形",` 那一条。"""
    found = re.search(r'_\s*=>\s*("(?:[^"\\]|\\u\{[0-9a-f]+\})+")', body)
    return unescape(found.group(1)) if found else (None, None)


def arms(body):
    """`"a" | "b" => "字形",` → [(字形, 码位, [工具名])]"""
    out = []
    # `=> "字形"` 和 rustfmt 折出来的 `=> { "字形" }` 都要认——名字一多它就会折，
    # 只认前一种的话那一整行工具会从表里凭空消失。
    pattern = (
        r'((?:\s*\|?\s*"[a-zA-Z_]+"\s*)+)=>\s*\{?\s*'
        r'("(?:[^"\\]|\\u\{[0-9a-f]+\})+")'
    )
    for match in re.finditer(pattern, body):
        names = re.findall(r'"([a-zA-Z_]+)"', match.group(1))
        glyph, code = unescape(match.group(2))
        out.append((glyph, code, names))
    return out


def wrap_tools(text, room):
    """工具名一行放不下就折，按逗号断——从中间切开一个名字没法读。"""
    if len(text) <= room:
        return [text]
    pieces, current = [], ""
    for name in text.split(", "):
        candidate = name if not current else "%s, %s" % (current, name)
        if len(candidate) > room and current:
            pieces.append("%s," % current)
            current = name
        else:
            current = candidate
    if current:
        pieces.append(current)
    return pieces


def helper_glyph(source, name):
    """`fn glyph_xxx()` 里那两个分支：(字形, 码位, ASCII 退路)。

    不能按第一个 `}` 切——那是 `if nerd() {` 的收尾，切出来只剩一个字面量。
    """
    start = source.index("fn %s() -> &'static str {" % name)
    end = source.index("\n}\n", start)
    found = re.findall(r'"((?:[^"\\]|\\u\{[0-9a-f]+\})+)"', source[start:end])
    if len(found) < 2:
        return None
    glyph, code = unescape('"%s"' % found[0])
    return glyph, code, unescape('"%s"' % found[1])[0]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ascii", action="store_true", help="把 ASCII 退路一起列出来")
    parser.add_argument("--plain", action="store_true", help="不上色")
    parser.add_argument(
        "--narrow",
        action="store_true",
        help="把「不确定宽度」的符号（☰ ⊞ ✦ …）按一格算；竖线对不齐时试这个",
    )
    args = parser.parse_args()
    if not SOURCE.exists():
        print("! 找不到 %s" % SOURCE, file=sys.stderr)
        return 2
    source = SOURCE.read_text(encoding="utf-8")

    ascii_block, nerd_block = glyph_blocks(source)
    fallback = {}
    for glyph, _, names in arms(ascii_block):
        for name in names:
            fallback[name] = glyph
    ascii_default = default_arm(ascii_block)[0] or ""

    rows = []
    for glyph, code, names in arms(nerd_block):
        rows.append(
            (glyph, code, fallback.get(names[0], ascii_default), ", ".join(names))
        )

    specials = [
        ("glyph_think", "思考（已思考 / 思考中）"),
        ("glyph_err", "跑失败的那一步（盖过按类型挑的图标）"),
        ("glyph_notice", "通知（后台任务完成之类）"),
    ]
    extra = []
    for fn, what in specials:
        got = helper_glyph(source, fn)
        if got:
            extra.append((got[0], got[1], got[2], what))
    # 认不出来的工具走 `tool_glyph` 自己那条兜底分支，不是 `glyph_tool`。
    nerd_default, nerd_code = default_arm(nerd_block)
    if nerd_default:
        extra.append((nerd_default, nerd_code, ascii_default, "认不出来的工具（兜底）"))

    bold = "" if args.plain else BOLD
    dim = "" if args.plain else DIM
    reset = "" if args.plain else RESET

    def cells(ch):
        point = ord(ch)
        # 私有区（Nerd Font 的字形都在这儿）终端按**一格**画。
        if (
            0xE000 <= point <= 0xF8FF
            or 0xF0000 <= point <= 0xFFFFD
            or 0x100000 <= point <= 0x10FFFD
        ):
            return 1
        kind = unicodedata.east_asian_width(ch)
        # `Ambiguous` 在中文环境下按两格画（`☰ ⊞ ✦` 这些退路符号就是），
        # 但这取决于终端怎么配——对不齐就 `--narrow`。
        wide = "WF" if args.narrow else "WFA"
        return 2 if kind in wide else 1

    def width(text):
        return sum(cells(ch) for ch in text)

    def pad(text, columns):
        return text + " " * max(columns - width(text), 0)

    # 列之间画竖线：字形宽了窄了一眼就看得出来（竖线会被挤歪）。
    # 判断"这个图标合不合适"本来就是在判断它占几格。
    bar = "%s│%s" % (dim, reset)

    def row(glyph, code, back, what):
        cols = [pad(" %s " % glyph, 5), pad(" %s " % code, 10)]
        if args.ascii:
            cols.append(pad(" %s " % back, 5))
        return "%s %s" % (bar.join(cols), what)

    print(row("%s图标%s" % (bold, reset), "%s码位%s" % (bold, reset),
              "%s退路%s" % (bold, reset), "%s工具%s" % (bold, reset)))
    print("%s%s%s" % (dim, "─" * 78, reset))

    columns = shutil.get_terminal_size((100, 24)).columns
    left = 5 + 3 + 10 + 3 + (5 + 3 if args.ascii else 0)
    room = max(columns - left - 1, 24)

    def emit(glyph, code, back, what):
        pieces = wrap_tools(what, room)
        print(row(glyph, code, back, pieces[0]).rstrip())
        for piece in pieces[1:]:
            print("%s%s" % (" " * left, piece))

    for glyph, code, back, names in rows:
        emit(glyph, code, back, names)
    print("%s%s%s" % (dim, "─" * 78, reset))
    for glyph, code, back, what in extra:
        emit(glyph, code, back, "%s%s%s" % (dim, what, reset))

    print()
    print(
        "%s共 %d 类工具图标 + %d 个特殊图标。"
        "看不清就是字体里没有那个字形——`MIYU_TUI_ASCII=1` 走退路那一列。%s"
        % (dim, len(rows), len(extra), reset)
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
