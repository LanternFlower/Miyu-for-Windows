# Miyu 0.6.0 · 从第一次见面，到完整的工作时间线

0.6.0 带来了五屏新手引导、默认开启的全屏 TUI，以及重新设计的 WebUI 时间线。这个版本也补齐了程序调用接口、会话沙盒、压缩后的文件回读、记账与赞助等能力，并修复了长会话、多用户、后台子代理与移动端的一批实际问题。

**直接运行 `miyu` 就是全屏 TUI，无需设置 `MIYU_TUI=1`。** `miyu dev` 进入开发模式；需要旧 inline 前端时可使用 `MIYU_TUI=0 miyu`。本页安装包已更新为 **0.6.0-2**，安装过首批 0.6.0-1 的用户请升级至此修订。

## 第一次见面：五屏 OOBE 与星空大厅

新安装会依次带你完成 **人格 → 功能 → 认识你 → 终端集成 → 接模型**。可以使用内置 Miyu，也可以起名字、写设定创建自己的角色；记忆、知识库、MCP、技能和每项内置脚本都能选择。终端页识别本机 shell，并标明已经集成的项目。

接模型支持本机已登录的 Claude Code / Codex / Antigravity，也提供 16 条国内外供应商预设，填入 key 后即可选择模型，列表支持 `/` 搜索。每屏确认就保存，中途退出可继续。老用户升级不会被引导拦住，随时运行 `miyu oobe` 可以重走一遍。

