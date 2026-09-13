#!/usr/bin/env python3
"""拿一个桩 chafa 顶替真的，验证 Miyu 到底给它传了什么。

为什么要桩而不是真二进制：真二进制只能告诉我们「成/不成」，桩能把 Miyu 组装
出来的**参数原文**和 **stdin 形态**记下来，断言才有落点。桩按版本扮演：认识
该版本有的选项，对没有的一律 stderr + 退出码 2——这正是旧 chafa 的真实脾气。

用法：
    python3 e2e.py [/path/to/miyu]
默认取 ../../target/release/miyu。
"""
import json, os, subprocess, sys, tempfile, shutil

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "../../target/release/miyu")
BIN = os.path.abspath(BIN)

# 每个选项是哪个版本进来的（chafa NEWS）
SINCE = {
    "--polite": (1, 10, 0),
    "--relative": (1, 14, 0),
    "--probe": (1, 16, 0),
    "--probe-mode": (1, 18, 1),
}

STUB = r'''#!/usr/bin/env python3
import os, sys
VERSION = {version!r}
SINCE = {since!r}
LOG = {log!r}

args = sys.argv[1:]
if "--version" in args:
    print("Chafa version " + ".".join(str(p) for p in VERSION))
    raise SystemExit(0)

with open(LOG, "a") as handle:
    handle.write(repr({{"args": args, "stdin_tty": os.isatty(0)}}) + "\n")

for arg in args:
    want = SINCE.get(arg.split("=")[0])
    if want and tuple(VERSION) < tuple(want):
        sys.stderr.write("chafa: 未知选项 " + arg + "\n")
        raise SystemExit(2)

# 装成出了一张字符画
sys.stdout.write("\x1b[48;2;1;2;3m \x1b[0m\n")
'''


def run_with_stub(version, home, image):
    """PATH 前置一个假装是 `version` 的 chafa，跑一次 print_image。"""
    shim = tempfile.mkdtemp(prefix="chafa-stub-")
    log = os.path.join(shim, "calls.log")
    path = os.path.join(shim, "chafa")
    with open(path, "w") as handle:
        handle.write(STUB.format(version=version, since=SINCE, log=log))
    os.chmod(path, 0o755)

    env = dict(os.environ)
    env["PATH"] = shim + os.pathsep + env["PATH"]
    env["MIYU_HOME"] = home
    env.pop("MIYU_IMAGE_TRACE", None)
    proc = subprocess.run(
        [BIN, "tool", "print_image", json.dumps({"image": image})],
        capture_output=True, text=True, env=env, timeout=60,
    )
    calls = []
    if os.path.exists(log):
        with open(log) as handle:
            calls = [eval(line) for line in handle if line.strip()]  # noqa: S307
    shutil.rmtree(shim, ignore_errors=True)
    return proc, calls


def main():
    if not os.path.exists(BIN):
        print(f"没有二进制: {BIN}")
        return 1
    home = tempfile.mkdtemp(prefix="miyu-chafa-home-")
    os.makedirs(os.path.join(home, "config"), exist_ok=True)
    sys.path.insert(0, HERE)
    from pty_probe import ensure_image
    image = ensure_image()

    cases = [
        ((1, 18, 2), "Arch / Fedora 42+ / homebrew / nix"),
        ((1, 18, 0), "Alpine 3.23 — 差一个补丁版本就没有 --probe-mode"),
        ((1, 16, 2), "Alpine 3.22"),
        ((1, 14, 5), "Debian 13 / Ubuntu 24.04+ / Fedora 41"),
        ((1, 12, 4), "Debian 12 / openSUSE Leap"),
        ((1, 8, 0), "Ubuntu 22.04 LTS — 连 --polite 都没有"),
    ]

    print(f"{'chafa':<9}{'退出':>5}  {'传给 chafa 的参数':<34}{'stdin':<7}发行版")
    print("-" * 100)
    failures = []
    for version, distro in cases:
        proc, calls = run_with_stub(version, home, image)
        text = ".".join(str(part) for part in version)
        if not calls:
            print(f"{text:<9}{proc.returncode:>5}  {'(没调用 chafa)':<34}{'-':<7}{distro}")
            failures.append(f"{text}: chafa 没被调用 — {proc.stdout.strip()} {proc.stderr.strip()}")
            continue
        call = calls[-1]
        # 只看选项，不看 --size 的值和图片路径
        shown = [a for a in call["args"] if a.startswith("--") or a in ("on", "off", "ctty")]
        shown = [a for a in shown if not a.startswith("--size")]
        stdin = "tty" if call["stdin_tty"] else "null"
        print(f"{text:<9}{proc.returncode:>5}  {' '.join(shown):<34}{stdin:<7}{distro}")

        # 断言：一个该版本不认识的选项都不许传
        for arg in call["args"]:
            want = SINCE.get(arg.split("=")[0])
            if want and tuple(version) < tuple(want):
                failures.append(f"{text}: 传了它不认识的 {arg}")
        if proc.returncode != 0:
            failures.append(f"{text}: 退出码 {proc.returncode} — {proc.stderr.strip()[:120]}")

    # chafa 不在 PATH 上时的说法
    env = dict(os.environ)
    env["PATH"] = tempfile.mkdtemp(prefix="empty-")
    env["MIYU_HOME"] = home
    proc = subprocess.run([BIN, "tool", "print_image", json.dumps({"image": image})],
                          capture_output=True, text=True, env=env, timeout=60)
    message = (proc.stdout + proc.stderr).strip().splitlines()
    print(f"\nchafa 不存在时：{message[-1][:100] if message else '(无输出)'}")
    if "chafa" not in (proc.stdout + proc.stderr):
        failures.append("chafa 缺失时的错误信息没提到 chafa")

    shutil.rmtree(home, ignore_errors=True)
    print()
    if failures:
        for failure in failures:
            print(f"  ✗ {failure}")
        return 1
    print("  ✓ 每个版本都只收到它认识的选项")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
