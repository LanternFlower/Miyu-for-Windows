# 01 · DSH 模式（DeepSeek Harness 锚定模式）

## 1. 目标语义（用户已澄清）

- 新增加一个专门的 `dsh` 模式。
- **第一轮的第一步**（第一次发给模型请求）使用 DeepSeek Harness **Minimal 预设的一比一复刻**，让 DeepSeek 进入其训练分布锚定的优质思维链。
- **从第二步开始**开放全部工具；本 turn 后续工具轮、以及之后所有 turn 均不再回到极简目录（compaction 是否回退见决策 D14）。

## 2. 参考实现（本机可审计）

| 材料 | 位置 |
|---|---|
| DSH 官方 Minimal 预设 | `/home/shorin/Downloads/deepseek-harness/apps/cli/config/agent-presets/minimal/agent.cordis.yml` |
| Anchored Standard 插件（用户指定参考） | `/home/shorin/.dsh/.agent-presets/anchored-standard/`，核心：`agent.cordis.yml`、`tool-bootstrap.mjs`、`compaction-epoch.mjs`、`instruction-hint.mjs` |
| DSH `str_replace_editor` 完整 schema | `packages/fs/tool-str-replace-editor/src/index.ts`（参数、描述、view/create/str_replace/insert 语义） |
| DSH persistent `bash` schema | `packages/shell/tool-bash-persistent/src/index.ts` |

### Anchored Standard 的可移植结论

1. **第一步工具 schema 决定锚定**：官方 Minimal 的 `bash + str_replace_editor` 在 256k maxTokens 下 5/5 锚定；standard 家族 schema 11/11 落回非锚定行为。因此第一步不能拿 `run_command + apply_patch` 凑合（决策 D1 推荐字节级复刻）。
2. **第一步 system prompt 必须只有一个短句**：`You are a helpful software engineer assistant.`，`complete: true`，不注入 runtime context、AGENTS.md 摘要、skill catalog、host env。
3. **晋升信号**：Anchored Standard 默认 `promoteOn: either`（第一个 `assistant/message` 或 `tool/call`），请求 #2 起进入“受控目录”；并进一步证明晋升后**不要**立刻倾倒 20+ 工具全目录，否则轨迹被拉回 standard-like。
4. **compaction 是“第二个第一步”**：Anchored Standard 在 `compaction/end` 后回退到受控目录，直到新晋升信号。

## 3. 与 Miyu 现有机制的映射

| DSH 概念 | Miyu 落点 |
|---|---|
| agent preset / composition | `AgentMode::Dsh` + `dsh` 保留 persona（类似 `DEV_PERSONA = "dev"`） |
| Minimal persona | 新增 `dsh_system_prompt(paths)`，默认常量 `You are a helpful software engineer assistant.`，可在 `config/dsh-prompt.md` 覆盖 |
| bootstrap 工具目录 | 新 `dsh_bootstrap_registry`：`bash` + `str_replace_editor` 两个新工具（schema/description 按 DSH 源码复刻） |
| promotion 状态 | 新增会话级持久化阶段标记（推荐 `sessions` 或新 `session_phases` 表；不能只放内存，重启会回到第一步） |
| post-promotion 目录 | `dsh_registry`：DSH 模式全量工具（复用 Normal 目录的 coding 子集还是全部工具，见 D15） |
| compaction epoch | `AgentMode::Dsh` 下 compaction 后把 phase 拨回 bootstrap；有 durable 晋升信号后恢复 |

## 4. 关键设计

### 4.1 第一步的字节契约

第一步请求必须与 DSH Minimal 语义等价：

- system：`You are a helpful software engineer assistant.`
- tools：只含 `bash`、`str_replace_editor`，按名排序。
- 不注入：host environment、memory preamble、runtime/turn system context、persona reminder、preset dialogs、mode reminder、`<runtime>` 尾巴也不应破坏锚定（建议 DSH 模式第一步连 runtime tail 都禁止，全部动态信息从第二步开始追加）。
- `bash` 工具执行实现可先退化为一次性 `bash -lc`（Miyu 没有 PTY 持久 shell 基建），但**模型可见 schema 与 description 必须一致**；持久 shell 是后续增强，不属于第一步锚定条件。
- `str_replace_editor` 实现放新文件 `src/tools/dsh/`，只依赖工作区路径解析，不依赖其它工具。

### 4.2 两阶段工具目录

`chat_with_tools` 当前每轮从 `self.tools` 计算 definitions。DSH 最小改动：

