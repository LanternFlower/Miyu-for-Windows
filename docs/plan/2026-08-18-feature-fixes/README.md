# Miyu 优化/修复专项方案（2026-08-18）

> 性质：**只做研究与计划，不写实现代码**。本目录是后续施工依据；每个文件独立覆盖一个主题，避免单文件膨胀。
> 代码基准：`main` @ `41b33ab`（本机 Miyu 仓库）。所有文件路径以拆分后的 `src/` 结构为准。
>
> 施工进度（2026-08-19）：#4（04 英文化）、#7（07 文件分享，改为独立 share_file 子系统与 artifact 解耦）、#11（08 天气）、#16（12 生图限流）已实现；新增 [13-scheduled-messages.md](13-scheduled-messages.md)（QQ 定时消息插件）已实现。决策落定：D7=人格保持中文、D8=沿用 WebUI 登录态、D9=直接报错、D12=每条用户消息（含 queued follow-up）各一张。

## 一、问题清单与定位结论

| # | 需求 | 根因（当前结论） | 方案 |
|---|---|---|---|
| 1 | 专门 DSH 模式 | Miyu 没有 DSH 模式；工具目录/提示词由 `AgentMode` 和 `build_tool_registry` 决定，没有“第一步极简、之后开放”的阶段概念 | [01-dsh-mode.md](01-dsh-mode.md) |
| 2 | turn 每完成一步就记录 | 已有 journal 落盘，但 Content/ToolCall 增量有 16KiB/80ms 缓冲；回合消息数组与执行进度没有“每步持久化检查点”，daemon 崩溃后只能显示已刷出内容，不能从断点继续 | [02-turn-step-durability.md](02-turn-step-durability.md) |
| 3 | 子代理继承主上下文 | `SubagentRunner::run_with_resume` 新建会话时只给 `[system, user(prompt)]`，TaskContext 也只携带 config/paths/tools；主对话消息没有进入工具执行上下文 | [03-subagent-context.md](03-subagent-context.md) |
| 4 | 工具描述与提示语句全英文 | 61/61 个 `descriptions/*.json` 含中文；大量 agent 侧注入、错误回灌、追问/终局提示用 `agent_text()` 按系统 locale 切中文 | [04-english-agent-surface.md](04-english-agent-surface.md) |
| 5 | token/缓存统计是否正确 | 结构基本正确（provider usage 归一化 + 会话累计 + subagent 归账都在），但存在已知口径差异与几处可疑点，需要审计并补测试 | [05-token-cache-audit.md](05-token-cache-audit.md) |
| 6 | REPL Ctrl+左/右按词跳光标 | `LiveReplEditor::handle_event` 只匹配 `KeyCode::Left/Right`，未检查 `CONTROL`；`remove_word_before_cursor` 已有“词”语义但只用于 Ctrl+W | [06-repl-input-history-sessions-footer.md](06-repl-input-history-sessions-footer.md) |
| 7 | WebUI 附件局域网可下载 | `present_artifact/create_artifact` 已经会快照文件到 SQLite 并经 `/api/artifacts/{id}` 鉴权下载；缺的是“可分享链接/下载入口/权限语义”，且 `/api/media` 只允许媒体扩展名 | [07-webui-file-sharing.md](07-webui-file-sharing.md) |
| 8 | 普通 REPL 不应进入终端集成会话 | `GetReplSession` 和直连 REPL 在 `repl_session` 指针为空时 fallback 到 `store.session_id()`（终端会话）；dev 已自举，normal 没有 | [06-repl-input-history-sessions-footer.md](06-repl-input-history-sessions-footer.md) |
| 9 | 重进 REPL footer 上下文为 0 | `cold_context()` 硬编码 `tokens: 0`；daemon 启动后 `manager.context` 一直是 0，`session_state_for` 对“当前会话”直接读这份快照，直到第一次对话才更新 | [06-repl-input-history-sessions-footer.md](06-repl-input-history-sessions-footer.md) |
| 10 | 第二个 REPL 上键调不出刚输入的历史 | 沙箱实测（daemon + mock LLM）**未复现**：`persist_repl_history_entry` 在提交前写入，第二个 REPL 能读到。疑与用户具体启动方式/会话指针/MIYU_HOME 差异有关，需复现信息；方案改为 daemon 侧实时历史账本 | [06-repl-input-history-sessions-footer.md](06-repl-input-history-sessions-footer.md) |
| 11 | 天气不应自动定位 | `get_weather` 对 `location="" && forecast` 显式走 `wttr.in auto_location`；工具描述也明确承诺“空字符串自动定位” | [08-weather-no-autolocate.md](08-weather-no-autolocate.md) |
| 12 | 接入 Claude Code 订阅 | **已完成（08-20，claude-code 分支）**：范围经用户扩大为三件套——`claude-code` 供应商协议（订阅中转）+ `miyu mcp-serve` 工具桥 + `claude_code` 委托工具，见 [09-claude-code.md](09-claude-code.md) 施工记录 | [09-claude-code.md](09-claude-code.md) |
| 13 | undo 无法使用 | 核心 `undo_last_turn` 有完整测试且逻辑通过；远端 `/undo` 链路也能定位。需要用户提供具体失败形态（busy / 返回 0 / 无视觉变化）。已发现相邻隐患：undo 只删 turn 行，`manager.context` 刷新依赖“目标会话=当前会话”分支，UI 不回放删除结果 | [10-undo-background-jobs.md](10-undo-background-jobs.md) |
| 14 | 后台命令是否按会话区分 | 命令和子代理共用 `JobEntry.session_id` + `job_visible()`；远端 REPL 的 strip 也按 `repl_session` 过滤。主要缺口在**直连模式 `JobsFeed::Local` 不过滤**，以及 ledger 恢复后无 session 的旧任务全局可见 | [10-undo-background-jobs.md](10-undo-background-jobs.md) |
| 15 | ask_question 兼容性 | `ask_question` 是唯一绕过 registry 的特判路径；`always_loaded=false`。默认 stub 模式下模型只能看到 60 字符摘要 + `{"type":"object"}`，拿不到真实 schema，导致参数形状漂移 | [11-ask-question.md](11-ask-question.md) |
| 16 | 平台生图每会话每请求限一张 | `generate_image` 每次调用生成一张，但同一 turn 内可反复调用；无按 turn 计数，也无“管理员/私聊白名单豁免”开关 | [12-image-generation-limit.md](12-image-generation-limit.md) |

