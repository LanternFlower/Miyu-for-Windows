#!/usr/bin/env python3
"""通用的 `mod tests` 拆分器：按测试名关键词分主题落到 <mod>/tests/ 下。

用法：
    python3 split_tests_generic.py 源文件 配置.json

配置是 {"shared": [...], "groups": [[名字, 说明, [关键词...]], ...]}，
最后一组关键词留空作兜底。共用 fixture 落 shared.rs，各主题文件
`use super::shared::*`。

搬进子目录后 `super::` 的含义会变（原来指父模块，现在指当前 mod），所以
统一改写成绝对路径；`super::super::` 会被替换两次，要收掉重复的一段。
"""
import json
import re
import sys
from pathlib import Path

import rustscan


def main():
    src = Path(sys.argv[1])
    cfg = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
    shared_names = set(cfg["shared"])
    groups = cfg["groups"]
    # 模块路径：src/agent/mod.rs → crate::agent
    parts = src.with_suffix("").parts
    parts = [p for p in parts[1:] if p != "mod"]
    mod_path = "crate::" + "::".join(parts)
    parent_path = "crate::" + "::".join(parts[:-1]) if len(parts) > 1 else "crate"
    out = src.parent / "tests"

    lines = src.read_text(encoding="utf-8").split("\n")
    skeleton = rustscan.blank_out(lines)
    start = next(i for i, l in enumerate(lines) if re.match(r"^mod tests\b", l))
    end = rustscan.item_end(lines, start, skeleton)
    body = lines[start + 1:end]

    header, idx = [], 0
    while idx < len(body):
        s = body[idx].strip()
        if s.startswith("use "):
            chunk = [s]
            while not chunk[-1].rstrip().endswith(";"):
                idx += 1
                chunk.append(body[idx].strip())
            header.append(" ".join(chunk))
        elif s and not s.startswith("//") and not s.startswith("#!"):
            break
        idx += 1

    body_skeleton = skeleton[start + 1:end]
    items, i = [], idx
    item_re = re.compile(
        r"^(?:#\[|(?:pub\([\w:) ]+\)\s+)?(?:async\s+)?"
        r"(?:fn|struct|enum|const|impl|static|type|trait)\s)")
    name_re = re.compile(
        r"^(?:pub(?:\([\w:) ]+\))?\s+)?(?:async\s+)?"
        r"(?:fn|struct|enum|const|impl|static|type|trait)\s+(\w+)")
    while i < len(body):
        if not item_re.match(body[i].strip()):
            i += 1
            continue
        st = i
        while st > 0 and (body[st - 1].strip().startswith("#[")
                          or body[st - 1].strip().startswith("///")):
            st -= 1
        nl = i
        while nl < len(body) and body[nl].strip().startswith("#["):
            nl += 1
        m = name_re.match(body[nl].strip())
        name = m.group(1) if m else ""
        c = rustscan.item_end(body, nl, body_skeleton)
        items.append((name, "\n".join(
            l[4:] if l.startswith("    ") else l for l in body[st:c + 1])))
        i = c + 1

    buckets = {g[0]: [] for g in groups}
    shared = []
    for name, chunk in items:
        if name in shared_names:
            shared.append(chunk)
            continue
        for g, _, keys in groups:
            if not keys or any(k in name for k in keys):
                buckets[g].append(chunk)
                break

    def fix(text):
        text = text.replace("use super::*;", f"use {mod_path}::*;", 1)
        text = re.sub(r"\bsuper::(?!shared\b)", f"{mod_path}::", text)
        return text.replace(f"{mod_path}::{mod_path}::", f"{parent_path}::")

    out.mkdir(parents=True, exist_ok=True)
    uses = "\n".join(header) + "\n"
    (out / "shared.rs").write_text(fix(
        f"//! {cfg.get('shared_doc', '测试共用的 fixture。')}\n\n"
        + uses + "\n" + "\n\n".join(shared) + "\n"), encoding="utf-8")

    names = ["shared"]
    for g, doc, _ in groups:
        if not buckets[g]:
            continue
        (out / f"{g}.rs").write_text(fix(
            f"//! {doc}\n\n" + uses + "use super::shared::*;\n\n"
            + "\n\n".join(buckets[g]) + "\n"), encoding="utf-8")
        names.append(g)
        print(f"  {out}/{g}.rs  {len(buckets[g])} 项")

    (out / "mod.rs").write_text(
        f"//! {cfg.get('doc', '按被测主题分文件的测试。')}\n\n"
        + "".join(f"mod {n};\n" for n in names), encoding="utf-8")

    head = lines[:start]
    while head and head[-1].strip() in ("#[cfg(test)]", ""):
        head.pop()
    src.write_text("\n".join(head + ["", "#[cfg(test)]", "mod tests;"]
                             + lines[end + 1:]), encoding="utf-8")
    print(f"  shared.rs {len(shared)} 项；源文件剩 {len(head) + 2 + len(lines) - end} 行")


main()
