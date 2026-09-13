# Miyu 发布流程

本手册按 0.6.0 实际执行的 Linux 容器构建、安装和发布链更新。工具入口在
`packaging/ci/`，资源清单在 `packaging/common/assets.json`，Arch 四份配方仍以
`packaging/arch/` 为真相源。远端 CI 的职责与凭据契约见
[CI 说明](docs/plan/distribution/ci.md)。

## 范围与工作区

0.6.0 使用 `linux-smoke`：Arch、Debian 13、Ubuntu 25.10、Ubuntu 26.04、Fedora 44，
均为 Linux x86_64。公开附件仅为 Arch、DEB、RPM 的主程序与 voice，共六个包。GNU tar 仅保留内部验收。
主程序要求真实安装、资源/版本校验及指定供应商正常回复；voice 要求实际安装和版本校验。
GNU tar 使用独立安装前缀与 MIYU_HOME。物理麦克风、Mac 和完整升级恢复不在这次验收范围。

在独立 worktree 操作。是否合并 main、部署宿主、推送 AUR 或更新另一个软件源，取决于
当前任务的明确范围；这些都不是“创建 GitHub Release”自动执行的附带步骤。
不要覆盖其他工作区、AUR 检出的未提交改动，也不要无条件删除 `~/.local/bin/miyu`。

## 1. 准备与源码门禁

版本同时写入 Cargo.toml、Cargo.lock 和源码发布配方。二进制 AUR 包装器保留上次公开
资产的真实版本/hash，等新资产发布并回读后再更新，避免提前发布不存在的下载地址。

源码通过 `cargo fmt --check`、隔离的 `test_scripts/refactor-check.sh`、声明 MSRV 的
`cargo check --locked --all-targets` 和 Python 打包测试。产品测试必须使用临时
HOME/MIYU_HOME/XDG；进程与目录清理由 `Sandbox`、`ProcessSupervisor` 管理。
`packaging/ci/run_tests.py` 提供预定义源码测试入口。

```bash
python3 -m unittest discover -s packaging/ci/tests -v
python3 packaging/ci/run_tests.py --suite source-unit --report-dir out/distribution/source-unit
```

先用 preview 输入完成真实安装排错。源码完成后提交 release commit，创建本地版本 tag。
正式输入要求源码 clean，tag、Cargo 版本与 source commit 一致。无需为了构建先合并 main
或提前公开未验收的 tag。

## 2. 冻结并准备最终输入

以下为 0.6.0 的实际入口；重跑应选新的空输出目录。metadata 同时保存源码快照和文件清单。

```bash
python3 packaging/ci/metadata.py --mode release --source-ref HEAD --tag v0.6.0 \
  --profile linux-smoke --revision 1 --out out/distribution/release-0.6.0/release-input.json
python3 packaging/ci/prepare.py --manifest out/distribution/release-0.6.0/release-input.json \
  --out out/distribution/release-0.6.0/inputs
```

已验证的下载缓存和相同 Cargo.lock 的 vendor 可通过 `--cache`、`--vendor-cache` 复用；
`--offline` 要求缓存完整，否则失败。Wiki、模型、ORT、sherpa 与工具下载均受锁定 hash 校验。
GNU 构建镜像基于 Debian 13；Arch 使用锁定基础镜像和 Archive 2026/09/13 软件快照。
两个 Dockerfile 位于 `packaging/linux/builders/`。记录准备出的实际 image ID。

## 3. 构建与打包

对 `gnu-x86_64`、`arch-x86_64` 各构建 core/voice。`build.py` 必须接收 manifest、inputs、
build-id、component、out、builder-image；可指定该 component 独占的 `--target-cache`。
最多两条构建并行，每条 jobs=2。编译在 `--network none` 容器内以 `--release --frozen` 执行，
保留 thin LTO/codegen-units=1；链接几分钟属正常，不能因日志暂时不变就重启构建。

每个 build-id 依次调用 `stage.py` 和 `package.py`。stage 接收对应 `--build-root`；package
按 manifest 中的每个 `--asset-id` 生成到独立 `--out`。DEB/RPM 传 checksum 验证过的绝对
`--nfpm` 路径，Arch 传实际 `--builder-image`。各脚本 `--help` 为当前参数契约。

发布包必须包含字体、模型、表情、脚本、知识库和许可证。构建记录经 stage/package
绑定到实际二进制；不能只改 JSON 中的 source/hash 来复用旧二进制。Arch 使用系统 ORT，
GNU 使用私有 CPU ORT。Arch namcap E 会拒绝打包，RPM 不声明发行版共有目录的所有权。

