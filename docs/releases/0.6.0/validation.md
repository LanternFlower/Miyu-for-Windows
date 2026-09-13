# Miyu 0.6.0 发布验收记录

正式版本：[Miyu 0.6.0](https://github.com/SHORiN-KiWATA/miyu-agent/releases/tag/v0.6.0)。发布时间 `2026-09-13T18:05:30Z`。

应用源码为 `bc7087f4c4b03adcef7f8cc8fce64a1c1abb0aaa`，标签 `v0.6.0`，源码快照 `93f6f1d883aca2e2fd617fef5624fba17b333be6db013f33f33d9d881f4a93d7`。
交付工具的 GitHub 草稿查询与渠道目录权限修正在后续提交中记录；应用包内容和公开标签未改写。

## 实际安装与模型回复

五个干净 Linux x86_64 容器执行真实依赖安装、包头/文件清单/hash/版本检查，并由安装后的 Miyu 调用 `opencodego/deepseek-v4.1-flash`。GNU tar 使用独立前缀与 home。全部 36 项必需检查通过。

| 目标 | 结果 | 必需检查 | 实际核心回复 |
|---|---|---:|---|
| arch-x86_64 | PASS | 6 | Hello, 有什么需要帮忙的？ |
| debian13-x86_64 | PASS | 12 | Hello there / Hello, I'm Miyu. |
| ubuntu2510-x86_64 | PASS | 6 | Hello，我是Miyu。 |
| ubuntu2604-x86_64 | PASS | 6 | 你好呀，今天过得怎么样？ |
| fedora-current-x86_64 | PASS | 6 | Hello there. |

## 其他验证

- 最终源码 `refactor-check.sh` 全绿，2,337 个源码/集成测试通过。
- Rust 1.89.0 `cargo check --locked --all-targets` 通过，原子计数修复相关既有测试 14/14 通过。
- 打包 Python 测试最终 70/70 通过；新增发布回归先在修复前报红。
- 四份 GitHub 工作流通过 actionlint。远端 Actions 未运行，本次发布使用已记录的本地容器链。
- 四个最终二进制来自固定镜像中的真实断网 release 构建。两个 Arch 包 namcap E 为零。
- 最终核心二进制的 OOBE 实跑并捕获四个页面；Release 正文包含四张真实 PTY 截图。
- 21 个正式附件逐个上传、下载回读 SHA256，精确 allowlist 一致后才从草稿转正式。GitHub latest 为 v0.6.0。
- AUR 主包/voice 从正式 URL 下载并重包后再次在干净 Arch 容器安装，6 项检查及真实模型输出通过。重复生成渠道文件的 patch 为空。

## 清理与边界

- 本次 7 个镜像、55 项构建/下载/重复源码路径已清理，测试容器为零，临时 MIYU_HOME 和供应商配置无残留。既有 Docker 镜像/卷未动。
- 保留工作区、最终 publish 目录和必要的 JSON/日志证据；本地证据总量约 356 MiB。
- 宿主生产 Miyu 未升级，main 未合并，原 main 的 next-release-note.md/todolist.md 和三个有本地改动的 AUR 检出均保留。
- 本次不提供 macOS 正式资产。voice 安装和版本通过不代表物理麦克风验收，也不声称完成原计划的所有升级、托管服务和恢复检查。
- 仓库 AUR 配方和 .SRCINFO 已同步公开资产。独立 AUR 仓库和额外软件源未推送。

公开证据位于 Release 的 `acceptance-0.6.0-1.json`、`provenance-0.6.0-1.json`、`release-manifest-0.6.0-1.json`、SHA256SUMS 与四份文件清单 SPDX。文件清单 SPDX 不等于完整依赖 SBOM。

本地证据入口：`out/distribution/release-0.6.0/publish/`、`out/distribution/aur-channel-acceptance/report.json`、`out/distribution/cleanup-report.json`。
