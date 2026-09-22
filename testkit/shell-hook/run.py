#!/usr/bin/env python3
"""shell hook 的版本标记与自动更新。

用户 09-19：「给生成的 miyu.fish 加上版本号，这样我后续可以做到自动更新」，
并选了「三个 shell 都加」「版本号 + 内容指纹」「装着的对不上就自动换」。

量的是三件事：

1. `fish-init` / `bash-init` / `zsh-init` 生成的文件顶上有那行标记，格式是
   `# miyu shell hook · v<版本> · <8 位指纹>`；
2. 手里那份**过期**（改过内容、或者版本号被改旧）时，随便跑一条 miyu 命令
   就会被换回当前这份；
3. **没装过的不会被装上**。这条最要紧：自动更新只能刷新已经存在的文件，
   不能替用户开启 shell 集成。

隔离要指**三个**环境变量，少一个就会动到你真正在用的文件：

- `MIYU_HOME`：bash / zsh 的 hook 文件在它下面；
- `XDG_CONFIG_HOME`：fish 的 hook 在 `~/.config/fish/conf.d/` 下，**不跟着
  `MIYU_HOME` 走**；
- `HOME`：`bash-init` / `zsh-init` 还会往 `~/.bashrc` `~/.zshrc` 里塞一段
  source 块，认的是 `$HOME`（第一版漏了这个，真把两段块写进了我自己的
  rc 文件里）。

也正因为 fish 那条路管不住，产品里默认「设了 MIYU_HOME 就不自动更新」，
这里拿 `MIYU_SHELL_HOOK_SYNC=1` 强制打开。

跑法（先 cargo build）：

    python3 testkit/shell-hook/run.py
"""

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("MIYU_BIN", ROOT / "target" / "debug" / "miyu"))
SANDBOX = Path(os.environ.get("SANDBOX", "/tmp/miyu-shell-hook"))
HOME = SANDBOX / "home"
XDG = SANDBOX / "xdg"
FAKE_HOME = SANDBOX / "fakehome"
STAMP = re.compile(r"^# miyu shell hook · v(\S+) · ([0-9a-f]{8})$")

SHELLS = {
    # shell: (子命令, hook 文件相对 SANDBOX 的位置)
    "fish": ("fish-init", XDG / "fish" / "conf.d" / "miyu.fish"),
    "bash": ("bash-init", HOME / "config" / "shell" / "bash-hook.sh"),
    "zsh": ("zsh-init", HOME / "config" / "shell" / "zsh-hook.zsh"),
}


def run(args, sync=False):
    env = dict(
        os.environ,
        MIYU_HOME=str(HOME),
        XDG_CONFIG_HOME=str(XDG),
        # `bash-init` / `zsh-init` 会往 `$HOME/.bashrc` `$HOME/.zshrc` 里
        # 塞 source 块——不指开就是写进跑走查这个人的 rc 文件里。
        HOME=str(FAKE_HOME),
        # 家目录隔离时产品默认不碰 shell 配置（见 refresh_shell_hooks）。
        # 这个走查要验的就是那条路，所以强制打开。
        **({"MIYU_SHELL_HOOK_SYNC": "1"} if sync else {}),
    )
    env.pop("MIYU_TUI", None)
    return subprocess.run(
        [str(BIN), *args], env=env, cwd=str(SANDBOX),
        capture_output=True, text=True, timeout=120,
    )


def stamp_of(path):
    """读出 (版本, 指纹)；没有标记返回 None。"""
    if not path.exists():
        return None
    first = path.read_text(encoding="utf-8").splitlines()[:1]
    if not first:
        return None
    match = STAMP.match(first[0])
    return match.groups() if match else None


