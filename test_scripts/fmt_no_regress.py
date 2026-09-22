#!/usr/bin/env python3
"""格式门禁。

**仓库已经 fmt-clean，所以默认走全仓 `cargo fmt --check`**（仓库根有
`.rustfmt-global` 这个开关文件）。工具链钉在 `toolchain.lock.json` 的 `rust`
上，和 CI 用的是同一个 rustfmt——2026-09-21 撞过：本机默认工具链是 nightly、
CI 是 1.96.1，两档对 `pub use crate::…` 的排序判得不一样。

---

下面那套「只禁止变差」是全仓 fmt-clean 之前的形态，留着以备再次出现大额
存量欠账（当时约 4400 行 diff，全仓格式化会产生一个与拆分混在一起的巨大提交，
破坏 `git blame` 与 `git bisect`）。它对每个改动过的文件比较 HEAD 与现在的
违规行数，只在变多时失败；新文件要求零违规。

⚠️ 这条路有一个已知的坑，删开关之前必须先修：它把文件内容写到 `/tmp` 的
临时文件里再跑 `rustfmt`，而 rustfmt 会去解析文件里的 `mod x;`——临时目录里
没有兄弟文件，它会直接报错退出、stdout 为空，于是**每个声明了子模块的文件
（所有 mod.rs / lib.rs）都被算成 0 违规**，门禁对它们恒为绿。2026-09-21 正是
这样把一处 `use` 排序问题放到了 CI 才红。现在 `violations()` 会把这种情况
当作「量不出来」抛错，而不是当成「干净」。
"""
import json
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
        if out.returncode != 0 and not out.stdout.strip():
            # rustfmt 根本没跑成（最常见的是临时文件解析不了 `mod x;`）。
            # 这时候 stdout 是空的，按「0 违规」处理等于把门禁关掉。
            raise RuntimeError(
                f"rustfmt 没能量出这个文件的格式：{out.stderr.strip()[:200]}"
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


def pinned_toolchain():
    """和 CI 用同一个 rustfmt。"""
    lock = ROOT / "packaging/common/toolchain.lock.json"
    try:
        return json.loads(lock.read_text(encoding="utf-8"))["rust"]
    except (OSError, ValueError, KeyError):
        return None


def main():
    if (ROOT / ".rustfmt-global").exists():
        version = pinned_toolchain()
        argv = ["cargo", "fmt", "--check"]
        if version:
            installed = subprocess.run(["rustup", "toolchain", "list"],
                                       capture_output=True, text=True)
            if version in installed.stdout:
                argv.insert(1, f"+{version}")
            else:
                print(f"⚠️  没装 {version}，用默认工具链量格式；"
                      f"CI 用的是 {version}，两档可能判得不一样")
        return subprocess.run(argv, cwd=ROOT).returncode

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
