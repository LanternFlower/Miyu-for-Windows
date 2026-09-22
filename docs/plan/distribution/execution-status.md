# Distribution implementation status

总体进度：100%。用户修改后的 Linux 0.6.0 容器安装、真实模型验收、正式 Release 和环境清理已完成。

- 正式发布：https://github.com/SHORiN-KiWATA/miyu-agent/releases/tag/v0.6.0
- 应用 release commit：`bc7087f4c4b03adcef7f8cc8fce64a1c1abb0aaa`。
- 工作分支：`worktree-distribution-2026-09-14`，未合并 main。
- 五个 Linux 安装目标、八个包、36 项必需检查全部通过。AUR 包装包另做实际重包安装和模型输出验证。
- 正式附件 21 个，包含四张 OOBE 截图；全部上传后下载回读 SHA256。
- 本次容器、七个镜像、临时供应商配置及构建/下载缓存已清理，保留最终资产和证据。
- 原完整 T00–T24 的 Mac、硬件、托管服务、升级恢复等未验证项没有被记为 PASS。当前任务按用户后续修改的 linux-smoke 标准完成。

详见 [最终发布验收记录](../../releases/0.6.0/validation.md)、[流程修复证据](gate-repairs.md) 和 [更新后的发布手册](../../../miyu-release-workflow.md)。
