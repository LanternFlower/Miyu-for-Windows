#!/usr/bin/env python3
"""把 Rust 源码里的字符串与注释抹掉，只留下用于数花括号的骨架。

按行做正则替换的版本会被**裸字符串**骗到：`r#"{"a": 1}"#` 里的引号和花括号
一个都不是代码，但正则会把 `"{"` 当成一个完整字符串，剩下的花括号就漏进了计
数。openai_compatible.rs 里有 73 处裸字符串，`mod tests` 的结尾因此定位到了错
误的行——**不报错，只是少搬了三分之二的内容**。

跨行也必须处理：裸字符串和块注释都能横跨很多行，逐行独立扫描一定错。

`blank_out(lines)` 返回等长的列表，字符串内容与注释被换成空格，其余原样。
"""


def blank_out(lines):
    """抹掉字符串与注释，保持行数与列宽不变。"""
    out = []
    # 跨行状态：块注释深度、以及正在进行中的字符串（普通/裸）
    comment_depth = 0
    raw_hashes = None      # 裸字符串正在进行中时，收尾需要的 # 个数
    in_string = False      # 普通字符串正在进行中（末尾反斜杠续行）

    for line in lines:
        result = []
        index = 0
        length = len(line)
        while index < length:
            char = line[index]

            if comment_depth > 0:
                if line.startswith("*/", index):
                    comment_depth -= 1
                    result.append("  ")
                    index += 2
                    continue
                if line.startswith("/*", index):
                    comment_depth += 1
                    result.append("  ")
                    index += 2
                    continue
                result.append(" ")
                index += 1
                continue

            if raw_hashes is not None:
                closing = '"' + "#" * raw_hashes
                if line.startswith(closing, index):
                    raw_hashes = None
                    result.append(" " * len(closing))
                    index += len(closing)
                    continue
                result.append(" ")
                index += 1
                continue

            if in_string:
                if char == "\\":
                    result.append("  ")
                    index += 2
                    continue
                if char == '"':
                    in_string = False
                    result.append(" ")
                    index += 1
                    continue
                result.append(" ")
                index += 1
                continue

            if line.startswith("//", index):
                result.append(" " * (length - index))
                break
            if line.startswith("/*", index):
                comment_depth = 1
                result.append("  ")
                index += 2
                continue

            # 裸字符串：r"..."、r#"..."#、br#"..."#
            if char in "rb":
                cursor = index
                if line.startswith("br", cursor):
                    cursor += 2
                elif char == "r":
                    cursor += 1
                else:
                    cursor = None
                if cursor is not None:
                    hashes = 0
                    while cursor + hashes < length and line[cursor + hashes] == "#":
                        hashes += 1
                    if cursor + hashes < length and line[cursor + hashes] == '"':
                        raw_hashes = hashes
                        span = cursor + hashes + 1 - index
                        result.append(" " * span)
                        index += span
                        continue

            if char == '"':
                in_string = True
                result.append(" ")
                index += 1
                continue

            # 字符字面量：'{' 会污染计数，生命周期 'a 不会（后面不是引号）
            if char == "'" and index + 2 < length:
                if line[index + 1] == "\\":
                    end = line.find("'", index + 2)
                    if end != -1 and end - index <= 5:
                        result.append(" " * (end - index + 1))
                        index = end + 1
                        continue
                elif line[index + 2] == "'":
                    result.append("   ")
                    index += 3
                    continue

            result.append(char)
            index += 1

        out.append("".join(result))
    return out


OPEN = "{[("
CLOSE = "}])"


def item_end(lines, start, blank=None):
    """给定条目起始行，返回它最后一行的下标（括号配平或分号结束）。

    三种括号都参与配平，但**只有 `{` 算「条目开始了」**——条目的正文由花括号
    界定，别的括号只是类型或参数的一部分：

    - 只数花括号不数方括号：`const T: [S; N] = [S { .. }, S { .. }];` 的数组
      层没被计入，表在第一个元素处就被截断（REPL_COMMAND_TABLE 踩过）。
    - 方括号算「开始」：`fn f<W>(payload: &[u8]) -> R` 的 `&[u8]` 让第一行就
      seen+归零，函数体连同 `where` 子句被丢在原地。
    - 圆括号算「开始」：带 `where` 子句的泛型函数，签名的 `)` 让深度归零。

    没有花括号的条目（数组常量、类型别名）靠「深度归零 + 行尾分号」收尾；分号
    必须配上深度归零，否则 `thread_local!( .. ; )` 里那个分号会提前结束。
    """
    skeleton = blank if blank is not None else blank_out(lines)
    depth, seen = 0, False
    cursor = start
    while cursor < len(lines):
        line = skeleton[cursor]
        depth += sum(line.count(c) for c in OPEN) - sum(line.count(c) for c in CLOSE)
        seen = seen or "{" in line
        if seen and depth <= 0:
            return cursor
        if not seen and depth <= 0 and line.rstrip().endswith(";"):
            return cursor
        cursor += 1
    return len(lines) - 1
