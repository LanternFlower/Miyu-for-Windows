# 06 · REPL 输入编辑、会话三车道、footer 与输入历史

## 1. Ctrl+左/右按词跳光标

### 根因

`src/cli/repl/editor.rs::LiveReplEditor::handle_event` 的 `KeyCode::Left/Right` 分支不检查 `KeyModifiers::CONTROL`，所以 Ctrl+Left/Right 与普通 Left/Right 一样只移动一个字符。仓库里已有向后删词实现 `remove_word_before_cursor`（`src/cli/repl/layout.rs`），但没有向前/向后**移动**函数。

### 方案

1. 在 `src/cli/repl/layout.rs` 新增：
   - `word_start_before_cursor(value, cursor) -> usize`
   - `word_end_after_cursor(value, cursor) -> usize`
   - 语义与 `remove_word_before_cursor` 一致：先跳过当前侧的空白，再跨过连续非空白。后续如需要再扩展标点类词边界，第一版保持“空白分词”。
2. `handle_event` 在 `KeyCode::Left/Right` 之前增加：
   - `KeyCode::Left if modifiers.contains(CONTROL)`
   - `KeyCode::Right if modifiers.contains(CONTROL)`
3. 占位符 `[Image N: ...]` 保持原子：Ctrl 跳词不得落入占位符中间；命中占位符时整块跳过。
4. 单元测试加入 `src/cli/tests/input_editing.rs`：中英文混合、连续空格、行首行尾、占位符边界。

## 2. 终端集成 / Normal / Dev 三会话并行

### 根因

两条入口在“REPL 指针为空”时都 fallback 到终端会话：

- 远端：`src/web/ipc_server.rs` `GetReplSession`，`None if dev => 自举 dev`，否则 `store.session_id()`。
- 直连：`src/cli/repl/direct.rs` `run_direct_repl`，normal 无指针时 `set_repl_session(&persona, &state.session_id())`。

dev 已经自举独立会话；normal 没有，于是第一次 `miyu normal` 会污染终端集成会话。

### 方案

- 两个入口统一调用一个新 helper：`ensure_repl_session(store, persona, is_dev)`。
  - 有有效指针 → 使用。
  - 指针空且 dev → 现有自举逻辑不变。
  - 指针空且 normal → **新建一个 `kind='user'` 的普通本地会话**（空名，走首次消息自动命名），设置 REPL 指针，不移动 `store.session_id()`（终端 lane）。
- 不迁移历史数据；用户可 `/session` 主动切回终端会话。
- 更新 `src/web/tests/ipc_bridge.rs` 现有测试：normal 首启不再等于 terminal id，且 terminal lane 不动。
- WebUI 的 current session 语义不变：WebUI 继续使用全局 current（终端）或显式会话；不要顺手改。

## 3. 退出 REPL 再进入，footer 上下文为 0

### 根因（精确定位）

`src/runtime/state.rs::cold_context` 对 `ContextSnapshot.tokens` 硬编码 `0`，只算了累计 token。daemon 冷启动后 `manager.context.tokens = 0`；`src/web/sessions.rs::session_state_for` 对 `current_session_id == session_id` 的请求直接读这份内存快照，不重新计算。因此 REPL 首帧显示 `0/168k`；发生一次对话后 `finish_run` 写入真实 context 才恢复。

### 方案

- `session_state_for` 不再无条件信任 `manager.context.tokens`。为当前会话也构建 pinned Agent 调 `current_context()`（与当前非 current 分支同一条路）。
- 为避免每次 IPC 都重建 registry/Agent：增加 `ManagerState` 的 per-session context memo（`HashMap<String, ContextSnapshot>`），在 turn finish / undo / pop / reset / switch 后失效或更新；daemon 启动时 memo 为空，首次 `session_state_for` 现算。
- `cold_context` 改为调用同一计算路径；至少把 `tokens` 改为从会话历史计算而不是 0。
- 回归测试：新建有历史的 temp state，构造 DaemonState 后调 `session_state_for(current)`，断言 `context_tokens > 0`；把修复去掉测试必须报红。

## 4. 第二个 REPL 上键没有刚输入的历史

### 现状与实测

- 当前实现：远端 REPL 在提交前执行 `persist_repl_history_entry(paths, input)`（`remote/interactive.rs` 约 941 行），写入全局 `state/repl-history.jsonl`；启动时 `load_repl_input_history` 合并“会话历史 + 该文件”。
- 我在 `/tmp` 沙箱 + mock LLM 下用 tmux 开两个 `miyu normal` 实测：**第二个 REPL 上键可以看到第一个 REPL 刚输入的内容**。当前代码未复现。
- 可能变量：两次启动的 `MIYU_HOME` 不同、第一/第二个 REPL 实际落在不同会话（例如一个先被 `/session` 切走）、直连与 daemon 混用、或第二 REPL 在文件 flush 前极早启动（实测 150ms 也未复现）。

### 方案（先做鲁棒化，不等复现）

1. **历史归属会话化**：新增 `repl_history` 表 `(session_id, seq, entry, created_at)`，或把现有 JSONL 记录加 `session_id` 字段（新文件格式，旧行视为全局 legacy）。
2. **daemon 是唯一写入者**：客户端提交时通过 IPC `RecordReplHistory { session_id, entry }` 写入；`GetReplSession` 返回该会话历史尾部。两个 REPL 无论启动先后，都从同一真相源读。
3. 客户端保留现有 file 路径作为离线/直连 fallback；直连模式本地直接写 SQLite（它本就有 StateStore）。
4. 启动时只显示本会话历史 + legacy 全局条目；`/reset` 不清输入历史（与当前语义一致）。
5. 给用户补充复现信息：是否两个 REPL 的 `MIYU_HOME` 一致、是否使用 `/session`、是否是 `MIYU_DIRECT`。在根因完全确认前，按上面方案实施仍能消除该类问题。

## 5. 修改文件清单

- `src/cli/repl/layout.rs`、`src/cli/repl/editor.rs`：词跳。
- `src/cli/tests/input_editing.rs`：词跳回归。
- `src/web/ipc_server.rs`、`src/cli/repl/direct.rs`：ensure_repl_session。
- `src/web/sessions.rs`、`src/runtime/state.rs`、`src/runtime/run.rs`：context memo / cold context。
- `src/ipc/protocol.rs`、`src/web/session_cmds.rs`：RecordReplHistory 与 GetReplSession 历史返回。
- `src/state/conversation_db/*`、`src/state/sessions.rs`：repl_history 表。
- `src/cli/repl/remote/interactive.rs`、`src/cli/repl/session.rs`：历史读写切换。

## 6. 验收

1. Ctrl+Left/Right 在 ASCII/CJK/混合文本中按词移动，占位符原子。
2. 全新 `MIYU_HOME`：先 `miyu normal` 后，`miyu session` 中 terminal 会话与 REPL 会话不同；`miyu ask --continue` 仍进 terminal 会话。
3. 有历史的 daemon 冷启动：REPL 首帧 footer `context_tokens` 非 0 且与下一次请求前估算一致。
4. 两个 REPL 同时开：第二个上键有第一个的历史；重启 daemon 后历史仍在且按会话隔离。
5. 门禁与字节稳定性同总览。
