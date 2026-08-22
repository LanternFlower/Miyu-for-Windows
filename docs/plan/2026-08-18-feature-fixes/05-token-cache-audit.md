# 05 · Token 与缓存统计审计

## 1. 结论先行

当前实现**基本正确，但缺少端到端证明**。没有发现“缓存命中显示恒 0”或“total 明显错加”这类硬伤；发现 5 处口径问题，其中 1 处已经直接造成用户可见 bug（footer 首次为 0，见 06）。建议先补审计测试，再按测试结果做小修。

## 2. 已验证正确的部分

| 能力 | 位置 |
|---|---|
| DeepSeek 顶层 `prompt_cache_hit_tokens/prompt_cache_miss_tokens` | `llm/mod.rs::Usage` + `normalize_cache_fields` |
| OpenAI `prompt_tokens_details.cached_tokens/cache_write_tokens` | 同上 |
| Anthropic cache read/write 归一化 | `openai_compatible/sse/anthropic.rs` |
| Responses `input_details.cached_tokens` | `openai_compatible/sse/responses.rs` |
| turn 内多轮模型请求累计 | `agent/mod.rs::UsageAccumulator`，逐轮 `add_result` |
| 会话累计 = turns 总和 + 子代理 audit 会话 | `state/conversation_db/sessions.rs::session_token_totals` |
| cache-usage JSONL（不记正文、0600、轮转） | `llm/cache_log.rs` |
| 估算值带 `≈`，不会伪装成精确值 | `tools/subagent_runner.rs::format_token_count`、`TokenEstimateMethod` |

## 3. 发现的问题（按优先级）

### P1：`cold_context` 把当前上下文硬编码为 0

`runtime/state.rs::cold_context`：

```rust
ContextSnapshot { tokens: 0, window: ..., cumulative_tokens: ... }
```

daemon 冷启动后 `session_state_for` 对当前会话直接返回 `manager.context`，于是 footer 首次进入显示 `0/168k`，对话一次后才恢复。修复归入 06；它是 token 统计问题中最可见的一个。

### P2：`normalize_cache_fields` 把“不可能值”静默夹到 `prompt_tokens`

`llm/mod.rs` 中 `cache_read_tokens > prompt_tokens` 时直接 `cache_read = prompt`。这是数据损坏掩盖，不是修因。按 `docs/cache-and-prompt-plan.md` 的约定应标记 malformed 并保留观测，再由使用侧决定展示。当前行为会让适配器字段映射错误永远不可见。

建议：增加 `usage_malformed: bool` 或日志字段；UI 对 malformed 显示 `C?`；cache log 保留原始 `reported` 与 clamped 前后值。

### P3：turn 级估算一旦混入，整 turn 都标 estimated

`UsageAccumulator.estimated` 是单调 OR。某轮 provider 没给 usage、后续轮给了 usage，最终 `result.usage_estimated=true`，UI 会把含真实 provider 数据的总和当成“估算”。建议增加 `mixed` 第三态，或至少持久化 `provider_usage_rounds / estimated_rounds`。

### P4：上下文水位用本地 BPE 估算，且 `last_request_usage` 有注释无消费者

- `Agent::effective_context_tokens` 用本地 o200k BPE 估算 `messages + tools`，不包含 provider chat-template/消息包装开销，通常低几个百分点。
- `ChatResult.last_request_usage` 注释写明“used for the context meter”，但全仓库没有消费点（`rg last_request_usage` 只有构造）。
- 二选一：恢复用最终请求 `prompt_tokens` 校准水位，或删字段/改注释。若恢复，必须保留“无 provider usage 时用本地估算”的 fallback。

### P5：摘要 turn 的 `token_total` 口径需要明示

`insert_summary_turn` 写入的是生成摘要那一次 API 的用量，不是摘要体积。它计入了会话 Σ（合理，因为确实花了钱），但任何把它当作“上下文占用”的代码都错。审计要求：所有消费 `token_total` 的地方标注是“费用口径”，上下文占用只允许 `effective_context_tokens` 或最终请求 usage。

## 4. 审计与回归网

新增 `src/llm/tests/usage_fixtures.rs`（或并入现有 tests）：

- 四协议 usage fixture：DeepSeek hit/miss、OpenAI details、Anthropic cache blocks、Responses input details。
- 归一化不变量：`cache_read <= prompt_tokens`、`cache_write <= prompt_tokens`、`effective_total = prompt + completion`（provider total 缺省时）。
- `UsageAccumulator`：3 轮（有 usage / 无 usage / 有 usage）的累计值、estimated 状态（按 P3 新语义）。
- `session_token_totals`：普通 turns + 子代理 sessions + compaction summary 的期望值。
- cache-usage JSONL：reported=false 时不写 0 命中为“支持缓存”。
- 对每个新增断言执行 `AGENTS.md` 2.3 证伪：暂时去掉修复必须报红。

## 5. 修改文件清单（预计）

- `src/runtime/state.rs`：cold context（与 06 合并改）。
- `src/llm/mod.rs`：malformed 标记、TurnTokens 口径注释。
- `src/agent/mod.rs`：UsageAccumulator 三态。
- `src/agent/setup.rs` / 或 footer 消费处：`last_request_usage` 校准或删注释。
- `src/state/conversation_db/*`：需要时为 summary usage 增加来源列。
- `src/llm/tests/*`：fixture 回归网。

## 6. 验收

1. 四协议 fixture 全部通过；手工破坏任一映射字段，至少一个测试报红。
2. 冷启动 footer 不再是 0（回归测试）。
3. `cache-usage.*.jsonl` 两轮请求的 `cache_read` 符合 provider 原始报告。
4. `bash scripts/refactor-check.sh` 全绿。
