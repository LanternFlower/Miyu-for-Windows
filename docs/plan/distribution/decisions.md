# Distribution implementation decisions

The supplied 2026-09-13 execution plan fixes the six target IDs and T00–T24 scope. No target is removed to make checks pass. Progress is VERIFIED tasks / 25, separate from local implementation.

- Work on `worktree-distribution-2026-09-14`, based on main `b4d2ac92`. The user subsequently authorized committing and publishing version 0.6.0 after automated acceptance, without waiting for manual approval. Continue on this worktree; do not directly merge main.
- Use uncommitted preview snapshots with explicit new-source inclusion. Build/test evidence must bind both git commit and snapshot hash.
- Linux preview may exercise a declared target subset during development. This is not full completion. Stable profiles keep all six core targets and existing Linux voice.
- Freeze signing policy before building: preview Mac direct downloads are unsigned prereleases; stable direct-download Mac core/voice require signed-download verification. Homebrew-only unsigned core can be considered only as a separately declared, validated channel, not a runtime bypass of stable direct-download checks.
- Mac voice hardware/TCC and Apple signing remain required external validations. No Linux cross-compile, mock or missing-credential skip can satisfy them.
- Keep glibc 2.41 GNU build baseline, current Arch system ORT provider, pinned GNU private CPU ORT, thin LTO/codegen-units=1, existing user directory/database/prompt contracts.
- The repository has four PKGBUILDs (the older AGENTS build note says three). The plan and current source agree on preserving all four.
- Reuse current `test_scripts/refactor-check.sh` path. Its comments are stale; actual script and exit status are authoritative.

## User amendment (2026-09-14, authoritative over the original test bar)

The user changed acceptance to actual container package installation plus a successful real Miyu response using the existing `opencodego/deepseek-v4.1-flash` provider. After all applicable targets pass, publish a new 0.6.0 release autonomously. Review and fix release-process defects as needed. Include real OOBE screenshots in the release note. Test environments must be cleaned after use. Credentials must remain ephemeral and must not enter logs, source exports or artifacts.

The full implementation plan remains a reference, but its hardware/manual acceptance and no-publication constraints are superseded where they conflict with this explicit instruction. The Mac publication scope has been asked separately because Linux containers cannot supply native Apple Silicon evidence. No Mac success claim may be inferred from Linux tests.

### 0.6.0 Linux smoke release profile

Following the changed acceptance instruction, `linux-smoke` is an additional explicit profile. It freezes all five Linux installation targets and retains main + voice packages for Arch/GNU. Required core checks are artifact identity, real dependency-resolving installation, complete packaged resources and a successful real `opencodego/deepseek-v4.1-flash` response. Voice requires matching artifact identity and actual package installation. GNU tar assets are validated independently. No physical microphone, managed-service or native Mac claim follows from these smoke results. Original stable-core/stable-full profiles and their stronger checks remain intact.

Mac scope was asked asynchronously. Pending a different answer, proceed with the recommended Linux-only 0.6.0 asset set, with Mac adaptation explicitly unverified and unreleased.

## GNU 构建基座下探到 Ubuntu 24.04（2026-09-20，推翻上面的 glibc 2.41 基线）

DEB 声称支持 Ubuntu 25.10 起，原因不是发行版特性，而是**构建基座**：GNU builder
用的是 Debian 13 镜像（glibc 2.41），产物引用 2.41 的符号，装到更老的系统上起不来，
于是包里写死 `libc6 (>= 2.41)`。24.04 是 LTS，支持到 2029 年，挡在门外没有道理。

- GNU builder 基座换成 `ubuntu:24.04`（实测 24.04.5，glibc **2.39**；工具链 gcc 13.3 /
  clang 18.1.3 / cmake 3.28.3 / alsa 1.2.11 / openssl 3.0.13，够用）。
- 包里的 glibc 下限不再写字面量，从 `toolchain.lock.json` 的
  `builders.gnu-x86_64.glibc` 读——基座和声明必须是同一个数，以前它在 nfpm 的两份
  yaml 和 `package.py` 里各写了一遍。
- 安装验收目标 `ubuntu2510-x86_64` → `ubuntu2404-x86_64`：两个 Ubuntu 目标改为
  一头一尾（支持下限 + 最新），中间版本的 glibc 都在两者之间。
- Debian 13（glibc 2.41）继续验收，不受影响；Arch、Fedora、macOS 目标不动。
- 24.04 仓库里运行时依赖齐全（实测）：chafa 1.14.0、ripgrep 14.1.0、libasound2t64
  1.2.11、python3 3.12.3。chafa 1.14.0 支持 `--relative`、不支持 `--probe`，正是
  `testkit/chafa-compat` 覆盖的档位。
