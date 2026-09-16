# 大文件拆分施工单(2026-09-16,拆 crate 之后)

规模门禁(`test_scripts/refactor_size_report.py`):目标 800 / 上限 1500 / 红线 2000。现状 39 个文件超 1000 行,
1 个越红线(`render/stream/timeline.rs` 2358)。原则:**按职责切,不按行数切;只搬代码不改逻辑**;
老路径用 `pub(super) use` / `pub(crate) use` 保住;`impl X` 可以分散在多个文件里(同 crate 内)。
硬约束、验收三件套、检查点、委派规则与 `2026-09-16-crate-split-burndown.md` 的「硬约束」一节完全相同
(路径现在在 `crates/<crate>/src/…` 或根包 `src/…`;cargo 命令加 `--workspace`)。

两类:**A 机械(可委派)**——把内联测试搬出去、把方法组搬进子文件;**B 工程(我做)**——拆千行以上的单个函数。

## A 批(可委派;每个文件一小节,做完一个就跑 `cargo check --workspace --all-targets` 与规模门禁)

### A1 `crates/miyu-hosts/src/render/stream/timeline.rs`(2358 → 目标 ≤ 900)— **已完成(09-17,Opus 子代理,12.8 分钟一次过)**

结果:`timeline/mod.rs` 781、`live.rs` 623、`subagent.rs` 691、`question.rs` 166、`glyphs.rs` 136(+39 行模块头与再导出);放宽 4 处 `pub(super)`(`step_rows` `step_detail` `indented_body` `subagent_peek`);`render::` 204/204;check 零警告。子代理报出的施工单偏差:`render_speech_lines`/`undecorate`/`summary_line` 的实际调用方不在施工单指的文件里(靠再导出走通),`step_rows` 的 doc 注释拆前就串行(未动)。

把文件变成目录 `timeline/`:`mod.rs` 留 `StreamRenderer` 的时间线核心(`timeline_enabled` `timeline_static`
`timeline_push_tools` `settle_new_steps` `commit_static_steps` `timeline_push_thought` `flush_after_timeline`
`queue_after_timeline` `cut_timeline` `replay_*_elapsed`)与 `Timeline` / `Step` / `PendingStep` 三个类型及其 impl;

- `timeline/live.rs`:`impl StreamRenderer { timeline_live, refresh_live_block, live_tool_lines, live_block_lines,
  timeline_waiting, timeline_preparing_line, timeline_running_tool_lines, timeline_thinking_line }` 与它们独占的自由函数
  (`render_speech_lines` `write_compact_summary` `push_wrapped` `undecorate` `indent_body` `indented_body` `peek_tail` `summary_line`);
- `timeline/subagent.rs`:`impl StreamRenderer { subagent_* 全部, refresh_subagent_panels, publish_subagent, finish_subagent_log,
  running_subagent_tokens }` + `SubagentLog` + `subagent_lines` `collapse_subagent_segment` `flush_subagent_thought`
  `trim_subagent` `subagent_title` `thread_panel`;
- `timeline/question.rs`:`timeline_push_question` `write_question_exchange`;
- `timeline/glyphs.rs`:`tool_glyph` `step_rows` `step_detail` `tool_output_lines`。
- `format_seconds` 已在 `durations`,`command_peek` 在 `tools::tool_display`,不动。
- 测试文件 `render/tests/timeline.rs`、`timeline_panels.rs` 不动;它们引用的 `pub(super)` 帮手在 `timeline/mod.rs` 里用
  `pub(super) use live::*; pub(super) use subagent::*;…` 再导出。

### A2 `crates/miyu-engine/src/tools/mod.rs`(1378 → ≤ 700)— **已完成(09-17,D2 批)**:mod.rs 448;`readable_names.rs` 217(多带了紧邻的 `readable_load_target_name`/`batch_preparing_phase`)、`tests.rs` 537、`tier_schema_probe.rs` 117、`shape_tests.rs` 59;放宽 1 处。

`mod tests`(L640 起 544 行)→ `tools/tests.rs`(`#[cfg(test)] mod tests;`);`mod tier_schema_probe`(122 行)→ `tools/tier_schema_probe.rs`;
`mod shape_tests`(62 行)→ `tools/shape_tests.rs`。三个都是纯搬,`use super::*` 保持。
`builtin_readable_tool_name` / `builtin_readable_group_name` / `readable_tool_name` / `preparing_phase`(约 180 行)→ `tools/readable_names.rs`,
`mod.rs` 里 `pub(crate) use readable_names::*;`。

