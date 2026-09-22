# Miyu 0.6.0-2 重编验收

用户要求默认进入全屏 TUI，并重新编译替换 0.6.0 资产。本次源码为
`9142225c0adeb4e3740586c440ab085d5f36feb2`，包修订为 2。
旧 v0.6.0 源码 `bc7087f4c4b03adcef7f8cc8fce64a1c1abb0aaa` 已留本地备份；
替换时同步版本 tag，以确保源码下载与新二进制一致。

## 新包实测

| 目标 | 结果 | 检查数 | 实际核心回复 |
| --- | --- | ---: | --- |
| arch-x86_64 | PASS | 6 | 你好，今天想聊点什么。 |
| debian13-x86_64 | PASS | 12 | Hello，这个点该睡了 / Hello，晚上好。 |
| fedora-current-x86_64 | PASS | 6 | Hello，这么晚还不睡？ |
| ubuntu2510-x86_64 | PASS | 6 | Hello。 |
| ubuntu2604-x86_64 | PASS | 6 | 晚上好，这个点了还不睡？ |

全部 36 项检查通过，测试使用隔离 MIYU_HOME，供应商为本机配置中的
opencodego/deepseek-v4.1-flash。voice 实际安装与版本检查通过，不代表麦克风硬件验收。
GNU tar 保留内部重定位验收，不作为公开附件。

旧包在真实 PTY 中默认没有进入备用屏。相同回归用例在旧选择逻辑下报红，修复后通过。
新 GNU 与 Arch 二进制在 MIYU_TUI 未设置时均进入全屏，MIYU_TUI=0 回退测试通过。
Arch 包字体清单已检查，namcap E 为零。发布器相关 Python 测试 73 项通过。

本次公开资产仅包含六个发行版包。七张图片引用仓库，六个 SHA256 放在 Release 正文折叠区。
详细本地证据：out/distribution/revision-2/ 下的 release-input.json、reports/、publish/、
*-default.json、new-inline.json 与发布/清理记录。


## 渠道与本机同步

- 六个公开安装包上传后逐个下载回读 SHA256，正常发布器再次确认远端精确六附件。
- 七张图片通过仓库 URL 回读并核对本地原图 hash。正文已按完整 next-release-note 重写。
- AUR miyu / miyu-voice 从正式 URL 下载并重包，干净 Arch 容器安装与真实模型回复的 6 项检查再次通过。
- 本机 miyu、miyu-voice 已升级到 0.6.0-2，实际执行路径为 /usr/bin/miyu，daemon 已轮换。
- 删除 ~/.local/bin/miyu 的旧调试覆盖文件（1,519,667,864 字节）；保留其他本地工具。
- main 已合并并推送。用户 todolist.md 原样保留，next-release-note 完整归档后清理。



## 发布后的 CI 环境修复

首次 main CI 的 Rust 1.96 测试在无终端 runner 上出现三项失败：两个搜索测试因未安装 ripgrep 返回 ENOENT，图片尺寸测试因没有 TERM/控制终端未得到尺寸。测试清理器读取不可 dump 的后代进程 environ 又触发 EACCES，掩盖了原测试退出码。

CI 现明确安装 ripgrep，图片尺寸在 cfg(test) 下使用固定 80×24。子收割器按本进程收养关系清理，排除预存子进程，并限时确认回收；命令退出码与清理错误分别保留。六项清理回归通过，Python 总计 79 项通过。三项 Rust 回归已分别复现修前失败，并在删除 TERM/COLORTERM、stdin 为 DEVNULL、无控制终端的环境中验证通过。这些改动不改变已发布程序的运行路径，v0.6.0-2 包仍严格对应上述构建源码。


本轮七个新增 Docker 镜像与 26 项独立构建/测试路径已清理，测试容器为零。保留最终发行 bundle 与必要报告；三个原有 Docker 镜像、用户工具链缓存、用户 todolist.md 和 AUR 检出中的原有非跟踪构建产物均保留。

---

# 首批 0.6.0-1 发布记录（历史）

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
