# Linux 0.6.0 CI 工作流

本次范围为用户更新后的 `linux-smoke` 发布目标。四份工作流和受控封装已实现、通过本地静态检查，尚未推送或在 GitHub runner 执行。此文不把远端未执行的编译、MSRV 或安装检查记为通过，也不宣称 macOS 已支持。

## 工作流职责

| 工作流 | 入口 | 实际职责 |
|---|---|---|
| `ci.yml` | PR、main push、手动 | Rust 1.96.1 fmt、Python packaging 单测、隔离 source-unit；另用 Rust 1.89.0 执行 `cargo check --locked --all-targets` |
| `release.yml` | 仅手动 | 默认只生成 `NOT_EXECUTED` dry-run 计划；显式执行才走准备、构建、安装、聚合、发布 |
| `build-package-verify.yml` | 受控 reusable workflow | 仅接受 `gnu-x86_64` 或 `arch-x86_64`，运行该构建对应的全部安装目标 |
| `packaging-update.yml` | 手动指定已完成 release run | 下载已聚合产物，在同一源码 ref 上生成 AUR PKGBUILD、.SRCINFO 和 patch；不 apply、不推送渠道仓库 |

Rust job 的两档工具链最多同时运行两个任务，且在格式/Python 检查通过后启动。发布构建矩阵也是 `max-parallel: 2`，GNU 与 Arch 各一条；各自的 core、voice 顺序构建。MSRV job 不使用 `continue-on-error`，当前依赖若不支持 1.89.0 会明确失败。

源码测试使用 `packaging/ci/run_tests.py --suite source-unit` 的归属标记和独立 HOME/MIYU_HOME/XDG、进程回收规则。PR 工作流不引用模型凭据。

## 运行主机与构建基线

2026-09-14 核对 `actions/runner-images` 官方 README 时，`ubuntu-26.04` 标记为 preview，`ubuntu-24.04` 为稳定的 `ubuntu-latest`。所以工作流采用 `${{ vars.LINUX_X64_RUNNER || 'ubuntu-24.04' }}`；管理员可设置该变量，但 runner 必须是具备 Docker 的原生 Linux x86_64。

host runner 的版本不决定 GNU ABI。GNU 构建仍使用源码中的 `packaging/linux/builders/Dockerfile.gnu` 及其 Debian 13 digest；Arch 使用固定快照 Dockerfile。封装创建镜像后解析实际 image ID，传给 build/package，并在使用完后删除这一个镜像，不清理用户其他镜像或容器。

Python 固定 3.11，发布 Rust 固定 1.96.1，MSRV 为 1.89.0。Cargo vendor 在准备 job 一次完成；两个 builder 消费同一快照。产品编译由现有 `build.py` 在 `--network none` 的容器内执行。

## 发布默认行为与执行条件

`release.yml` 没有 tag push 触发器。`workflow_dispatch.dry-run` 默认 `true`，只生成可下载的 `dry-run-plan.json`。它明确写 `status: NOT_EXECUTED`，不会调用 provider、伪造报告或调用 publish。

显式关闭 dry-run 后：

1. 检查专用 provider secret。
2. 要求输入 tag 恰为 `v<Cargo.toml version>`，这个已存在 tag、checkout HEAD 与控制工作流的 commit 必须一致；正整数 revision 经 argparse/metadata 校验。
3. `metadata.py --mode release --profile linux-smoke` 冻结源码，再完整 `prepare.py`。
4. GNU/Arch 原生容器构建 core/voice，stage 后打包。
5. Arch 跑 `arch-x86_64`；GNU 跑 Debian 13、Ubuntu 25.10、Ubuntu 26.04、冻结 Fedora 版本这四个安装目标。每个目标都实际调用 `verify.py`，使用专用 opencodego/deepseek-v4.1-flash 配置。GNU tar 的验收由 manifest 固定到 Debian 目标，不能漏掉。
6. `verify_release.py` 必须收到全部真实 PASS 报告，覆盖 final package hash；随后才能调用 `publish.py --dry-run` 检查最终 allowlist。
7. 单独 publish job 下载这份已校验 bundle，调用 `publish.py --execute`。唯一拥有 `contents: write` 的 job 是 publish。所有其他 job 默认只有 contents read；渠道 patch job 额外需要 actions read，以读取指定 run 的 artifact。