### A3 `crates/miyu-engine/src/tools/vision/mod.rs`(1360 → ≤ 900)— **已完成**:mod.rs 825;`register.rs` 138(三个 scoped;不带 scope 的 `register` 留 mod.rs,与 `mod register` 同名共存)、`tests.rs` 408;放宽 0 处;被迫加 `use super::image_generation;` 再导出(下沉一层后 `super::` 指向变了)。

`mod tests`(314)、`mod batch_tests`(46)、`mod video_route_tests`(44)→ `vision/tests.rs`(一个文件里三个 `mod`);
`register_scoped` 一家(`register_scoped` `register_scoped_platform` `register_scoped_local` 与只被它们用的帮手)→ `vision/register.rs`。

### A4 `crates/miyu-engine/src/tools/subagent.rs`(1343 → ≤ 800)— **已完成**:`subagent/mod.rs` 647、`log.rs` 319、`audit.rs` 193、`tests.rs` 197;放宽 10 处 `pub(super)`;`include_str!` 相对路径多一层 `../`。

变目录 `subagent/`:`mod tests` → `subagent/tests.rs`;`SubagentAudit` + `record_subagent_audit` → `subagent/audit.rs`;
`spawn_subagent_log_bridge` `readable_subagent_log_line_timed` 与日志相关帮手 → `subagent/log.rs`;`run_core` `spawn_background` `register` 留 `mod.rs`。

### A5 `crates/miyu-engine/src/tools/apply_patch.rs`(1209 → ≤ 800)— **已完成**:`apply_patch/mod.rs` 434、`parse.rs` 212、`hunks.rs` 297、`tests.rs` 272;放宽 5 处。D2 批四文件共 21 分钟一次过,`tools::` 337/337;子代理用「去空白行多重集」比对证明零丢失。

变目录:`mod tests`(275)→ `apply_patch/tests.rs`;`parse_patch_with` `detect_patch_namespace` 与解析帮手 → `apply_patch/parse.rs`;
`apply_hunk` `seek_sequence` `preflight_operations` → `apply_patch/hunks.rs`。`mcp/result_tests.rs` 那种 `#[path]` 写法不用学,直接目录模块。

### A6 `src/cli/repl/tail/screen/overlay.rs`(1384 → ≤ 900)— **已完成(09-17,D3 批)**:`overlay/mod.rs` 508、`log.rs` 501、`screen.rs` 390;放宽 17 处(含 `LogStep` 10 个字段);`paint_body_above` 的 `pub(super)` 平移成 `pub(in super::super)`;子文件里 `super::` 深一层。

变目录 `overlay/`:`impl Screen`(L997 起 385 行,浮层相关的 Screen 方法)→ `overlay/screen.rs`;
`log_steps` `log_step_detail` `collapse_log_segment` `read_tail` `LogStep` → `overlay/log.rs`;`Overlay` 结构与 `impl Overlay` 留 `mod.rs`。

### A7 `src/cli/repl/tail/screen/mod.rs`(1298 → ≤ 900)— **已完成**:mod.rs 651、`draw.rs` 304(绘制半:paint/row_key/top_pad/suspend/resume/选区上色)、`tail_impl.rs` 353;放宽 0 处。

`impl super::LiveReplTail`(L947 起 349 行)→ `screen/tail_impl.rs`;`impl Screen`(719 行)按「绘制」与「输入/状态」分两半,
绘制那半 → `screen/draw.rs`。

### A8 `src/cli/mod.rs`(1368 → ≤ 900)— **已完成**:mod.rs 814、`history_replay.rs` 323、`entry_flows.rs` 137、施工单外多切一刀 `terminal_guard.rs` 114(光标还原 + 挂断守卫 + 全屏视口查询);再导出按原可见性用私有 `use`(不是施工单写的 `pub(crate)`),放宽 21 处 `pub(super)`。D3 批三文件 14.5 分钟一次过,`cli::` 264/264。

`session_replay_frame` `ReplHistoryEntry` 及 impl `run_history_with_state` `persist_remote_queued_submission`(约 290 行)→ `cli/history_replay.rs`;
`run_one_shot` `run_oobe_flow` → `cli/entry_flows.rs`。`run`(292 行的总入口)留着。

### A9 `crates/miyu-hosts/src/web/sessions.rs`(1191 → ≤ 800)— **已完成(09-17,D4 批)**:`sessions/mod.rs` 523、`http.rs` 345、`state.rs` 355;放宽 1 处;`reset_conversation` 其实是路由 handler 但按施工单归 state.rs;`ResetConversationRequest` 被 commands_api 复用留 mod.rs。

