#!/usr/bin/env python3
"""只把文件开头的导入块交给 rustfmt 排版。

搬模块时最常见的格式回归就是导入顺序：原地把 `crate::web::` 换成
`crate::runtime::` 之后，那一行就不在字母序上了。全文件跑 rustfmt 会顺手改掉
一堆历史遗留的排版，产生与本次改动无关的巨大 diff；这个脚本只动开头连续的
`use` 块，其余一个字节不碰。

    python3 scripts/fmt_imports.py src/a.rs src/b.rs
"""
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path


def format_block(block: str) -> str:
    with tempfile.NamedTemporaryFile(
        "w", suffix=".rs", encoding="utf-8", delete=False
    ) as handle:
        handle.write(block)
        temp = handle.name
    try:
        subprocess.run(
            ["rustfmt", "--edition", "2021", temp], capture_output=True, check=False
        )
        return Path(temp).read_text(encoding="utf-8")
    finally:
        os.unlink(temp)


def main():
    for name in sys.argv[1:]:
        path = Path(name)
        lines = path.read_text(encoding="utf-8").split("\n")
        # 跳过文件头的 //! 文档与属性
        start = 0
        while start < len(lines) and (
            lines[start].startswith("//!")
            or lines[start].startswith("#![")
            or not lines[start].strip()
        ):
            start += 1
        # 收下开头连续的 use / 夹在中间的注释
        end = start
        seen_use = False
        while end < len(lines):
            line = lines[end]
            if line.startswith("use "):
                seen_use = True
                # 多行 use 要吃到分号
                while end < len(lines) and not lines[end].rstrip().endswith(";"):
                    end += 1
            elif seen_use and (line.startswith("//") or not line.strip()):
                pass
            elif seen_use:
                break
            elif not line.strip() or line.startswith("//"):
                pass
            else:
                break
            end += 1
        if not seen_use:
            print(f"  {name}: 没找到导入块")
            continue
        block = "\n".join(lines[start:end])
        formatted = format_block(block).rstrip("\n")
        if formatted == block.rstrip("\n"):
            print(f"  {name}: 导入块已经是规范的")
            continue
        path.write_text(
            "\n".join(lines[:start] + formatted.split("\n") + lines[end:]),
            encoding="utf-8",
        )
        print(f"  {name}: 导入块已重排")


if __name__ == "__main__":
    main()