- `Agent` 增加 `tool_phase: ToolPhase`（Bootstrap/Full）与 `dsh_bootstrap_tools: ToolRegistry`、`dsh_full_tools: ToolRegistry`。
- 循环开始时按 phase 选择 definitions。
- 每轮 assistant 结果落地后（`push_assistant_message_with_reasoning` 前）调用 `promote_dsh_phase`；晋升前先把 phase 标记**持久化**，再改内存。
- `tool_round` 从第二步起全程 full；后续 turn 构造 Agent 时从会话状态恢复 phase。

### 4.3 会话/人格路由

- 新 `DSH_PERSONA = "dsh"`，`AgentMode::Dsh`。
- `turn_mode_for_session`、`CreateSession`、`GetReplSession`、`ListSessions`、`parse_mode`、`mode_name`、CLI 参数 `miyu dsh` 全部增加 Dsh 分支。
- 记忆、技能、会话列表与 dev 一样走独立保留人格，互不可见。
- REPL/WebUI 入口：CLI `miyu dsh`；WebUI 会话 mode 增加 `"dsh"`（D4：不强制 DeepSeek 模型）。

### 4.4 phase 持久化

推荐：

```
ALTER TABLE sessions ADD COLUMN dsh_phase INTEGER NOT NULL DEFAULT 0;
-- 0 = bootstrap（尚未晋升）, 1 = full
```

写点收敛在 `StateStore::promote_dsh_session()`，与 journal 落盘同顺序：**先落库再执行后续工具**。cold start 读取一次；redo/queued follow-up 沿用当前 phase；compaction 后按 D14 回退。

## 5. 修改文件清单（预计）

- `src/agent/control.rs`：`AgentMode::Dsh`、label/reminder、`AgentTurnControl` 携带 dsh registry。
- `src/agent/setup.rs` / `src/agent/prompt.rs`：dsh prompt 源、跳过 host/memory/dialogs 的条件。
- `src/agent/turn_loop/mod.rs`：phase 选择 definitions 与晋升点。
- `src/tools/mod.rs`：`build_tool_registry` 增加 Dsh 分支；`dsh_bootstrap_registry` / `dsh_registry`。
- 新 `src/tools/dsh/{mod.rs,bash.rs,str_replace_editor.rs}`：两个 DSH 契约工具（文件 <800 行）。
- `src/state/*`：`DSH_PERSONA`、dsh phase 读写与迁移。
- `src/web/dto.rs`、`src/web/sessions.rs`、`src/web/session_cmds.rs`、`src/web/turns/task.rs`、`src/web/ipc_server.rs`：mode 路由。
- `src/cli/args.rs`、`src/cli/mod.rs`、`src/cli/repl/*`：`miyu dsh` 入口与显示。
- `src/config/persona_paths.rs`：`dsh_scoped` / `dsh_system_prompt`。

## 6. 缓存与字节稳定性

- 第一步 tools 数组只有两个工具；第二步切全量是**计划内的一次 schema 变更**，之后会话内保持稳定。
- 晋升状态必须持久且读一次冻结，不允许每 turn 重扫历史导致 phase 抖动。
- `bash`/`str_replace_editor` 的 description 使用常量，禁止拼接时间戳/路径。
- 测试：同 session 构造两次第一步请求，字节一致；第二步请求是第一步前缀延伸（工具数组变化导致 tools 区不延伸，但 message 区仍应延伸；mock byte-prefix 测试按 cache plan 口径调整断言）。

## 7. 验收

1. 新 session 第一步请求 fixture：system 精确一句、tools 仅两件、无注入块。
2. 第一步返回任意 assistant 消息后，第二步请求 tools 为 full；kill -9 后重启，phase 仍为 full。
3. `miyu dsh` / WebUI dsh 会话列表 / tool-call 目录 mode 返回正确。
4. compaction 行为按 D14 决策有明确测试。
5. `bash scripts/refactor-check.sh` 全绿；两轮 cache log 无明显异常 miss。

## 8. 待确认决策

- D1：bootstrap 工具是否新增字节级 `bash + str_replace_editor`（推荐是）。
- D2：`promoteOn: either`（推荐）。
- D3：dsh 独立 persona/记忆（推荐是）。
- D4：是否限制只允许 DeepSeek 模型（推荐不限制）。
- D14：compaction 后是否回退 bootstrap 再晋升（Anchored Standard 行为；推荐采纳，但会增加状态机）。
- D15：第二步“全部工具”是 Normal 全量还是 coding 子集（推荐先复用 Normal 全量以吻合“开放全部工具”的字面需求，跑一轮 eval 后再考虑 Anchored Standard 式按需解锁）。