变目录:HTTP handler(`*_http`)→ `sessions/http.rs`;`resolve_local_session_ref_with_kinds` `replacement_for_last_session`
`session_state_for` `reset_conversation` `spawn_session_title_refinement` → `sessions/state.rs`。

### A10 `crates/miyu-base/src/config/mod.rs`(1186 → ≤ 800)— **已完成**:mod.rs 492、`voice.rs` 368(四个结构体 + Default + inherent impl + 21 个 default_* 帮手)、`memory.rs` 88、`context.rs` 85、`display.rs` 128(含 RawDisplayConfig 与手写 Deserialize)、`scaling_probe.rs` 50;放宽 0 处。D4 批 12.5 分钟一次过,`web::` 122/122、`config::` 123/123。**A 批十份全部完成,四批子代理共 61 分钟,零返工。**

按领域把结构体定义连同 `impl Default` 搬出:`VoiceConfig` + `MiniMaxTtsConfig` 等语音配置 → `config/voice.rs`;
`MemoryConfig` → `config/memory.rs`;`ContextConfig` → `config/context.rs`;`RawDisplayConfig` → `config/display.rs`;
`mod.rs` 里 `pub use voice::*;` 等再导出。`mod scaling_probe` → `config/scaling_probe.rs`。
serde 派生与字段一字不改;`config/tests/*` 断言不动。

### A 批验收

每个文件做完:`cargo check --workspace --all-targets` 零警告;`python3 test_scripts/refactor_size_report.py --check` 该文件降到目标以下、
无新增越线;全部做完跑整套 `refactor-check.sh`、`request_shape_probe`(五张脸逐字节相同)、`cargo test --workspace` 用例数不减。

## B 批(我做):千行函数

- `crates/miyu-engine/src/agent/turn_loop/mod.rs::chat_with_tools`(**1216 行的一个函数**)与
  `src/cli/repl/remote/interactive.rs::run_remote_repl`(**1334 行**)。这两个不是搬文件能解决的:要按阶段抽成方法
  (请求组装 / 一轮模型往返 / 工具执行 / 续传与排队 / 收尾),借用与局部状态要重新划分。
  验收除三件套外加 `request_shape_probe`、`testkit/cli` 黑盒、`testkit/tui` 走查;每抽一块跑一次 agent 测试。

### B1 `chat_with_tools` 切法(按 09-16 通读定)

循环内的局部态收成 `struct RoundState`(tool_round / question_rounds / replay_start / overflow_recovery_attempted /
loaded_tools / contract_hinted / usage_accumulator / last_round_completed_at / responses_continuation /
continuation_input_start / continuation_context / artifact_* / repeat_gate / repeat_fused),`chat_with_tools` 只剩
「初始化 → loop { 六步 } 」的骨架,每步一个 `impl Agent` 方法,文件按步分:

| 步 | 原行 | 抽成 | 住哪 |
|---|---|---|---|
| 目录热刷新(脚本 / 技能) | 84–141 | `refresh_tool_catalogs(&mut self)` | `turn_loop/catalog_refresh.rs` |
| 组请求(续传切片、上下文插入、tool 结果配平、缓存写宽限、keepalive 快照) | 143–210 | `build_round_request(&mut self, …) -> RoundRequest` | `turn_loop/round_request.rs` |
| 一轮模型往返(select:超越 / 流 / 转轮) | 211–268 | `run_model_round(&mut self, …) -> Option<Result<ChatResult>>` | `turn_loop/model_round.rs` |
| 出错自愈(续传不支持重发全量、溢出压缩重试) | 269–373 | `recover_round_error(&mut self, …) -> Result<RoundRecovery>` | `turn_loop/overflow_recovery.rs` |
| 被超越 / 无工具收尾 / 轮数上限收尾 / 用量事件 | 374–619 | `handle_superseded_round`、`emit_round_usage`、`finish_without_tools`、`finish_at_tool_limit` | `turn_loop/finish.rs` |
| 工具批执行(长度截断、ask_question、复读闸、并行 task、单工具+进度+子过程检查点、内联媒体、载荷落库) | 620–1148 | `execute_round_tool_calls` + `run_ask_question` + `run_tool_with_progress` + `attach_inline_media` | `turn_loop/tool_exec.rs` |
| 轮后(tool_flow 检查点、goal 通知注入、排队消息消费+续传上下文) | 1149–1239 | `consume_queue_after_round` | `turn_loop/queue.rs` |

