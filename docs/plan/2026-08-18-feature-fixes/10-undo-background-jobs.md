# 10 · Undo 修复与后台任务会话隔离审计

## A. Undo

### 现状（已定位）

- 核心逻辑完整：`state/conversation_db/history.rs::undo_last_turn` 覆盖普通 turn、可逆 compaction、嵌套 compaction、running turn 拒绝；`src/state/tests/turns.rs` / `redo.rs` / `compact.rs` 有测试。
- 远端链路完整：`/undo` → `IpcCommand::Undo` → `reserve_admin_for_session` → `ActorCommand::Undo` → `undo_last_turn`。
- **未定位到必然失败的代码路径**。需要用户提供具体复现（D11）。

已发现的可改进点：
1. 回合运行中按 `/undo` 只会得到通用 `ADMIN_BUSY_MESSAGE`，用户会误以为功能坏。应返回 `"undo is unavailable while this session's turn is running"`。
2. 远端 `/undo` 成功后只 `repl_note("已撤销消息数")`，**屏幕上方的旧回复不会消失**，直到重进 REPL 或下一条消息。这是“看起来没生效”的高概率根因。
3. 直连模式同样只 `println!`，不重绘活动区。
4. `manager.context` 只在目标会话等于 daemon 当前会话时刷新；其它会话的 footer 依赖 `session_state_for` 的旧快照（与 06 的 memo 修复合并处理）。

### 方案

1. 远端 `/undo` 成功后：
   - 清空当前 tail 并重新渲染 `session_replay_frame`（用更新后的 store；`config.display.repl_replay_turns` 控制深度）。
   - 再 `repl_note` 显示撤销结果；被撤销 prompt 回填 editor（现有行为保留）。
2. 直连模式：同样清屏/重绘 live tail（若无 live tail 则保持 println）。
3. `undo_last_turn` 增加更细粒度错误类型（`Running` / `NothingToUndo`），IPC 给出可读英文/中文消息。
4. 补充集成测试：
   - 有 running turn 时 `/undo` 返回明确 busy，不改变数据。
   - 远端 undo 后 `session_replay_frame` 不再包含被删 turn。
5. 若用户复现后发现更深问题，按复现追加根因，不掩盖。

## B. 后台命令/子代理会话隔离

### 现状

- `JobEntry.session_id` 对命令与子代理使用同一字段；`spawn_background` / `spawn_background_subagent` 都在 turn scope 内捕获 `workspace::try_session()`。
- `job_visible()` 限制 `job(action=status/stop)` 只看到本会话，`all=true` 逃生。
- 远端 REPL 的 strip 在 `retain_session_jobs` 中按 `repl_session` 过滤；wake run 也带 `session_id`。
- 已知缺口：
  1. **直连 REPL `JobsFeed::Local` 直接 `jobs::overview()`，不按会话过滤**（虽然直连受 core lease 限制，通常只有一个前端）。
  2. `job_visible` 对 `session_id = None` 的任务全局可见；legacy/异常路径会被所有会话看到。
  3. 台账只持久化进程/pid，不持久化 session；daemon 重启后命令进程被清理，问题不大，但语义上应明确“跨重启不复活”。

### 方案

1. `JobsFeed::Local` 增加 `session: Option<String>`，初始化时传入当前直连会话，使用同一 `retain_session_jobs`。
2. `job_visible` 改为 fail-closed：`current` 存在但 job 无 session 时不可见（除非 `all=true`）；测试/工具桥路径显式给 session 或用 `all=true`。
3. 台账加 `session_id` 字段（向后兼容缺省 None），仅用于诊断展示，不用于复活。
4. 增加测试：两个 session 各 spawn 命令，`job_status` 默认互不可见，`all=true` 可见；strip 只显示本会话。

## 修改文件清单

- `src/state/conversation_db/history.rs`：undo 错误类型。
- `src/web/ipc_server.rs`、`src/web/actor/mod.rs`：undo busy 文案与 context 刷新。
- `src/cli/repl/remote/interactive.rs`、`src/cli/repl/direct.rs`：undo 后重绘。
- `src/cli/repl/jobs.rs`：Local feed session 过滤。
- `src/tools/jobs/mod.rs`、`ledger.rs`：可见性 fail-closed、台账字段。
- 测试：`src/web/tests/ipc_bridge.rs`、`src/state/tests/*`、`src/tools/jobs/tests/*`。

## 验收

1. 空闲会话 `/undo` 后，屏幕上被撤销回复立即消失，footer 与 replay 一致。
2. 运行中 `/undo` 得到明确“turn 正在运行”，功能不被误判。
3. 两个会话的后台命令在 tool 和 REPL strip 中互不可见；`all=true` 仍可达。
4. 直连模式 strip 不再显示其它会话任务。
5. `bash scripts/refactor-check.sh` 全绿。
