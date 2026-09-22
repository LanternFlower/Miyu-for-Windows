#!/usr/bin/env python3
"""回车那一刻的单行分流:哪些交给 zsh、哪些交给 Miyu。fish 那份的 zsh 版。

守的是同一个坑:zsh 也是**先展开再找命令**。一句自然语言里带个没匹配上的
通配符,zsh 在展开阶段就报 `no matches found`,`command_not_found_handler`
永远不触发。复现(修之前):

    zsh -i -c 'command_not_found_handler(){ echo HIT }; 说句话 /nope/*.zst'

判定函数直接从 `src/shell/zsh.rs` 里抠出来跑,不会和真 hook 跑偏;用真 zsh 执行,
`whence` 的结论就是本机的真实结论。

    python3 testkit/zsh-accept-line/run.py

不花额度,不起 daemon,不连模型。需要本机装了 zsh。
"""

import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SOURCE = REPO / "src" / "shell" / "zsh.rs"
FUNCTIONS = ["__miyu_first_token", "__miyu_head_is_plain_word"]

# (输入, 期望去向)。SHELL = 照旧交给 zsh;AI = 交给 Miyu。
CASES = [
    ("输出这段命令sudo rm -f /var/lib/nope/core.a1b2.*.zst", "AI"),
    ("帮我看看 *.zst 是什么", "AI"),
    ("这是什么?告诉我", "AI"),
    ("你好", "AI"),
    ("gti status", "AI"),
    ("FOO=1 你好呀", "AI"),
    ("ls -la", "SHELL"),
    ("git status", "SHELL"),
    ("rm -f /var/lib/nope/core.*.zst", "SHELL"),
    ("sudo rm -f /var/lib/nope/*.zst", "SHELL"),
    ("FOO=1 ls", "SHELL"),
    ("if true; then true; fi", "SHELL"),
    ('echo "带 * 的引号"', "SHELL"),
    ("~/bin/whatever", "SHELL"),
    ("$EDITOR foo", "SHELL"),
    ("$(date) 啥", "SHELL"),
]


def extract():
    text = SOURCE.read_text()
    out = []
    for name in FUNCTIONS:
        match = re.search(rf"^{name}\(\) \{{\n.*?^\}}$", text, re.S | re.M)
        if not match:
            sys.exit(f"在 {SOURCE} 里找不到 {name},hook 结构变了")
        out.append(match.group(0))
    return "\n\n".join(out)


def main():
    if not shutil.which("zsh"):
        sys.exit("本机没有 zsh,跳过")

    with tempfile.TemporaryDirectory() as tmp:
        script = Path(tmp) / "decide.zsh"
        script.write_text(
            extract()
            + """

decide() {
    local head
    head=$(__miyu_first_token "$1")
    if [[ -n $head ]] && __miyu_head_is_plain_word "$head" \\
        && ! whence -- "$head" >/dev/null 2>&1; then
        printf 'AI\\t%s\\n' "$head"
    else
        printf 'SHELL\\t%s\\n' "$head"
    fi
}

for line in "$@"; do decide "$line"; done
"""
        )
        # cwd 固定在 /,免得当前目录里正好有个同名脚本把 whence 的结论带偏。
        result = subprocess.run(
            ["zsh", str(script), *(text for text, _ in CASES)],
            capture_output=True, text=True, cwd="/",
        )

    lines = [line for line in result.stdout.splitlines() if line.strip()]
    if result.stderr.strip():
        print(result.stderr.strip(), file=sys.stderr)
    if len(lines) != len(CASES):
        sys.exit(f"期望 {len(CASES)} 条判定,实得 {len(lines)} 条:\n{result.stdout}")

    failures = []
    for (text, expected), line in zip(CASES, lines):
        got, head = line.split("\t", 1)
        print(f"{'✓' if got == expected else '✗'} {got:<5} 首词=[{head}]  << {text}")
        if got != expected:
            failures.append((text, expected, got))

    print()
    if failures:
        for text, expected, got in failures:
            print(f"期望 {expected},实得 {got}:{text}")
        sys.exit(f"{len(failures)}/{len(CASES)} 条判定不对")
    print(f"{len(CASES)}/{len(CASES)} 条判定符合预期")


if __name__ == "__main__":
    main()
