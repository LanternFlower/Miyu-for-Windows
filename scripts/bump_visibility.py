#!/usr/bin/env python3
"""把一个文件里的顶层条目、结构体字段、impl 方法提升到指定可见性。

搬模块的必备步骤：原本同模块内随便访问的私有项，搬出去之后调用方成了外部，
必须显式放开。纯可见性调整，不改行为。

    python3 scripts/bump_visibility.py 文件 'pub(in crate::cli)'

**trait impl 里的方法不加修饰**——`impl Default for X`、`impl Drop for X`
里的 `fn` 带可见性是编译错误（E0449），方法的可见性由 trait 决定。这一条踩
过两次（runtime 一次、cli 一次），所以固化进工具。

按花括号深度判定，不会误伤结构体**字面量**里的 `name: value`（第一版就是在
这里翻的车，把初始化表达式也加了 pub）。
"""
import re
import sys
from pathlib import Path

# 已经带可见性前缀的条目自身不用改，但**里面的字段和方法仍要处理**——
# `pub(crate) struct X` 的私有字段搬出去之后一样访问不到（踩过：QqListenerManager）
TOP_ITEM = re.compile(
    r"^(?:(pub(?:\([\w:) ]+\))?)\s+)?(struct|enum|fn|async fn|const|static|type|impl)\b"
)
TRAIT_IMPL = re.compile(r"^impl(?:<[^>]*>)?\s+[\w:<>, ]+\s+for\s+")
FIELD = re.compile(r"^(\w+):\s")
METHOD = re.compile(r"^(async\s+)?fn \w+")
ASSOC_CONST = re.compile(r"^const [A-Z_]+")


def strip_noise(line):
    line = re.sub(r'"(?:[^"\\]|\\.)*"', '""', line)
    line = re.sub(r"'(?:[^'\\]|\\.)'", "''", line)
    return line.split("//")[0]


def main():
    path, vis = Path(sys.argv[1]), sys.argv[2]
    lines = path.read_text(encoding="utf-8").split("\n")
    out, depth, block = [], 0, None
    counts = {"顶层": 0, "字段": 0, "方法": 0, "常量": 0}

    for line in lines:
        stripped = line.lstrip()
        indent = len(line) - len(stripped)

        top = TOP_ITEM.match(stripped) if depth == 0 and indent == 0 else None
        if top:
            already_public, kind = top.group(1), top.group(2)
            if kind == "impl":
                # trait impl 里的方法由 trait 决定可见性，加修饰是编译错误
                block = "trait_impl" if TRAIT_IMPL.match(stripped) else "impl"
            elif kind in ("struct", "enum"):
                block = kind
            else:
                block = None
            if kind != "impl" and not already_public:
                line = f"{vis} " + line
                counts["顶层"] += 1
        elif depth == 1 and block == "struct" and FIELD.match(stripped) \
                and not stripped.startswith("pub"):
            line = line[:indent] + f"{vis} " + stripped
            counts["字段"] += 1
        elif depth == 1 and block == "impl" and not stripped.startswith("pub"):
            if METHOD.match(stripped):
                line = line[:indent] + f"{vis} " + stripped
                counts["方法"] += 1
            elif ASSOC_CONST.match(stripped):
                line = line[:indent] + f"{vis} " + stripped
                counts["常量"] += 1

        out.append(line)
        clean = strip_noise(line)
        depth += clean.count("{") - clean.count("}")
        if depth <= 0:
            depth = 0
            block = None

    path.write_text("\n".join(out), encoding="utf-8")
    print("  ".join(f"{key} {value}" for key, value in counts.items() if value))


if __name__ == "__main__":
    main()