## 二、待用户拍板的决策

| # | 问题 | 推荐 |
|---|---|---|
| D1 | DSH 第一步的工具是否要字节级复刻 `bash + str_replace_editor`，还是用 Miyu 现有 `run_command + apply_patch` 近似？ | 字节级复刻两个新工具；Anchored Standard 的实验证明 tool schema 是锚定主因 |
| D2 | DSH 的“晋升”信号：第一次助手输出即晋升，还是必须产生 tool call？ | `promoteOn: either`（第一次 assistant message 或 tool call 即晋升，避免无工具回答时永久卡极简） |
| D3 | DSH 是否独立 persona/记忆/session（像 dev）？ | 是，新增 `dsh` 保留人格，默认提示词固定一句英文 |
| D4 | DSH 是否硬限制只能选 DeepSeek 模型？ | 不硬限制；模式可用任意模型，但首次进入给出“为 DeepSeek 设计”的提示 |
| D5 | turn 每步记录的目标：只保证“崩溃后历史/输出不丢”，还是“daemon 重启后自动从中断处续跑”？ | 分两期：一期做持久化检查点 + 精确恢复展示；自动续跑二期再做（副作用重放需要专门设计） |
| D6 | 子代理继承上下文：全量还是限额？是否始终开启？ | 默认开启，携带当前 turn 组装后的可见消息，按配置 token/字节上限截断，子代理 prompt 可要求 `context: none` 关闭 |
| D7 | 英文化范围：默认人格 `miyu.md`、默认 dev 提示词、用户自建 persona 是否保持中文？ | 工程提示与工具契约全英；**用户自建 persona 原样保留**；默认 Miyu 人格是否英文化请用户定（推荐保持，它属于产品人格而非工程提示） |
| D8 | WebUI 文件分享：仅登录用户可下载，还是要生成免登录临时链接？ | 一期仅登录用户 + 一键复制完整下载 URL；免登录临时 token 二期 |
| D9 | 天气：直接运行（CLI/工具空 location）是返回错误让 AI 追问，还是工具内尝试读取会话最近地点？ | 直接返回明确错误，让 AI 追问用户；不做任何自动定位 |
| D10 | Claude Code 接入形态：新增 `claude_code` 工具 / 作为 `task` 的 tier / 替代 run_command？ | ~~新增独立工具~~ **08-20 用户改判：全都要**——供应商协议（中转层）+ MCP 工具桥 + 独立工具三件套并行，工具部分维持原设计，仅本地 owner 会话注册 |
| D11 | undo 请补充复现：报错文案、操作路径（`miyu normal` 还是直连）、会话状态 | 先补 2 个回归测试 + UI 回放修复，再按复现收口 |
| D12 | 平台生图限制中，“每次请求”=一个用户 turn；turn 内 queued follow-up 是否重置计数？ | 每个 user 输入/queued prompt 重置；同一 turn 内不同 follow-up 各允许一张 |
| D13 | 普通 REPL 首启自举新会话后，旧行为下用户已有的“终端会话即 REPL 会话”数据怎么处理？ | 不迁移、不删除；新建独立会话，旧数据仍在终端集成会话，用户可用 `/session` 切回 |

## 三、实施约束

- 遵守 `AGENTS.md`：消息组装逐字节确定、append-only、权限由代码承担、不洗取证链。
- 任何涉及 `agent/`、`llm/`、`tools/registry`、提示词的改动，验收时必须跑 `bash scripts/refactor-check.sh`，并手测两轮 `cache-usage.*.jsonl`。
- 每项独立提交；功能完成后由用户验证再 commit。
