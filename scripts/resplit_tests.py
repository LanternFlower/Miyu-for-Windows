#!/usr/bin/env python3
"""把已经拆过一次的 `tests/xxx.rs` 再按主题分出一个兄弟文件。

`split_tests.py` 是从 `mod tests` 里一次分完；这个用于事后发现某个主题文件还是
太大，要再切一刀。新文件沿用原文件的导入块，并自动登记到 `tests/mod.rs`。

    python3 scripts/resplit_tests.py tests/大文件.rs 新模块名 "说明" 名字1 名字2 ...
"""
import re
import subprocess
import sys
from pathlib import Path


def main():
    source, name, doc, *items = sys.argv[1:]
    src = Path(source)
    target = src.with_name(f"{name}.rs")
    if target.exists():
        # 直接覆盖会把已有的一整个 mod 冲掉，而且编译照过——只有用例数门禁看得见
        print(f"{target} 已存在，换个名字（直接覆盖会丢掉它原有的测试）")
        return 1

    # 导入块沿用原文件的：测试之间共用的 fixture 与类型基本一致。
    # `use` 可能跨行（`use a::{\n b,\n};`），要吃到分号为止——只取首行会留下
    # 一个不配平的花括号，报成「unclosed delimiter」。
    lines = src.read_text(encoding="utf-8").split("\n")
    header, index = [], 0
    while index < len(lines):
        line = lines[index]
        if line.startswith("use "):
            header.append(line)
            while not lines[index].rstrip().endswith(";"):
                index += 1
                header.append(lines[index])
        elif header and line.strip() and not line.startswith("//"):
            break
        index += 1
    target.write_text(f"//! {doc}\n\n" + "\n".join(header) + "\n", encoding="utf-8")

    result = subprocess.run(
        ["python3", "scripts/extract_module.py", str(src), str(target)] + items
    )
    if result.returncode != 0:
        target.unlink(missing_ok=True)
        return result.returncode

    mod_rs = src.with_name("mod.rs")
    text = mod_rs.read_text(encoding="utf-8")
    if f"mod {name};" not in text:
        mod_rs.write_text(text.rstrip("\n") + f"\nmod {name};\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