![首次见面](https://raw.githubusercontent.com/SHORiN-KiWATA/miyu-agent/v0.6.0/docs/releases/0.6.0/oobe/01-welcome.png)

<details>
<summary>展开查看人格、功能与模型设置</summary>

![选择人格](https://raw.githubusercontent.com/SHORiN-KiWATA/miyu-agent/v0.6.0/docs/releases/0.6.0/oobe/02-persona.png)
![选择功能](https://raw.githubusercontent.com/SHORiN-KiWATA/miyu-agent/v0.6.0/docs/releases/0.6.0/oobe/03-features.png)
![接入模型](https://raw.githubusercontent.com/SHORiN-KiWATA/miyu-agent/v0.6.0/docs/releases/0.6.0/oobe/04-provider.png)

</details>

空会话换上星空与渐变 MIYU 艺术字，输入框收在字下方。发出第一句话之前按 **Tab** 可切换普通 / 开发模式；`/new`、`/reset` 回到大厅并清理画布，`/session` 切回旧会话时回放最近的对话。可以用 `config/banner.txt` 换自己的看板，或用 `display.banner = false` 关闭。

## 默认全屏 TUI：过程可以回看、展开、追踪

思考、命令、编辑和回答串成一条时间线。运行中的步骤有转轮，命令下方显示六行实时输出，编辑可以查看 diff。过程结束后收成 `Worked for … · tools · thoughts` 一行，点开还能逐步查看参数、完整输出与修改。

![TUI 时间线，用户提供的实机截图](https://raw.githubusercontent.com/SHORiN-KiWATA/miyu-agent/v0.6.0/docs/releases/0.6.0/screenshots/tui-timeline.png)

- 正文从顶部开始，输入框固定在底部；可以滚动回看、拖选复制、点击链接。
- 子代理和后台子代理各有自己的时间线浮层，显示思考、工具、Markdown 正文、实时词元与耗时；运行期间也可以继续给后台子代理追加指令。
- shellhook 和单次 `miyu "…"` 使用同一套过程样式，命令输出、diff、错误、问答与后台跟进都按顺序展示。
- `/compact` 会显示压缩进度和摘要；Ctrl+C 中断能正确收尾，重开会话后仍能回看被中断的一轮。
- 修复多行粘贴丢换行、历史占位符丢内容、模型切换后 footer 不刷新、设置进出闪屏，以及终端鼠标转义码泄漏。
- 修复 Konsole 图片消失、kitty 环境变量污染其他终端、旧版 chafa 不兼容和小图被过度放大等问题。

## WebUI：时间线、预览与跨设备体验

![WebUI 登录界面](https://raw.githubusercontent.com/SHORiN-KiWATA/miyu-agent/v0.6.0/docs/releases/0.6.0/screenshots/webui-login.png)

AI 正文直接铺在页面上，工具过程用细线连接图标，失败步骤单独标红。思考收起时仍能看到尾部内容；连续工具调用结束后默认自动收起，耗时落库，刷新或换设备回看仍保留。回复运行中继续发送的消息会出现在对话末尾，并可在轮到之前撤下。

![WebUI 时间线，用户提供的实机截图](https://raw.githubusercontent.com/SHORiN-KiWATA/miyu-agent/v0.6.0/docs/releases/0.6.0/screenshots/webui-timeline.png)

- **可交互 artifact**：HTML 预览可以执行脚本、响应按钮和绘制图表；内置 ECharts 6.1.0、KaTeX 与 Prism，无需联网取库。预览继续隔离登录凭证、浏览器存储和外网访问。
- SVG 与 PNG / JPEG / WebP / GIF 可缩放、平移；CSV / TSV 显示为表格；源码预览有语法高亮。
- 用户附件点击即可预览文本、PDF、音视频与图片，下载有独立入口；正文中的裸网址可点击，独占一行的链接可生成带图卡片。
- 主题、配色、字号与过程展开偏好跨设备同步，宽屏设置页居中；刷新会回到原控制台面板。
- 手机回车改为换行，发送使用按钮或 Ctrl/Cmd+Enter；修复 Safari 软键盘黑屏、流式滚动跳动、任务条与回到底部按钮互相遮挡。
- 停止生成避免全量重绘长对话；已回答的问答卡刷新后保持原位置，删除最后一个会话也不会被多个页面重复新建。

## 可供其他程序调用的 AI 后端

- `miyu ask --output-format json|stream-json` 输出最终正文或逐行事件，含用量、模型和耗时；支持模型、上下文窗口、system prompt、记忆、工具、图片、工作目录与超时等覆盖选项。
- **`miyu stdio`** 提供常驻的逐行 JSON 协议：一个进程可并发管理多会话、多回合，支持宿主回答提问和中途取消。见 [CLI 后端协议](https://github.com/SHORiN-KiWATA/miyu-agent/blob/v0.6.0/docs/cli-backend.md)。
- `miyu session` 支持列出、新建、查看、删除、重命名、清空、回退、压缩、模型与沙盒管理，支持 `--json`。单回合正文上限提高到 20 万字符。
- QQ、终端和 WebUI 统一支持 **PDF**：模型支持时直接发送文件，不支持时提供路径供工具处理，单份上限 20 MB。供应商菜单也能手填未公开在目录中的模型名。

## 会话沙盒、开发模式与子代理

新增 **`/sandbox <路径>`**，将会话的命令、后台任务、脚本、文件读写和 CLI 供应商进程限制在指定目录及明确放行的路径内。`/sandbox` 查看范围，`/sandbox clear` 解绑；WebUI 和 `miyu session sandbox` 同样可用。绑定时会探测 Linux Landlock 支持。

开发模式减少无关的固定提示开销，原实测约 **3900 → 2950 token（−24%）**。开发任务可委派给 `dev=true` 子代理，使用相同的开发人格与精简工具面。后台执行现在保留工作区、沙盒和会话身份；成员使用 CLI 后端时，工具桥也能在自身权限范围内连接 daemon。

子代理看图改走独立视觉模型，修复过去只收到占位标记的问题；后台任务可实时展开查看，普通成员也能看到属于自己的任务状态。

## 压缩、缓存与记忆

- **压缩一个会话不再阻塞其他会话**。摘要超时按输出预算计算，避免慢模型必然超时；手动压缩实时显示摘要，不再长时间空白。
- 压缩后会从磁盘重新读取最近接触的文件回到上下文，默认最多 5 个、单文件 4000 token、总计 24000 token，小窗口自动缩减。Claude Code / Codex / Antigravity 自有工具接触的文件也会记录。
- 被折叠的原始对话与旧摘要归档在 `state/compact/<会话>/`，保留最近 5 份，模型可回读细节。摘要强化用户要求、错误纠正、当前工作与下一步。
- 上下文占用优先使用供应商真实用量；修复摘要缓存命中漏记，终端与 WebUI 增加输出速度，接近 100% 的缓存命中率保留必要精度。
- 记忆整理更关注人物偏好、环境配置、实测结果与约定，减少通用百科内容和重复记忆。
- `/reset-memory` 只清本会话记忆；新增 `/reset-all-memory` 清当前人格全部长期记忆。`/wipe` 保留磁盘技能，若要一并清理请明确使用 `miyu memory reset --include-skills`。

## 知识库、人格与扩展

- 知识库支持拖入多个文件或整个目录，逐文件反馈结果，目录可勾选后批量删除。
- 重建语义索引有百分比、当前文件和失败原因；重复请求会排队，失败日志可追查。修复成员重建误用管理员知识库、本地嵌入状态误报和普通文档被关键词误挡。
- MCP 与技能加入人格白名单，关闭的 MCP 不会启动；自定义人格自己的脚本、技能始终可用，修复注册成功却无法调用的问题。
- 纯中文人格名使用独立目录并兼容迁移，成员 artifact 落入自己的家目录；模型名中的下划线、链接、代码高亮与附件类型显示得到修正。
- 新增 Reddit 归档检索，支持按版块或用户找帖子、阅读评论树；网络检索回答附来源。

## 记账、赞助与 QQ

- **日常记账**：自然语言记录支出，WebUI 提供流水、账户、分类、预算和数据视图，支持多币种及账户转账等操作。
- **赞助记账**：记录赞助人、金额和备注，支持人民币、美元并固定入账时汇率；控制台提供总额、人数、排行榜与明细。增删改由管理员操作。
- QQ 新增跨午夜的睡眠时间，例如 `23:00-07:00`；管理员和符合条件的私聊白名单仍可唤醒。
- 修复提前解禁后长时间不回复的问题，并为禁言拦截补日志；撤回消息记录发送者、原文摘要与原因。
- 私聊工具间的文字及时发送，CLI 中转供应商也能边工作边反馈；生图后的重复文字会被拦下。

## 其他稳定性修复与升级提示

- 修复 OpenCode Go 缺少会话头导致的 400；Zen 请求补齐对应客户端识别头。多模型池各自轮询，辅助请求不再打偏主会话轮换。
- 缺失工具返回在发送前补齐，避免部分模型将会话永久拒收；空推理字段不再把流式正文拆成一行一段。
- 修改供应商 ID 时同步历史用量记录；成员可以独立设置模型思考程度。读取更新版本写入的配置时保留未知字段，不再拒绝启动。
- **命令变化**：`miyu normal` 退役，普通模式直接运行 `miyu`；`/workspace` 与 `miyu session workspace` 改为沙盒命令。旧工作目录不会在升级后被悄悄绑定成沙盒。
- 移除 deep_research、check_issue 与输入法诊断技能；Arch 工具合并进 archlinux 插件，保留工具名。上面的 TUI 截图展示时间线样式，其中 `check_issue` 是早期实机记录。
- 更新前的旧记忆没有会话标记，需用 `/reset-all-memory` 清理；已有 `MIYU_HOME` / `~/.miyu` 继续使用。

## 下载与安装

附件仅提供 **Linux x86_64** 的六个发行版安装包。主包自带字体、语义模型、表情、脚本、默认知识库和许可证；语音为可选包，需要与主包安装相同修订。

| 发行版 | 主包 | 可选语音包 |
| --- | --- | --- |
| Arch Linux | `miyu-0.6.0-2-x86_64.pkg.tar.zst` | `miyu-voice-0.6.0-2-x86_64.pkg.tar.zst` |
| Debian 13 / Ubuntu 25.10、26.04 | `miyu_0.6.0-2_amd64.deb` | `miyu-voice_0.6.0-2_amd64.deb` |
| Fedora 44 | `miyu-0.6.0-2.fc44.x86_64.rpm` | `miyu-voice-0.6.0-2.fc44.x86_64.rpm` |

```bash
# Arch Linux
sudo pacman -U ./miyu-0.6.0-2-x86_64.pkg.tar.zst

# Debian / Ubuntu
sudo apt install ./miyu_0.6.0-2_amd64.deb

# Fedora 44
sudo dnf install ./miyu-0.6.0-2.fc44.x86_64.rpm
```

[完整更新记录（根据 next-release-note 归档）](https://github.com/SHORiN-KiWATA/miyu-agent/blob/v0.6.0/docs/releases/0.6.0/changelog.md)