`attach_contract` 闭包借了 `self.tools` 与 `contract_hinted`,改成 `fn attach_contract(&self, hinted: &mut BTreeSet<String>, name, message)`。
所有发事件的方法保持 `F: FnMut(AgentEvent) -> Result<()>` 泛型;`messages: &mut Vec<ChatMessage>` 与 `RoundState` 作参数传,不进 `self`。
每抽一步跑 `cargo test -p miyu-engine agent::` + `request_shape_probe`(五张脸逐字节相同)。

### B2 `run_remote_repl` 切法

主循环是「读输入 → 斜杠命令 22 分支 → 发回合」。局部态收成 `struct RemoteRepl { paths, config, mode, active_session_id, history,
cumulative_tokens, footer, live_repl, jobs_shared, jobs_feed }`(`src/cli/repl/remote/session_state.rs`),
斜杠命令的 `match` 整体搬成 `RemoteRepl::run_slash_command(&mut self, command, args) -> Result<LoopStep>`
(`remote/slash.rs`,分支体逐个抽成方法,重复四次的「重取会话状态 + 重建 footer + 重建 client 摘要」收成 `refresh_footer_from_session`),
发回合那段抽成 `RemoteRepl::submit_chat(&mut self, input, images, entry) -> Result<LoopStep>`(`remote/submit.rs`);
`run_remote_repl` 只剩建 `RemoteRepl` + `loop { read_input → 分派 }`。验收:`testkit/cli` 黑盒 48 项 + `testkit/tui` 走查。

## 不动的

`agent/tests/context.rs`、`config/tests/{provider,platform}.rs`、`render/tests/*`、`cli/tests/*`、`web/tests/session.rs` 这些
测试文件超 1000 行属于测试体量,不在规模门禁的红线判定里,不拆。

# 场所层 `AgentMode` → 人格作用域(大文件拆分之后做;我做)

**现状**:回合引擎里 `AgentMode` 已只剩边界折算(`Agent::new` / `switch_mode` → `core.dev`),但 cli / web / runtime / platforms
还有 ~300 处按「模式」思考;`tools::build_tool_registry(config, paths, mode, …)` 把 `Dev` 折成 `DEV_PERSONA`、`Normal` 折成
`config.active_persona_scope()`——这条折算就是答案:**场所要说的从来是「这次回合用哪个人格作用域」**。

**目标**:
- `miyu_base::config` 新增 `PersonaScope`(newtype over `String`,构造 `PersonaScope::dev()` / `PersonaScope::active(&config)` /
  `PersonaScope::named(id)`;`is_dev()`;`mode_word() -> &'static str` 给 IPC/会话记录/CLI 输出用,`normal|dev` 一字不改;
  `from_mode_word(Option<&str>, &config)` 给 IPC 入口用)。
- 引擎:`Agent::new(…, scope: PersonaScope)`、`switch_persona(scope, tools)`、`Agent::persona_scope()`;`build_tool_registry(config, paths, &scope, …)`;
  `AgentMode` 枚举删除(agent/control.rs),`Agent::mode()` 删除。引擎内 21 处非测试引用清零。
- 场所:`runtime::TurnRequest.mode` → `scope: PersonaScope`;`runtime::state` 的 `normal_tools/dev_tools` → 按 scope 建(`tools_for(&scope)`);
  IPC 协议里 `mode: Option<String>` 字段与序列化不动(兼容),进口处 `PersonaScope::from_mode_word`,出口处 `scope.mode_word()`;
  cli 的 `--dev` / REPL 车道切换 / banner / footer / 会话回放按 `scope.is_dev()` 取色,`LiveReplTail::new(scope, …)`;
  web dto 的 `mode` 字段照旧输出 `mode_word()`。
- 一律脚本机械改写 + 编译器驱动;每一层改完跑该层测试;最后 `request_shape_probe` 五张脸逐字节相同、`testkit/cli` 48 项、
  `testkit/tui` 走查、oobe 探针 7/7。
- 文档:`docs/architecture.md` 「回合引擎里没有模式」一段改成「场所与引擎都只说人格作用域,`normal|dev` 是线上的词」;
  `docs/interfaces/README.md` 对应句;`next-release-note.md` 不写(用户不可见)。

**不做**:不改会话库 schema(`mode` 列照存 `normal|dev`);不改 WebUI 前端 JS(它只认 `mode` 字段的词)。
