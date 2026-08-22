# 03 · 子代理继承主智能体上下文

## 1. 根因（已定位）

`SubagentRunner::run_with_resume` 新建会话时：

```rust
None => (
    vec![
        ChatMessage::system(self.system_prompt.clone()),
        ChatMessage::plain("user", prompt.to_string()),
    ],
    0,
)
```

主对话消息从未进入 `SubagentRunner`。`TaskContext` 只保存 `config/paths/tools`，工具执行时也没有渠道拿到主 Agent 正在使用的 `messages`。`task` 工具的 schema 甚至写明 “subagent has no access to the main agent's conversation history”，导致主模型被迫把背景重写一遍。

## 2. 方案

### 2.1 增加 turn 级上下文 task-local

在 `src/tools/workspace.rs` 增加：

- `tokio::task_local! MAIN_CONTEXT: Option<Arc<Vec<ChatMessage>>>`
- `with_main_context(context, future)` / `try_main_context()`。

设置点：
- `src/agent/turn_loop/stream.rs` 组装完 `messages`、并在 `chat_with_tools` 前，把 `Arc<Vec<ChatMessage>>` 放入 task-local。
- `src/agent/turn_loop/redo.rs` 同样处理。
- 注意：task-local 在 `tokio::spawn` 后丢失。**后台子代理**必须在 `run_task` 内、`spawn_background_task` 之前把 `try_main_context()` 捕获进 `TaskParams`/`TaskContext`（或一次性转成 owned snapshot）。

### 2.2 SubagentRunner 组装顺序

推荐顺序（保持子代理自身 system 独立）：

```
[ system = subagent-general ]
[ main conversation snapshot（去掉顶层 system 或按配置保留） ]
[ user = task prompt ]
[ user = "The context above is inherited from the main conversation..." 短说明（可选，恒定文本） ]
```

规则：
- 顶层主 system prompt 默认**不**复制；子代理保持自己的 `subagent-general.md`。D6 若要求人格一致，可作为配置项 `inherit_main_system = false/true`。
- 主上下文 snapshot 必须是**当前请求实际发送前**的 `messages`（不含当前轮未执行工具结果，不含 persona_reminder 浮动块），这样字节确定且不泄漏未完成状态。
- 上下文上限：新增配置 `tools.subagent_context_max_chars`（默认如 80_000 字符），按 turn 从最新往前保留完整 turn；超限在 turn 边界截断，不在消息中段切。
- 如果主上下文为空（例如直接 CLI tool-call 桥），退化为现有行为，不报错。

### 2.3 后台任务

`spawn_background_task` 已经特意在 turn scope 内解析 `AuditAnchor`；上下文 snapshot 沿用同一原则：

- 在 `run_task`（仍处于 task-local scope 的同步段）读取并 clone `Arc`。
- 把 snapshot 放进 `TaskParams` 传给 background future。
- job log 不记录上下文正文；审计只记录 `context_chars/context_tokens`。

### 2.4 工具契约与审计

- `task` 描述改为英文（顺带 04 项），删除 “no access to history” 措辞，说明默认继承、如何通过 `context: "none"` 关闭。
- `record_subagent_audit` 的 turn 内容仍只存 `prompt + result`，上下文不落库；在 `stats` 中新增 `context_tokens` 字段用于显示真实成本。

## 3. 范围

- 一期只改 `task` 子代理。
- `deep_research` 子代理保持隔离（其协议依赖严格 prompt 设计）；是否继承由 D16 决定，默认不改。

## 4. 修改文件清单

- `src/tools/workspace.rs`：task-local 与访问器。
- `src/agent/turn_loop/stream.rs`、`src/agent/turn_loop/redo.rs`：设置上下文。
- `src/tools/subagent_runner.rs`：`run_with_resume` 接受 `inherited_context: Option<Arc<Vec<ChatMessage>>>` 并组装。
- `src/tools/task.rs`：参数扩展、snapshot 捕获、描述更新、审计字段。
- `src/config/mod.rs` / `src/config/tests`：上限配置。
- `src/prompts/subagent-general.md`：英文化与继承说明。

## 5. 缓存与成本

- 子代理是独立辅助请求，本来不共享主请求前缀；加入继承会提高子代理成本，因此**必须**有字符上限和 `context:none` 逃生门。
- snapshot 在 turn 内只 clone 一次；不要在每次 tool call 重建。
- 测试断言：同一主上下文的两次子代理请求，组装字节一致。

## 6. 验收

1. mock 主 Agent 历史 → 调 `task` → 断言子代理首条请求包含主历史，且 system 仍为 subagent-general。
2. 超过上限时按 turn 截断，不产生半条消息。
3. 后台 `background=true` 的 task 在脱离 turn scope 后仍携带 snapshot。
4. `context:none` 回到现有双消息行为。
5. 审计 JSON 含 `context_tokens`，不把主上下文正文写进审计 turn。
