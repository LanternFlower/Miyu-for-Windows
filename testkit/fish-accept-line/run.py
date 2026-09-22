#!/usr/bin/env python3
"""回车那一刻的单行分流:哪些交给 fish、哪些交给 AI。

守的是这个坑:fish 是**先展开再找命令**。句子里带一个没匹配上的通配符——

    输出这段命令sudo rm -f /var/lib/systemd/coredump/core.….*.zst

fish 在展开阶段就报「未找到通配符的匹配项」,命令根本没开始找,
`fish_command_not_found` 也就永远不触发,整句话掉在地上。修法是回车时先看首词。

判定函数直接从 `src/shell/fish.rs` 里抠出来跑,所以不会和真 hook 跑偏;
用真 fish 执行,`type -q` 的结论也就是本机的真实结论。

    python3 testkit/fish-accept-line/run.py

不花额度,不起 daemon,不连模型。需要本机装了 fish。
"""

import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SOURCE = REPO / "src" / "shell" / "fish.rs"
FUNCTIONS = ["__miyu_first_token_raw", "__miyu_head_is_plain_word"]

# (输入, 期望去向)。SHELL = 照旧交给 fish 执行;AI = 交给 Miyu。
CASES = [
    # 用户现场:自然语言 + 没匹配上的通配符。修之前这一行连 hook 都到不了。
    ("输出这段命令sudo rm -f /var/lib/systemd/coredump/core.gamescope-wl.1000.efc4a04b18a6469fb58058eaa835d7ff.*.zst", "AI"),
    ("输出这段命令sudo rm -f /var/lib/nope/core.*.zst 你好", "AI"),
    ("帮我看看 *.zst 是什么", "AI"),
    ("这是什么?告诉我", "AI"),
    ("你好", "AI"),
    ("gti status", "AI"),          # 打错的命令,原来靠 command_not_found,终点相同
    ("FOO=1 你好呀", "AI"),         # 环境变量前缀要跳过
    # 真命令一律照旧,通配符没匹配上也照旧由 fish 自己报错。
    ("ls -la", "SHELL"),
    ("git status", "SHELL"),
    ("rm -f /var/lib/nope/core.*.zst", "SHELL"),
    ("sudo rm -f /var/lib/nope/*.zst", "SHELL"),
    ("FOO=1 ls", "SHELL"),
    ("if true; end", "SHELL"),
    ('echo "带 * 的引号"', "SHELL"),
    # 首词要展开才知道是什么的,一律交回 fish,这里不猜。
    ("~/bin/whatever", "SHELL"),
    ("$EDITOR foo", "SHELL"),
    ("(date) 啥", "SHELL"),
]


def extract_functions():
    text = SOURCE.read_text()
    out = []
    for name in FUNCTIONS:
        match = re.search(rf"^function {name}\n.*?^end$", text, re.S | re.M)
        if not match:
            sys.exit(f"在 {SOURCE} 里找不到 {name},hook 结构变了")
        out.append(match.group(0))
    return "\n\n".join(out)


def main():
    if not shutil.which("fish"):
        sys.exit("本机没有 fish,跳过")

    with tempfile.TemporaryDirectory() as tmp:
        script = Path(tmp) / "decide.fish"
        script.write_text(
            extract_functions()
            + """

function decide
    set -l head (__miyu_first_token_raw "$argv[1]")
    if not __miyu_head_is_plain_word "$head"; or type -q -- "$head"
        printf 'SHELL\\t%s\\n' "$head"
    else
        printf 'AI\\t%s\\n' "$head"
    end
end

for line in $argv
    decide "$line"
end
"""
        )
        # cwd 固定在 /,免得当前目录里正好有个同名脚本把 type -q 的结论带偏。
        result = subprocess.run(
            ["fish", str(script), *(text for text, _ in CASES)],
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
        mark = "✓" if got == expected else "✗"
        print(f"{mark} {got:<5} 首词=[{head}]  << {text}")
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
