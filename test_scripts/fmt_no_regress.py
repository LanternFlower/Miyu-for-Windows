#!/usr/bin/env python3
"""格式门禁：只禁止变差，不要求存量达标。

仓库整体尚未 fmt-clean（约 4400 行 diff）。全仓格式化会产生一个巨大的、与
拆分混在一起的提交，破坏 `git blame` 与 `git bisect`，所以不做。

但「只查改动过的文件」也不对：`web.rs` 在 HEAD 时就有 39 处违规，只要碰它
一下，历史欠账就全算到这次改动头上。

所以按和规模／依赖门禁一致的语义来：对每个改动过的文件，比较它在 HEAD 与
现在的违规块数量，**只在变多时失败**。新文件要求零违规——新写的代码没有
历史包袱。

等哪天单独做完格式化提交，`touch .rustfmt-global` 即可切到全仓 `cargo fmt
--check`。
"""
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(
    subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
)


def changed_files():
    names = set()
    for args in (["git", "diff", "--name-only", "HEAD", "--", "*.rs"],
                 ["git", "diff", "--cached", "--name-only", "--", "*.rs"]):
        out = subprocess.run(args, capture_output=True, text=True, cwd=ROOT)
        names.update(line for line in out.stdout.split("\n") if line.strip())
    return sorted(name for name in names if (ROOT / name).exists())


def violations(source: str) -> int:
    """rustfmt 想改掉的行数。source 是文件内容。

    数**行**而不是数「Diff in」块：块会随内容位移而合并或拆分，同一批违规
    在插入几行之后就能从 39 块变成 57 块，把没引入任何新问题的改动判成回归
    （实测踩过）。行数是内容相关的稳定量。
    """
    with tempfile.NamedTemporaryFile("w", suffix=".rs", encoding="utf-8", delete=False) as handle:
        handle.write(source)
        temp = handle.name
    try:
        out = subprocess.run(
            # --color never:带色码时行首是转义序列，startswith("-") 匹配不到，
            # 会把所有文件都算成 0 违规——一个恒为绿的门禁。
            ["rustfmt", "--check", "--edition", "2021", "--color", "never", temp],
            capture_output=True, text=True,
        )
        return sum(
            1
            for line in out.stdout.split("\n")
            if line.startswith("-") and not line.startswith("---")
        )
    finally:
        Path(temp).unlink(missing_ok=True)


def renamed_from(name: str):
    """这个文件是不是从别处搬来的？返回原路径。

    `git mv` 之后新路径在 HEAD 里不存在，直接判成「新文件、要求零违规」会把
    原文件的历史欠账全算到搬运工头上。拆分期几乎每一步都在搬文件，这个误报
    必须消掉。
    """
    for args in (
        ["git", "diff", "--cached", "-M", "--name-status"],
        ["git", "diff", "-M", "--name-status", "HEAD"],
    ):
        out = subprocess.run(args, capture_output=True, text=True, cwd=ROOT)
        for line in out.stdout.split("\n"):
            parts = line.split("\t")
            if len(parts) == 3 and parts[0].startswith("R") and parts[2] == name:
                return parts[1]
    return None


def at_head(name: str):
    out = subprocess.run(
        ["git", "show", f"HEAD:{name}"], capture_output=True, text=True, cwd=ROOT
    )
    if out.returncode == 0:
        return out.stdout
    origin = renamed_from(name)
    if origin is None:
        return None
    out = subprocess.run(
        ["git", "show", f"HEAD:{origin}"], capture_output=True, text=True, cwd=ROOT
    )
    return out.stdout if out.returncode == 0 else None


def main():
    if (ROOT / ".rustfmt-global").exists():
        return subprocess.run(["cargo", "fmt", "--check"], cwd=ROOT).returncode

    files = changed_files()
    if not files:
        print("本次无 .rs 改动")
        return 0

    problems = []
    for name in files:
        now = violations((ROOT / name).read_text(encoding="utf-8"))
        base_source = at_head(name)
        if base_source is None:
            if now:
                problems.append(f"{name}：新文件应当零违规，现有 {now} 处")
            else:
                print(f"  {name:<32} 新文件，格式干净")
            continue
        was = violations(base_source)
        mark = "" if now <= was else "  ← 变差"
        if now > was:
            problems.append(f"{name}：违规从 {was} 涨到 {now} 处")
        print(f"  {name:<32} {was} → {now}{mark}")

    if problems:
        print("\n格式门禁未通过：")
        for item in problems:
            print(f"  ✗ {item}")
        return 1
    print("格式门禁通过：没有把任何文件改得更不合规")
    return 0


if __name__ == "__main__":
    sys.exit(main())
