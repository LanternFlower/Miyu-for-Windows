#!/usr/bin/env python3
"""模块依赖方向门禁。

拆分要解决的不只是"文件太长"，还有"牵一发动全身"。当前有几条反向依赖让
底层模块反过来引用上层：平台层引用 Web/daemon 的内部类型、daemon 入口引用
CLI 的参数与工具注册、工具层引用平台层。这类边只要还在，任何一处改动都会
沿着它扩散。

规则表的方向是**自底向上禁止**：底层不许知道上层的存在。

    config / paths / i18n      最底层，谁都能用，它谁都不用
    llm / state / memory       不许 use web / cli / config_tui / platforms
    tools                      不许 use web / cli / config_tui / platforms
    render                     不许 use web / cli / platforms
    agent                      不许 use web / cli / config_tui
    platforms                  不许 use web / cli / config_tui
    web                        不许 use cli / config_tui
    cli / config_tui           最上层，可以用下面所有

白名单（`scripts/arch-dep-waivers.json`）记录**现存**违规的条数，每条都写明为什么还在。拆分推进时逐条删除；
新增违规不会命中白名单，直接失败。

用法：

    python3 scripts/arch_dep_check.py            # 报告 + 白名单外的违规即失败
    python3 scripts/arch_dep_check.py --warn      # 只报告，永远退出 0
    python3 scripts/arch_dep_check.py --write-waivers  # 把当前违规写成白名单
"""
import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src"
WAIVER_FILE = ROOT / "scripts" / "arch-dep-waivers.json"

FORBIDDEN = {
    "llm": {"web", "cli", "config_tui", "platforms"},
    "state": {"web", "cli", "config_tui", "platforms"},
    "memory": {"web", "cli", "config_tui", "platforms"},
    # tools → platforms 是方案里明确要断的边：平台作用域工具（看图、QQ 群管）
    # 现在直接拿 PlatformTurnContext，要断得先抽 trait。先记进白名单让它只减
    # 不增。
    "tools": {"web", "cli", "config_tui", "platforms"},
    "render": {"web", "cli", "platforms"},
    "agent": {"web", "cli", "config_tui"},
    "platforms": {"web", "cli", "config_tui"},
    "web": {"cli", "config_tui"},
}

USE_CRATE = re.compile(r"\buse\s+crate::([a-z_][a-z0-9_]*)")
CRATE_PATH = re.compile(r"\bcrate::([a-z_][a-z0-9_]*)::")


def module_of(path):
    """文件属于哪个顶层模块。`src/tools/web.rs` → `tools`，`src/cli.rs` → `cli`。"""
    rel = path.relative_to(SRC)
    return rel.parts[0][:-3] if len(rel.parts) == 1 else rel.parts[0]


def scan():
    """返回 {(来源模块, 目标模块): [(文件, 行号, 原文)]}"""
    edges = defaultdict(list)
    for path in sorted(SRC.rglob("*.rs")):
        source = module_of(path)
        banned = FORBIDDEN.get(source)
        if not banned:
            continue
        for number, line in enumerate(
            path.read_text(encoding="utf-8", errors="replace").split("\n"), 1
        ):
            if line.lstrip().startswith("//"):
                continue
            # 同一行可能同时命中两个正则（`use crate::web::X` 就是），
            # 按目标模块去重，否则计数虚高。
            targets = {
                match.group(1)
                for match in list(USE_CRATE.finditer(line))
                + list(CRATE_PATH.finditer(line))
            }
            for target in sorted(targets):
                if target in banned and target != source:
                    edges[(source, target)].append(
                        (str(path.relative_to(ROOT)), number, line.strip()[:100])
                    )
    return edges


def key_of(source, target):
    return f"{source}->{target}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--warn", action="store_true", help="只报告，永远退出 0")
    parser.add_argument("--write-waivers", action="store_true", help="把当前违规写成白名单")
    args = parser.parse_args()

    edges = scan()
    waivers = {}
    if WAIVER_FILE.exists():
        waivers = json.loads(WAIVER_FILE.read_text(encoding="utf-8")).get("waivers", {})

    print("跨层引用现状（按边聚合）：")
    if not edges:
        print("  无")
    for (source, target), hits in sorted(edges.items(), key=lambda kv: -len(kv[1])):
        allowed = waivers.get(key_of(source, target), {}).get("count")
        mark = ""
        if allowed is None:
            mark = "  ← 未列入白名单"
        elif len(hits) > allowed:
            mark = f"  ← 超出白名单（{allowed} → {len(hits)}）"
        print(f"  {source:<10} → {target:<12} {len(hits):>3} 处{mark}")
        for name, number, text in hits[:3]:
            print(f"      {name}:{number}  {text}")
        if len(hits) > 3:
            print(f"      …另有 {len(hits) - 3} 处")

    if args.write_waivers:
        payload = {
            "_说明": "现存跨层引用的白名单。拆分推进时逐条减少 count；"
                     "只允许变小，变大即失败。新增的边不在表里，直接失败。",
            "waivers": {
                key_of(source, target): {"count": len(hits), "reason": "拆分前的既有依赖"}
                for (source, target), hits in sorted(edges.items())
            },
        }
        WAIVER_FILE.write_text(
            json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        print(f"\n白名单已写入 {WAIVER_FILE.relative_to(ROOT)}")
        return 0

    if args.warn:
        return 0

    problems = []
    for (source, target), hits in edges.items():
        allowed = waivers.get(key_of(source, target), {}).get("count")
        if allowed is None:
            problems.append(f"新增跨层引用 {source} → {target}（{len(hits)} 处）")
        elif len(hits) > allowed:
            problems.append(
                f"{source} → {target} 从 {allowed} 处涨到 {len(hits)} 处"
            )
    if problems:
        print("\n门禁未通过：")
        for item in problems:
            print(f"  ✗ {item}")
        return 1
    print("\n门禁通过：没有新增跨层引用，已有的也没变多")
    return 0


if __name__ == "__main__":
    sys.exit(main())
