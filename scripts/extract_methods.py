#!/usr/bin/env python3
"""把 `impl X` 里的若干方法搬到另一个文件，在那边开一个新的 `impl X` 块。

`extract_module.py` 按顶层条目搬，遇到三千行的 `impl Agent` 就没辙了——整块是
一个条目，要么全搬要么不搬。Rust 允许同一个类型有多个 `impl` 块（同 crate 内
即可），所以按方法拆是合法的，语义完全不变。

    python3 scripts/extract_methods.py 源文件 目标文件 impl头 方法1 方法2 ...

「impl头」是用来定位的前缀，比如 `impl Agent`。目标文件已存在时追加一个新的
impl 块。方法找不到会报错退出，不静默漏掉。

只搬代码，不管导入：新文件缺什么由编译器指出来。
"""
import re
import sys
from pathlib import Path

import rustscan


def impl_span(lines, header, skeleton):
    """定位 `impl X {` 的 [起, 止]（止是那一行 `}`）。"""
    for index, line in enumerate(lines):
        if line.startswith(header) and line.rstrip().endswith("{"):
            return index, rustscan.item_end(lines, index, skeleton)
    raise SystemExit(f"找不到 impl 块：{header}")


def method_spans(lines, start, end, names, skeleton):
    """在 impl 块内按名字找方法，返回 {名字: (起, 止)}，含上方的属性与文档。"""
    # 方法在 impl 里缩进四格；签名可能跨行，靠花括号配平找结尾
    signature = re.compile(
        r"^    (?:pub(?:\([\w:) ]+\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)")
    found = {}
    cursor = start + 1
    while cursor < end:
        match = signature.match(lines[cursor])
        if not match:
            cursor += 1
            continue
        name = match.group(1)
        head = cursor
        while head > start + 1 and (
            lines[head - 1].lstrip().startswith("#[")
            or lines[head - 1].lstrip().startswith("///")
        ):
            head -= 1
        tail = rustscan.item_end(lines, cursor, skeleton)
        if name in names:
            found[name] = (head, tail)
        cursor = tail + 1
    return found


def main():
    source, target, header, *names = sys.argv[1:]
    src = Path(source)
    lines = src.read_text(encoding="utf-8").split("\n")
    skeleton = rustscan.blank_out(lines)
    start, end = impl_span(lines, header, skeleton)
    found = method_spans(lines, start, end, set(names), skeleton)

    missing = [name for name in names if name not in found]
    if missing:
        print(f"找不到这些方法：{', '.join(missing)}")
        return 1

    taken = []
    for name, (head, tail) in sorted(found.items(), key=lambda item: -item[1][0]):
        taken.append((head, "\n".join(lines[head:tail + 1])))
        del lines[head:tail + 1]
    src.write_text("\n".join(lines), encoding="utf-8")

    taken.sort()
    block = f"{header} {{\n" + "\n\n".join(chunk for _, chunk in taken) + "\n}\n"
    dst = Path(target)
    if dst.exists():
        dst.write_text(dst.read_text(encoding="utf-8").rstrip("\n") + "\n\n" + block,
                       encoding="utf-8")
    else:
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_text(block, encoding="utf-8")

    moved = sum(len(chunk.split("\n")) for _, chunk in taken)
    print(f"{source} → {target}：{len(taken)} 个方法，{moved} 行；"
          f"源文件剩 {len(lines)} 行")
    return 0


if __name__ == "__main__":
    sys.exit(main())
