#!/usr/bin/env python3
"""一个家目录只能有一个 daemon —— 端到端验收。

复现的是 09-21 本机那个现场：同一个 `~/.miyu`，一个 daemon 从设了
`MIYU_HOME` 的 shell 起（runtime_dir 是 `miyu-<hash>`），另一个从没设的
shell 起（runtime_dir 是字面量 `miyu`），两把运行时锁互相看不见，于是两个
daemon 同时跑在同一份数据上，还各自拉起一个 miyu-voice 抢同一个麦克风。

用法：
    python3 testkit/daemon-singleton/run.py [--binary <path>]
"""

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PASS, FAIL = [], []


def check(name, ok, detail=""):
    (PASS if ok else FAIL).append(name)
    print(f"  {'✓' if ok else '✗'} {name}" + (f"  {detail}" if detail else ""))


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Daemon:
    """一个 daemon 进程。`explicit_home` 决定走不走 MIYU_HOME 那条路。"""

    def __init__(self, binary, home_root, runtime_root, explicit_home, port):
        env = dict(os.environ)
        # HOME 决定「没设 MIYU_HOME 时」算出来的默认家目录，必须一起隔离，
        # 否则测试会打到开发机真正的 ~/.miyu 上。
        env["HOME"] = str(home_root)
        env["XDG_RUNTIME_DIR"] = str(runtime_root)
        env["XDG_CONFIG_HOME"] = str(home_root / ".config")
        env.pop("MIYU_HOME", None)
        if explicit_home:
            env["MIYU_HOME"] = str(home_root / ".miyu")
        self.explicit = explicit_home
        self.log = home_root / f"daemon-{'explicit' if explicit_home else 'default'}.log"
        handle = open(self.log, "wb")
        self.proc = subprocess.Popen(
            [str(binary), "__daemon", "--port", str(port), "--bind", "127.0.0.1"],
            env=env,
            stdout=handle,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )

    def wait_exit(self, timeout):
        try:
            return self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            return None

    def alive(self):
        return self.proc.poll() is None

    def output(self):
        try:
            return self.log.read_text(errors="replace")
        except OSError:
            return ""

    def kill(self):
        if self.alive():
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                self.proc.terminate()
            self.wait_exit(10)


def scenario_same_home(binary, workdir):
    """设了 MIYU_HOME 和没设,指的是同一个家目录 —— 第二个必须让位。"""
    print("\n[1] 同一个家目录,两种 MIYU_HOME 写法")
    home = workdir / "case1"
    (home / ".miyu").mkdir(parents=True)
    runtime = workdir / "run1"
    runtime.mkdir()

    first = Daemon(binary, home, runtime, explicit_home=True, port=free_port())
    # 先让占位的那个把锁抢稳；它要开库、建目录,给够时间。
    time.sleep(6)
    check("先起的 daemon 活着", first.alive(), f"pid={first.proc.pid}")

    second = Daemon(binary, home, runtime, explicit_home=False, port=free_port())
    code = second.wait_exit(30)
    check("后起的 daemon 主动退出", code is not None, f"exit={code}")
    text = second.output()
    check(
        "退出时说清了原因",
        ("让位" in text) or ("standing down" in text),
        text.strip().splitlines()[-1][:110] if text.strip() else "(无输出)",
    )
    check("先起的那个没被影响", first.alive())

    # 让位要赶在占资源之前:第二个 daemon 连自己那个 runtime_dir 都不该建
    # 出来,更别说开库、抢端口、拉起 miyu-voice。
    names = sorted(p.name for p in runtime.iterdir() if p.is_dir())
    check(
        "让位赶在建运行时目录之前",
        len(names) == 1,
        f"runtime 目录: {names}",
    )

    lock = home / ".miyu" / "daemon.lock"
    ok = False
    if lock.exists():
        try:
            record = json.loads(lock.read_text())
            ok = record.get("pid") == first.proc.pid
        except (ValueError, OSError):
            ok = False
    check("锁文件记着在位那个 daemon 的 pid", ok)

    first.kill()
    second.kill()


