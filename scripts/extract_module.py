#!/usr/bin/env python3
"""按顶层条目名把代码从一个文件抽到另一个文件。

拆大文件时反复要做同一件事：把散落在几千行里的一组相关条目搬到新模块。手工
按行号剪贴容易错——删了前面的块，后面的行号就漂了。这个脚本按**名字**定位，
从后往前删，并把紧贴条目上方的属性与文档注释一起带走。

    python3 scripts/extract_module.py 源文件 目标文件 名字1 名字2 ...

目标文件已存在时追加。条目找不到会报错退出，不会静默漏掉——静默漏掉正是
拆分里最难查的一类错。

只搬代码，不管导入：新文件缺什么由编译器指出来，比这里猜一份靠谱。
"""
import re
import sys
from pathlib import Path

import rustscan

KINDS = r"fn|struct|enum|trait|impl|type|const|static|mod|macro_rules!"


def find_spans(lines, name, skeleton):
    """返回顶层条目 `name` 的所有 [起, 止) 行下标，含其上方的属性与文档注释。

    同一个名字可以有多个顶层条目——`struct X` 与 `impl X`、`impl Trait for X`
    都算。只搬第一个会把类型和它的方法拆散在两个文件里，编译还能过（方法只是
    留在原处），错得很安静。所以这里返回全部。
    """
    pattern = re.compile(
        rf"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?"
        rf"(?:({KINDS})\s+{re.escape(name)}\b"
        rf"|impl(?:<[^>]*>)?\s+[\w:<>, ]+\s+for\s+{re.escape(name)}\b)"
    )
    found = []
    for index, line in enumerate(lines):
        if not pattern.match(line):
            continue
        start = index
        while start > 0 and (
            lines[start - 1].startswith("#[")
            or lines[start - 1].startswith("///")
            or lines[start - 1].startswith("//!")
            or (lines[start - 1].startswith("//") and not lines[start - 1].startswith("// ──"))
        ):
            start -= 1
        found.append((start, rustscan.item_end(lines, index, skeleton) + 1))
    return found


def main():
    source, target, *names = sys.argv[1:]
    # 名字重复会让同一段被搬多次：find_spans 每次都返回全部 span，
    # 从后往前删时同一个区间被删两遍，剩下半截括号
    names = list(dict.fromkeys(names))
    src = Path(source)
    lines = src.read_text(encoding="utf-8").split("\n")
    skeleton = rustscan.blank_out(lines)

    spans, missing = [], []
    for name in names:
        found = find_spans(lines, name, skeleton)
        if not found:
            missing.append(name)
        for span in found:
            spans.append((span, name))
    if missing:
        print(f"找不到这些顶层条目：{', '.join(missing)}")
        return 1

    # 从后往前删，行号才不会漂
    taken = []
    for (start, end), name in sorted(spans, key=lambda item: -item[0][0]):
        taken.append((start, "\n".join(lines[start:end])))
        del lines[start:end]
    src.write_text("\n".join(lines), encoding="utf-8")

    taken.sort()
    body = "\n\n".join(chunk for _, chunk in taken)
    dst = Path(target)
    if dst.exists():
        dst.write_text(dst.read_text(encoding="utf-8").rstrip("\n") + "\n\n" + body + "\n",
                       encoding="utf-8")
    else:
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_text(body + "\n", encoding="utf-8")

    moved = sum(len(chunk.split("\n")) for _, chunk in taken)
    print(f"{source} → {target}：{len(spans)} 个条目（{len(names)} 个名字），"
          f"{moved} 行；源文件剩 {len(lines)} 行")
    return 0


if __name__ == "__main__":
    sys.exit(main())