def main():
    if not BIN.exists():
        print(f"! 先 cargo build：{BIN} 不存在", file=sys.stderr)
        return 2
    if SANDBOX.exists():
        shutil.rmtree(SANDBOX)
    SANDBOX.mkdir(parents=True)
    FAKE_HOME.mkdir(parents=True)
    report = {}

    # 一、三个 init 都写出带标记的文件
    for shell, (command, path) in SHELLS.items():
        run([command])
        stamp = stamp_of(path)
        report[f"{shell} 生成的 hook 带版本标记"] = stamp is not None
        report[f"_{shell} 标记"] = stamp
    versions = {
        shell: stamp_of(path)[0]
        for shell, (_, path) in SHELLS.items()
        if stamp_of(path)
    }
    report["三个 shell 的版本号一致"] = len(set(versions.values())) == 1
    fingerprints = {
        shell: stamp_of(path)[1]
        for shell, (_, path) in SHELLS.items()
        if stamp_of(path)
    }
    # 指纹认的是「这一份脚本」，三个 shell 的脚本不一样，指纹就该不一样。
    report["三个 shell 的指纹各不相同"] = len(set(fingerprints.values())) == 3

    # 二、内容被改过 → 跑一条命令就换回来
    fish_path = SHELLS["fish"][1]
    current = fish_path.read_text(encoding="utf-8")
    fish_path.write_text(
        current.replace("# miyu shell hook · v", "# miyu shell hook · v0.0.1-old · ", 1)
        + "\n# 手改的一行\n",
        encoding="utf-8",
    )
    run(["paths"], sync=True)
    healed = fish_path.read_text(encoding="utf-8")
    report["过期的 hook 会被自动换回来"] = healed == current
    report["_换回来之后的标记"] = stamp_of(fish_path)

    # 三、没装过的不会被装上
    for shell, (_, path) in SHELLS.items():
        path.unlink()
    run(["paths"], sync=True)
    report["没装过的 shell 不会被自作主张装上"] = not any(
        path.exists() for _, path in SHELLS.values()
    )

    # 四、隔离家目录下默认不碰（不给 MIYU_SHELL_HOOK_SYNC 就不动）
    for shell, (command, _) in SHELLS.items():
        run([command])
    stale = fish_path.read_text(encoding="utf-8").replace(
        "# miyu shell hook · v", "# miyu shell hook · v0.0.1-old · ", 1
    )
    fish_path.write_text(stale, encoding="utf-8")
    run(["paths"])  # 不强制
    report["隔离家目录下默认不碰用户的 shell 配置"] = (
        fish_path.read_text(encoding="utf-8") == stale
    )

    # 五、那个名字下放着别人的文件：一个字都不许动
    #
    # 路径是写死的（`conf.d/miyu.fish`），`fish-init` 是用户明确要求的、照旧
    # 覆盖；自动那条路不是，不该把不认识的文件吃掉。
    foreign = "# 我自己写的\nalias m=miyu\n"
    fish_path.parent.mkdir(parents=True, exist_ok=True)
    fish_path.write_text(foreign, encoding="utf-8")
    run(["paths"], sync=True)
    report["不认识的文件一个字都不动"] = (
        fish_path.read_text(encoding="utf-8") == foreign
    )

    # 六、符号链接进 dotfiles 仓库：换内容，别把链接换成普通文件
    repo = SANDBOX / "dotfiles"
    repo.mkdir(exist_ok=True)
    real = repo / "miyu.fish"
    fish_path.unlink()
    run(["fish-init"])  # 先生成一份正常的
    real.write_text(fish_path.read_text(encoding="utf-8"), encoding="utf-8")
    fish_path.unlink()
    fish_path.symlink_to(real)
    real.write_text(
        real.read_text(encoding="utf-8").replace(
            "# miyu shell hook · v", "# miyu shell hook · v0.0.1-old · ", 1
        ),
        encoding="utf-8",
    )
    run(["paths"], sync=True)
    report["链接进 dotfiles 的 hook 还是链接"] = fish_path.is_symlink()
    report["链接指向的那份被换成了当前版本"] = (
        stamp_of(real) is not None and stamp_of(real)[0] != "0.0.1-old"
    )

    # 七、rc 文件只动沙箱里那份（第一版漏指 HOME，写进了真实的 ~/.bashrc）
    report["rc 文件写在沙箱里"] = (FAKE_HOME / ".bashrc").exists() and (
        "miyu bash hook" in (FAKE_HOME / ".bashrc").read_text(encoding="utf-8")
    )

    checks = {k: v for k, v in report.items() if not k.startswith("_")}
    passed = 0
    for name, ok in checks.items():
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
    print(f"\n{passed}/{len(checks)} passed")
    for name, value in report.items():
        if name.startswith("_"):
            print(f"   {name[1:]}: {value}")
    return 0 if passed == len(checks) else 1


if __name__ == "__main__":
    raise SystemExit(main())
