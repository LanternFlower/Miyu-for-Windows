# Miyu 0.6.0

这次先从「第一次见面」说起。新装 Miyu 会带你选人格、勾功能、介绍自己、接好终端，再选一个模型；老用户升级不会被打断，也可以随时运行 `miyu oobe` 重走一遍。

![Miyu 首次见面](https://github.com/SHORiN-KiWATA/miyu-agent/releases/download/v0.6.0/01-welcome.png)

## 五屏新手引导

用内置 Miyu，或给自己的角色起名字、写设定；记忆、知识库、MCP、技能和内置脚本可以逐项选择。接模型页提供国内外供应商预设，也支持本机已登录的 CLI 后端。每屏确认后保存，中途退出也能继续。

![选择人格](https://github.com/SHORiN-KiWATA/miyu-agent/releases/download/v0.6.0/02-persona.png)
![选择功能](https://github.com/SHORiN-KiWATA/miyu-agent/releases/download/v0.6.0/03-features.png)
![接入模型](https://github.com/SHORiN-KiWATA/miyu-agent/releases/download/v0.6.0/04-provider.png)

以上图片均来自真实 `miyu 0.6.0` 终端输出。

## 终端与网页的新界面

- 空会话有了星空大厅和渐变 MIYU 艺术字。第一句话发出前可按 Tab 切换普通 / 开发模式。
- 全屏终端以时间线展示思考、命令和编辑，完成后可收成一行；点开看完整输出、diff 和子代理自己的过程。`MIYU_TUI=1 miyu` 可进入全屏界面。
- WebUI 的工具过程、队列、附件与子代理进度统一了展示；artifact 预览支持可交互 HTML 和内置 ECharts，仍保留隔离边界。
- MCP、脚本和技能遵循人格范围。纯中文人格名、知识库重建、后台子代理和多模型轮询等多处问题得到修复。

## Linux 安装包

本次提供 **Linux x86_64** 的主程序与可选 voice：Arch `.pkg.tar.zst`、Debian/Ubuntu `.deb`、Fedora 44 `.rpm`，以及可移动的 GNU tar。主包自带字体、语义模型、表情、脚本、默认知识库和许可证；GNU 渠道携带私有 CPU ONNX Runtime。

安装目标为 Arch 固定快照、Debian 13、Ubuntu 25.10 / 26.04、Fedora 44。Ubuntu 25.10 已结束上游维护，建议新用户选择 26.04。本次验收要求在各容器中实际解析依赖安装，并由包内 Miyu 调用 `opencodego/deepseek-v4.1-flash` 正常回复。

```bash
# Debian 13 / Ubuntu 25.10、26.04
sudo apt install ./miyu_0.6.0-1_amd64.deb

# Fedora 44
sudo dnf install ./miyu-0.6.0-1.fc44.x86_64.rpm

# Arch Linux
sudo pacman -U ./miyu-0.6.0-1-x86_64.pkg.tar.zst
```

需要语音时再安装同版本 `miyu-voice` 包。现有 `MIYU_HOME` / `~/.miyu` 布局保持不变。macOS 的资源与沙盒适配已开始，但尚未完成 Apple Silicon 原生验收，本次不提供 Mac 正式资产。容器对话验收也不代表麦克风硬件验收。

发布资产附 SHA256 校验和、文件清单 SBOM、构建来源记录和含真实模型回复的验收报告。构建固定源码/资源快照，使用 Debian 13 用户态并断网编译；发布前核对所有必需安装报告，上传后再次下载校验，不覆盖同名不同内容的资产。

[查看完整更新记录](https://github.com/SHORiN-KiWATA/miyu-agent/blob/v0.6.0/docs/releases/0.6.0/changelog.md)