`notes-path` 默认 `docs/releases/0.6.0/release-notes.md`。执行发布时，这个文件必须已经纳入所选源码提交且非空。路径必须位于 checkout 内。第一次正式远端运行仍需实际验证 runner 容量、上游源可达性和专用凭据；本地 actionlint 不能证明这些条件。

## 专用凭据契约

只有显式执行发布的准备预检和安装验证步骤读取 `OPENCODEGO_PROVIDER_CONFIG`。reusable workflow 通过同名 secret 显式传参，不使用 `secrets: inherit`。构建、打包、聚合不接收该凭据。

secret 必须是只含一个 `providers` 数组的 JSON，并且数组里恰好一个 `id: opencodego` 的 provider。需要非空 `api_key`，`models` 包含 `deepseek-v4.1-flash`；可保留 `display_name`、`base_url`、`protocol`。使用专用测试凭据，不复制完整生产配置。

wrapper 只保留上述 provider 字段，在输出树以外创建 0600 临时配置；验证结束后 finally 删除。artifact 只包含源码输入、包或已脱敏报告，从不打包这个临时配置。凭据缺失返回 exit 3，不能放行非 dry-run 发布。provider 调用成功与否仍由真实安装验收报告决定。

## 跨 job 文件传输

源码快照和 prepared inputs 放在一个 gzip tar 中传递。结果包/报告、聚合发布目录也分别使用 tar，GitHub artifact 上传关闭二次压缩。不能直接 upload 源码目录，让 artifact 服务把执行位归一化。

wrapper 解包先调用安全 tar 检查，拒绝绝对路径、`..`、逃逸链接、链接环和非空目的目录，再恢复 tar 中普通文件的精确权限，拒绝特殊权限位。这一步有实际依据：已准备的 `fnv-1.0.7` vendor 里有六个 0640 文件，直接用上游资源解包器归一成 0644 会使 `verify_prepared` 失败。已经实测 0640、0755、相对符号链接的归档往返均保持不变。

恢复后 build/stage 仍验证 source snapshot、manifest SHA 和 prepared 全量文件库存。传输归档不是额外信任来源。发布只接受两个固定 build ID 的结果，重复 asset/report 目录会失败。失败日志另打包到 `failed-results-*`，不混入可发布结果。

渠道更新需要在对应 release source tag/ref 上 dispatch。封装会准备一个 Arch 镜像，给 `channel_update.py --builder-image <实际 image ID>` 使用，避免依赖本机才有的镜像名；可选 `published-url` 触发正式 Release 回读校验。流程不会调用 `--apply` 或任何 AUR push。

## Actions 固定值与真实检查

下列值来自 GitHub API 对官方 repository tag 的实际解析，工作流全部使用完整 commit SHA：

| Action | 查询 tag | 已固定 commit |
|---|---|---|
| actions/checkout | v4 | `11d5960a326750d5838078e36cf38b85af677262` |
| actions/setup-python | v5 | `a26af69be951a213d495a4c3e4e4022e16d87065` |
| actions/upload-artifact | v4 | `ea165f8d65b6e75b540449e92b4886f43607fa02` |
| actions/download-artifact | v4 | `d3f86a106a0bac45b974a628896c90dbdf5c8093` |

实际下载并运行 actionlint 1.7.12，校验其官方 release checksums，Linux amd64 归档 SHA256 为 `8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8`。四份工作流实际通过 actionlint，exit 0。

本地实际通过的检查：

- metadata、prepare、build、stage、package、verify、verify_release、publish、run_tests、workflow、channel_update 共 11 个脚本的 `--help`。
- workflow 全部 10 个子命令的 `--help`。
- 默认 release plan 输出 `NOT_EXECUTED`，不调用构建、模型或发布。
- 缺专用 provider secret 时 exit 3。
- 0640/0755/相对链接保持，以及非空解包目录拒绝。
- 官方 Actions SHA 查询、runner 标签核对和 actionlint 1.7.12。

取证位于 `out/distribution/ci-check/`：`action-pins.json`、`runner-images-readme.md`、`script-interface-checks.json`、`workflow-static-checks.json`、`actionlint.txt`。测试用临时目录已自动清理；actionlint 临时二进制和下载归档在最终检查完成后删除，保留版本/来源/校验记录。

未执行：GitHub 远端 workflow、远端 MSRV 构建、远端五发行版 provider 验收、远端发布和渠道 push。本次没有操作远端。用户当前批准的本地 0.6.0 安装/发布链由主任务独立执行。