def scenario_separate_homes(binary, workdir):
    """不同家目录互不干扰 —— 否则一跑测试就把开发机的 daemon 顶掉。"""
    print("\n[2] 两个不同的家目录")
    runtime = workdir / "run2"
    runtime.mkdir()
    homes = []
    for index in (1, 2):
        home = workdir / f"case2-{index}"
        (home / ".miyu").mkdir(parents=True)
        homes.append(Daemon(binary, home, runtime, explicit_home=True, port=free_port()))
    time.sleep(8)
    check("第一个家目录的 daemon 活着", homes[0].alive())
    check("第二个家目录的 daemon 也活着", homes[1].alive())
    for daemon in homes:
        daemon.kill()


def scenario_lock_released(binary, workdir):
    """在位的 daemon 走了,锁要能交给下一个。"""
    print("\n[3] 让位后锁能再被拿到")
    home = workdir / "case3"
    (home / ".miyu").mkdir(parents=True)
    runtime = workdir / "run3"
    runtime.mkdir()

    first = Daemon(binary, home, runtime, explicit_home=True, port=free_port())
    time.sleep(6)
    check("占位的 daemon 起来了", first.alive())
    first.kill()
    time.sleep(2)

    second = Daemon(binary, home, runtime, explicit_home=False, port=free_port())
    time.sleep(6)
    check("前一个走了之后,新 daemon 能起来", second.alive())
    second.kill()


def scenario_cli_is_told_why(binary, workdir):
    """CLI 探不到 daemon、锁又被占着时,要当场说清楚,而不是起一个注定
    让位的进程、然后让用户对着「启动超时」发呆。"""
    print("\n[4] CLI 撞上跑在别处的 daemon")
    home = workdir / "case4"
    (home / ".miyu").mkdir(parents=True)
    runtime = workdir / "run4"
    runtime.mkdir()

    holder = Daemon(binary, home, runtime, explicit_home=True, port=free_port())
    time.sleep(6)
    check("占位的 daemon 起来了", holder.alive())

    env = dict(os.environ)
    env["HOME"] = str(home)
    env["XDG_RUNTIME_DIR"] = str(runtime)
    env["XDG_CONFIG_HOME"] = str(home / ".config")
    env.pop("MIYU_HOME", None)  # 另一边没设 —— 就是分叉的来源
    done = subprocess.run(
        [str(binary), "daemon", "start"],
        env=env,
        capture_output=True,
        text=True,
        timeout=90,
    )
    output = (done.stdout or "") + (done.stderr or "")
    check("CLI 没有假装成功", done.returncode != 0, f"exit={done.returncode}")
    check(
        "说清了是同一个家目录、另一个运行时目录",
        ("runtime=" in output) and (str(holder.proc.pid) in output),
        output.strip().splitlines()[-1][:110] if output.strip() else "(无输出)",
    )
    check(
        "给了可照做的出路",
        ("daemon restart" in output) or ("MIYU_HOME" in output),
    )
    check("占位的 daemon 没被顶掉", holder.alive())
    holder.kill()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None)
    args = parser.parse_args()

    binary = args.binary or os.environ.get("MIYU_BINARY")
    if not binary:
        target = os.environ.get("CARGO_TARGET_DIR", "target")
        binary = str(Path(target) / "debug" / "miyu")
    binary = Path(binary).resolve()
    if not binary.exists():
        print(f"找不到二进制：{binary}")
        return 2
    print(f"二进制：{binary}")

    workdir = Path(tempfile.mkdtemp(prefix="miyu-singleton-"))
    try:
        scenario_same_home(binary, workdir)
        scenario_separate_homes(binary, workdir)
        scenario_lock_released(binary, workdir)
        scenario_cli_is_told_why(binary, workdir)
    finally:
        shutil.rmtree(workdir, ignore_errors=True)

    print(f"\n通过 {len(PASS)} / {len(PASS) + len(FAIL)}")
    if FAIL:
        print("失败：" + ", ".join(FAIL))
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