## 4. 实际安装与模型验收

对 manifest 的五个 target-id 分别运行 `verify.py`：

```bash
python3 packaging/ci/verify.py --manifest out/distribution/release-0.6.0/release-input.json \
  --packages out/distribution/release-0.6.0/packages --target-id debian13-x86_64 \
  --report-dir out/distribution/release-0.6.0/reports/debian13-x86_64 \
  --provider-config /home/shorin/.miyu/config/config.jsonc
```

这条本机配置路径仅用于本次用户已授权的本地验收。脚本只临时复制 opencodego provider，
调用 deepseek-v4.1-flash，不上传凭据。远端 Actions 另需专用测试 secret，不能把宿主配置
上传为 artifact。成功要求请求退出正常、最终回复非空、provider/model 正确，不要求人格
逐字照抄某个测试口令。

所有目标报告都必须指向最终包 hash。首次失败报告保留，重试用新目录。最终聚合目录只放
各目标适用的成功报告。容器必须确认已移除，才能删除 bind-mounted home 并宣布清理完成。
不要按日期猜测 Ubuntu 旧版本已经迁到 old-releases，保留能实际验证的官方源。

## 5. 聚合、上传与回读

```bash
python3 packaging/ci/verify_release.py --manifest out/distribution/release-0.6.0/release-input.json \
  --artifacts out/distribution/release-0.6.0/packages --reports out/distribution/release-0.6.0/reports \
  --publish-dir out/distribution/release-0.6.0/publish
python3 packaging/ci/publish.py --manifest out/distribution/release-0.6.0/release-input.json \
  --dir out/distribution/release-0.6.0/publish --dry-run
```

聚合生成精确资产清单、SHA256SUMS、文件清单 SPDX、实际构建来源和脱敏 acceptance JSON。
OOBE 截图来自真实 PTY；图片与说明放 `docs/releases/<version>/`，图片使用仓库链接，不作为 Release 附件。校验值放在正文折叠区。
文件清单 SPDX 不等于完整依赖 SBOM。

全部验证成功后推送已批准的分支和 tag，再把 dry-run 换为 `--execute`，同时传
`--notes docs/releases/0.6.0/release-notes.md`。内部 bundle 完整验证后，只选取六个发行版包。上传先创建 draft，每个文件上传后下载回读
hash，完整 allowlist 再次核对后才转正式。已有同名异内容或额外远端资产会失败，禁止 clobber。

## 6. 同步渠道与收尾

正式 Release 回读成功后，运行 `channel_update.py`，传最终 manifest、release-output、
published-url、空 out、builder-image；`--apply` 同步仓库 AUR 包装器和 `.SRCINFO`。
用生成的配方从正式 URL 下载并在容器重包安装，确认主包/voice 版本、资源和别名一致。
渠道变更单独提交，不能移动已经公开的 release tag。是否推送独立 AUR 仓库按当前任务授权；
有本地改动的检出先保留，不能直接覆盖。`miyu-git` 的远端 main 必须实际包含新资源脚本后
才能发对应 VCS 配方。

发布说明先归档到版本 changelog，再整理下一版记录。记录最终 tag、资产 hash、验收报告和
清理结果。移除本次的临时 homes、进程、容器、已登记镜像、重复源码与构建/下载缓存，保留
最终发行资产和必要证据。禁止全局 Docker prune，也不删除生产数据或共享的用户工具链缓存。

宿主升级、daemon 轮换和额外软件源同步属于独立部署步骤，本次容器发布不执行。

## 同版本重编（仅在用户明确要求替换发布包时）

默认应发布新的应用补丁版本。用户明确要求同版本重编时，递增 package revision，
重新提交源代码、构建全部包并执行真实安装验收。保留旧 tag commit 与原资产本地备份，
发布说明写明重编原因、修订号和新源码。验证成功后才更新版本 tag（用旧远端值作为
force-with-lease 条件），使自动源码下载对应实际构建源码；不得伪改构建记录复用旧二进制。

先上传并回读新修订的六个包，再移除旧包和内部附件，最后验证远端精确六附件名单。
替换期间暂停正常发布器的“远端不得有额外资产”步骤；这是人工执行的有旧新资产
清单及 hash 校验的迁移，正常发布器继续拒绝冲突与额外资产。之后用正常发布器再次
核验最终状态。按新 hash 更新仓库/AUR 配方和正文 SHA256，不能沿用旧校验值。
