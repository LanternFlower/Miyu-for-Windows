# 02 · Turn 每步落盘与崩溃续跑

## 1. 现状（已定位）

Miyu 已经有“半套”turn 持久化：

- `start_turn` 立即写 running turn 行。
- `TurnJournalSink` 把 stream 事件（content delta / tool_call / tool_result / image / question 等）写到 `turn_journal_events`；工具事件与 reasoning 事件会强制先 flush。
- `PendingTurnGuard` 在异常路径调用 `interrupt_turn`；`recover_stale_turns` 会把进程残留的 running turn 标记为 interrupted，并物化 journal 输出（有测试 `interrupted_turn_materializes_persisted_journal_output`）。

缺口：

1. **普通正文仍会丢尾部**：`JOURNAL_FLUSH_BYTES = 16 KiB` / 80ms 合并写。进程在 chunk 进入 pending 后、flush 前崩溃，这 16 KiB 内容丢失。
2. **没有“步骤检查点”**：`chat_with_tools` 的 `messages` 数组、`tool_round`、`question_rounds`、`loaded_tools`、usage 只在内存；崩溃后无法知道“哪些工具已经执行完、下一步该发什么”。
3. **不能从 daemon 崩溃续跑**：恢复路径只把 turn 标记 interrupted 并渲染已有 journal，不会自动继续 LLM 循环。
4. 子代理有进程内 checkpoint（`SubagentCheckpoint`），但同样不跨重启。

## 2. 目标拆解（待 D5 确认）

- **一期（推荐先做）**：每完成一步（模型轮结束 / 工具执行结束 / 用户追问结束）都产生可恢复的持久记录；崩溃后历史和输出零丢失。
- **二期（可选）**：daemon 启动时从检查点自动续跑。因工具副作用重放风险高，必须单独设计，不与一期混提交。

## 3. 一期方案：持久化 step checkpoint

### 3.1 数据模型

新表 `turn_checkpoints`（独立迁移）：

```sql
CREATE TABLE turn_checkpoints (
  turn_id        TEXT PRIMARY KEY,
  revision       INTEGER NOT NULL,
  step           INTEGER NOT NULL,          -- 已完成到第几步
  messages_json  TEXT NOT NULL,             -- 完整 ChatMessage 数组（serde_json）
  loaded_tools   TEXT NOT NULL,             -- BTreeSet 序列化
  tool_round     INTEGER NOT NULL,
  question_round INTEGER NOT NULL,
  usage_json     TEXT NOT NULL,
  updated_at     TEXT NOT NULL,
  FOREIGN KEY (turn_id) REFERENCES turns(turn_id) ON DELETE CASCADE
);
```

保留条件：
- 只在 tool round 边界 / question 边界 / supersede 边界写，不每个 provider token 写。
- 写 checkpoint 与 journal flush 用同一个 SQLite 事务或紧邻顺序：**先 journal，再 checkpoint，最后执行下一工具**。进程死在两者之间时，恢复端只相信 journal 中已出现的 tool_result。
- redo revision 按 `turn_id + revision` 隔离；redo 失败回滚时旧 revision checkpoint 不覆盖。

### 3.2 Agent 循环埋点

- `chat_with_tools` 每轮模型返回、执行完一批工具并 `push` 进 `messages` 后，调用 `save_turn_checkpoint(...)`。
- ask_question 收到回答并写 `question_exchange` 后同样落检查点。
- queued prompts supersede / 续轮前落检查点，保证恢复能看到已消费队列。
- `UsageAccumulator` 快照一并写入。

### 3.3 恢复语义（一期）

`recover_stale_turns()` 扩展：
1. running turn 存在 checkpoint → 用 checkpoint 的 `messages_json` 重放成 interrupted turn 的完整 assistant/tool 内容，而不是仅拼接 journal 文本；journal 仍是逐事件展示证据。
2. 没有 checkpoint（旧版本崩溃）→ 走现有 journal 物化路径。
3. UI 显示明确状态：“上次运行到第 N 步后中断”，而不是假装正常完成。
4. 不自动重跑工具。

### 3.4 Journal 收紧

- `JOURNAL_FLUSH_BYTES` 改为可配置，默认降到 `4 KiB`；80ms 定时保留给纯 reasoning。
- 新增 `AgentEvent::StepCompleted` 语义：每个 step 边界强制 `journal.flush`。
- 给 `PendingTurnGuard` 的 drop 路径加二次 flush 尝试（best-effort），避免 unwind 时 pending 丢失。

## 4. 二期方案（自动续跑，仅研究）

- 从 checkpoint 恢复 `messages` 后重新发起下一 LLM 请求，不再重放已完成的工具调用。
- 风险：崩溃点恰好在“工具已执行、结果未落 journal/checkpoint”之间，自动恢复可能重跑写工具（下载、告警、发消息等）。需要每个写工具具备幂等 key，或恢复策略默认 fail-closed。
- 建议先让一期跑一个发布周期，用真实崩溃数据统计“可安全续跑比例”，再决定是否做。

## 5. 修改文件清单（一期）

- `src/state/migrations/*`、`src/state/conversation_db/*`：新表 + 读写 + 恢复。
- `src/state/turns.rs`、`src/state/mod.rs`：`save_turn_checkpoint/load_turn_checkpoint`。
- `src/agent/turn_loop/mod.rs`、`parallel.rs`、`redo.rs`：边界落点。
- `src/agent/journal.rs`：强制 flush 与 buffer 策略。
- `src/web/turns/task.rs`、`src/web/event_map.rs`、`src/cli/repl/*`：中断态展示。
- `src/web/actor/job_wake.rs`：goal 续轮若涉及 checkpoint 同步处理。

## 6. 测试与验收

- 回归测试必须**可证伪**：临时移除 checkpoint 写入后，恢复测试要报红（参照 `AGENTS.md` 2.3）。
- 单元：每个边界（工具完成、ask_question 完成、supersede）产生 checkpoint；kill 在 checkpoint 前/后两种时序的恢复结果不同且可预期。
- 集成：`testkit/dev-smoke` 增加“提交长任务 → SIGKILL daemon → 重启 → 历史完整且标为 interrupted”。
- 门禁：`scripts/refactor-check.sh`；新增迁移 v11→latest、重复 open、rollback 测试。

## 7. 待确认

- D5：一期只做“不丢 + 可解释恢复”，还是同时做自动续跑（推荐一期）。
- 检查点频率：每工具步 vs 每模型轮（推荐每工具步，因为工具执行是昂贵副作用的分界）。
