# core / normal 接口治理:施工记录与后续拍板(2026-09-16)

来源计划:`~/Downloads/Miyu-2026-09-14-plan/06-core-normal与接口治理总计划.md`(GPT 起草)。
基线 `843c169a`。本文记录**已经做了什么、怎么验的、还有什么没做**;所有改动未 commit,等用户验收。

## 1. 结论

- 三层「下层不依赖上层」从文档约定变成编译事实:`arch_dep_check` 白名单清零,历史 8 条反向边
  (25 处引用)全部消掉,门禁输出「无」。
- 手法只有下沉、窄端口、假对象三种,没有新建第四层、万能 HostContext 或事件总线。
- 行为零变化:三个场所的 tools 数组逐字节不变(`shape_tests`);对外 HTTP 头、错误文案、
  消息来源(`OutboundOrigin`)逐一保留。
- 计划里的 Phase 2(Agent 快照)、Phase 3(子系统生命周期)、Phase 6(死代码)、Phase 7(PM 预检)
  **没动代码**——计划本身要求先拍板,且碰提示词字节;见 §5。

## 2. GPT 已完成部分(接手前,已复核)

| 项 | 落点 | 复核 |
|---|---|---|
| ProviderCatalog 领域模块 | `src/provider_catalog/{mod,http,cli}.rs`、`src/provider_url.rs` | `cli.rs` 与旧 `config_tui/cli_catalog.rs` 逐字节相同(只改可见性);`http.rs` 是旧 `fetch_models` 的等价搬迁。**UA 头被改成 `miyu-provider-catalog`,已改回 `miyu-config`**(对供应商可见的字节不该顺手变) |
| Web / TUI / OOBE 改调领域模块 | `web/providers_api.rs`、`config_tui/mod.rs`、`oobe/providers.rs` | 消掉 `web → config_tui` 2 处 |
| `compose_registry` 拆分 | `tools/compose.rs`(主流程 + 场所策略)、`compose_core.rs`、`compose_providers.rs` | 注册顺序不变,`shape_tests` 过;`tools/mod.rs` 1534 → 1368 行 |
| 终端视口下沉 | `terminal::{set_content_viewport, content_viewport}`,全屏 drop 时清 | 消掉 `render → cli` 2 处、`tools → cli` 2 处 |
| 中转线宿主工具位 | `runtime/live_turn.rs` + `platforms::LiveTurnGuard` 登记 | 消掉 `llm → platforms` 1 处;`host_tools_allowed` 是不变字段,守卫创建时取值等价 |
| `docs/interfaces/` | 9 份英文三行桩 | 本轮全部重写为中文 as-built 契约 |

## 3. 本轮完成

### 3.1 四组反向边

| 边 | 处数 | 解法 | 文件 |
|---|---|---|---|
| `tools → web` | 5 | `runtime::VoicePort` + `QqOutreachPort` 端口 | `tools/{voice_speak,voice_chat,platform_outreach}.rs` |
| `platforms → web` | 4 | `VoicePort` | `platforms/tool.rs`(send_voice_message)、`platforms/onebot/voice_inbound.rs` |
| `tools → platforms` | 5 | 2 处走 `QqOutreachPort`;3 处是 vision 测试借真 `PlatformTurnContext`,改成实现 `PlatformToolContext` 的假对象 | `tools/platform_outreach.rs`、`tools/vision/mod.rs` |
| `web → cli` | 4 | IPC 解码下沉 `runtime/ipc_events.rs`(与 `EventRecord` 同家);事件→渲染器下沉 `render/agent_events.rs`;`cli::handle_agent_event` 留薄包装只管问题面板 | `web/actor/job_wake.rs`、`cli/mod.rs`、`cli/repl/session.rs`(`ipc_text`/`ipc_u64` 改转发) |

端口实现与装入:`web/voice_bridge.rs::VoiceBridgePort`、`platforms/onebot/proactive.rs::OutreachPort`,
都在 `web/server.rs` 紧随 `voice_bridge::install_state` 装入。设计说明见
`docs/interfaces/host-capabilities.md`。

保留的语义细节:
- 非 daemon 进程端口为 `None`,错误文案原样("speak 只能在 daemon 里用(当前不是 daemon 进程)"、
  "send_qq_message only works inside the daemon"、"send_voice_message 只能在 daemon 里用");
- `send_qq_message` 文本路沿用旧 `send_direct_text` 的 `OutboundOrigin::Plugin`,语音路 `Tool`;
- 收件人策略按**调用时**配置现算(与旧代码一致,配置重载即生效),`qq_outreach_policy` 是纯函数。

### 3.2 其它

- 删除 `src/cli/ipc_event.rs`、`src/cli/tests/ipc_event.rs`(17 条用例原样搬到 `runtime/ipc_events.rs`)。
- `test_scripts/arch-dep-waivers.json` 清空;`arch_dep_check.py` 文档里的旧 `scripts/` 路径改为 `test_scripts/`。
- `docs/interfaces/` 九份文档重写;`docs/architecture.md` 新增「八、依赖门禁与宿主端口」。
- 新增用例:`runtime::ports`(策略纯函数、端口装入)、`tools::platform_outreach`(无端口报错 /
  禁用 / 别名解析 / 主管理员缺省 / 来源与分段)。

## 4. 验证

| 项 | 结果 |
|---|---|
| `cargo check --all-targets` | 通过(31s 增量) |
| `python3 test_scripts/arch_dep_check.py` | 「跨层引用现状:无」 |
| `cargo test --lib`(cgroup MemoryMax=20G) | 2405 passed / 0 failed / 31 ignored(首轮 1 失败是新断言写错,已修) |
| 定向 `runtime::ports runtime::ipc_events tools::platform_outreach tools::vision cli::tests::static_timeline render::agent runtime::live_turn tools::shape_tests` | 45 passed / 2 ignored |
| `cargo fmt --check`、`git diff --check`、`fmt_no_regress.py` | 通过 |
| `check-model-english.sh` | 通过 |
| `refactor_size_report.py --check` | **红,基线之前就红**:`src/render/tests/timeline.rs` 基线 1517 行,HEAD `843c169a` 已是 1563 行;本轮未碰该文件 |
| `bash test_scripts/refactor-check.sh`(全 target) | 见 §4.1(第一轮)与 §4.3(第二轮) |

### 4.1 refactor-check 全量结果

日志 `~/.cache/miyu-refactor-2026-09-16/refactor-check.log`(cgroup MemoryMax=24G、禁 swap):

| 门 | 结果 |
|---|---|
| 格式(`fmt_no_regress.py`) | 通过 |
| 编译(`cargo check --all-targets`) | 通过 |
| 测试(全 target,`--no-fail-fast`) | lib 2407 + 集成 2 + 1 = **2410 passed / 0 failed / 31 ignored**;`.test-count` 2337 → 2410(脚本自动写,随本轮一起提交) |
| 模型面语言 | 通过 |
| 文件规模 | **红(HEAD 旧账)**:`src/render/tests/timeline.rs` 1517 → 1564;脚本在此退出,没跑到下一道 |
| 依赖方向 | 单独跑 `arch_dep_check.py`:「无」 |

未跑:真供应商两轮 cache-usage(工具面字节由 `shape_tests` 钉死,本轮没有描述/schema/顺序改动);
真机 daemon smoke(见 §6 用户验收步骤)。

## 4.2 第二轮(用户批准「按建议推进」后,同日)

### Phase 3 子系统生命周期(已落地)

- `src/config/subsystems.rs`:`SUBSYSTEMS` 挂接表(id / 阶段 / 启用判定)+ `EnabledSubsystems::resolve`
  快照;`PersonaManifest::enabled_subsystems`。全仓唯一真相源。
- 接线:Agent 持 `subsystems` 快照(构造/`reload_config`),记忆前言、记忆库初始化、人格提醒只看它;
  `compose_registry` 折一次快照传给 core/providers;`rescope_platform_memory_tools` 看清单;
  real_context 插件 `settings()` 叠人格意愿(`persona_overlay`)——`emotion` 开关从此真的有人读。
- 修掉的不一致:记忆前言在 `prepare_for_turn` 重组时只看机器配置(清单关着记忆的人格第一回合起
  前言又回来)、平台回合把记忆工具补回给清单关着记忆的人格、`reload_config` 无条件建记忆库。
  这三处只影响 `subsystems.memory = false` 的自定义人格,默认人格与 dev 逐字节不变。
- 先红后绿:`real_context::tests::subsystem_gate`(人格关情绪 / dev 默认 两条红)、
  `agent::tests::subsystems::persona_with_memory_off_never_gets_the_memory_preamble`(红)→ 接线后全绿。
- 文档:`docs/interfaces/subsystems.md`(挂接表、判定、已知不一致),`docs/architecture.md` 新节。
- 子系统间直接引用只剩 `memory → skills` 一处(`is_generated_skill` 纯文本判定),按查询型引用接受。

### Phase 2 Agent 状态分组(已落地第一步)

- `src/agent/turn_state.rs`:44 个平铺字段按生命周期分成 `core: CoreTurnSnapshot` / `input: TurnInput` /
  `runtime: TurnRuntime` / `memory: MemorySubsystem`,字段、语义、初始值不变;`Agent` 只剩 10 个字段。
- 量尺:`agent/tests/request_shape.rs`(ignored 探针)把四个面的 system / messages / tools 落成 JSON,
  重构前后归一(运行时戳、临时家目录、工具名排序)后**逐字节相同**。
- 验证:`cargo check --all-targets` 通过;`agent::` + `shape_tests` 167 过;全 lib 见下。
- 未做:给扩展的只读视图(`PromptContext` 等)——没有消费者前不造;`AgentMode` 退役(340 处)另开专项。

### Phase 7 PM 预检(已落地最小集)

- `pm::SUPPORTED_CONTRACTS`(scripts v1 / skills v1 / persona-manifest v1,= 2026-09 as-built)与
  `pm::GRANTABLE_CAPABILITIES`(今天为空)是契约版本与能力的真相源。
- `miyu-package.toml` 新增可选字段 `requires-contracts = { scripts = 1 }`、`requires-capabilities = []`;
  `plan_install` 在写任何文件前跑 `check_contracts`:不认识的契约 / 高于支持的版本 / 任何能力请求 → 拒装。
  旧包不写这两个字段照常装。
- 测试:`pm::tests::contract_and_capability_preflight_refuses_what_the_host_cannot_honor` +
  `invalid_packages_are_rejected_before_any_write` 新增「契约不过一个文件都不写」。
- 文档:`docs/interfaces/compatibility.md` 表改成 as-built 版本;`docs/wiki/15-扩展指南.md` 清单示例补两行。

### Phase 6 死代码(只出清单,未删)

`docs/plan/2026-09-16-dead-code-inventory.md`:临时去掉全局 `allow(dead_code)` 编译得 177 条
(83 函数 / 28 方法 / 21 字段 / …,130 个文件)+ 编译器看不见的结构性候选(`AgentMode` 退役、
`persona_hint::reminder_message`、`agent → render` 两处)。每条按「删冗余先确认」流程走。
本轮新加的代码里,`SubsystemDescriptor.phases` 只有测试与文档读(显式 `allow`),
`EnabledSubsystems::{is_empty, ids}` 收成 `#[cfg(test)]`。

### 4.3 第二轮验证

| 项 | 结果 |
|---|---|
| Phase 3 接线后 `cargo test --lib` | 2419 passed / 0 failed / 31 ignored |
| Phase 2 分组后 `cargo test --lib` | 2419 passed / 0 failed / 32 ignored(多的 1 条是请求形状探针) |
| 请求形状量尺(四个面 system / messages / tools) | 归一后逐字节相同 |
| `pm::` | 9 passed |
| 依赖门禁 | 「无」 |
| 文件规模门禁 | 仍只有 HEAD 旧账那一条红 |
| 全 target `refactor-check.sh` | 格式/编译/模型面语言过;lib 2417 过 3 失败——`claude_code::haiku_has_no_thinking_variants_and_sends_no_effort`、`onebot::tests::notices::{a_long_mute_is_only_trusted_until_the_recheck_window, group_ban_notices_update_bot_and_whole_group_mute_state}`,三者所在模块本轮没碰,单独复跑 7/7 过,整套复跑 **2420 passed / 0 failed**(并行时序抖动:群禁言缓存是进程全局态、假 CLI 写临时文件);文件规模仍只有 HEAD 旧账那条红;`.test-count` 2337 → 2423 |

### 第三轮(用户重申原始目标后,同日)

**Phase 4 内置插件登记表**:`src/config/builtin_plugins.rs`(描述符,派生 `PLUGIN_IDS` /
`TOGGLE_PLUGINS` / `plugin_label`)+ `src/tools/builtin_plugins.rs`(注册函数表,
`registrars_cover_every_builtin` 钉对齐)。新增一个内置插件从 12 处降到 5 处(实现、描述 JSON、
`include_str!`、两张表各一行);`compose_core` 里 15 个 `if` 变成一次循环。请求形状量尺仍与
Phase 2 之前逐字节相同;派生名单与手写表逐字相同有测试。

**扩展查宿主信息(计划 03 §5.4 的"进程外只读查询")**:
- `runtime::host_grants`:daemon 拉起脚本时签一次性令牌(守卫掉落即作废),只在 daemon 里签;
- `runtime::host_query`:`host.info` / `providers.list` / `providers.get` / `subsystems.enabled`,
  脱敏 DTO,错误码 permission_denied / unknown_method / invalid_argument / not_found;
- IPC `HostQuery` + `miyu host <method> [json]`(隐藏子命令);脚本头部 `Capabilities:`,
  只给 `Trust: owner` 的脚本;环境 `MIYU_HOST_TOKEN` / `MIYU_HOST_CAPABILITIES` / `MIYU_HOST_BIN`;
- 能力词表 `runtime::HOST_CAPABILITIES` 同时是 PM `requires-capabilities` 的预检表;
  契约版本表挪到 `config::contracts`(host.info 与 PM 共用)。
- 测试:令牌生命周期、查询脱敏与错误码、IPC 一次往返(不认识 / 签发 / 越权 / 作废四态)、
  头部解析与授权规则、PM 预检认识的能力放行。

**子系统之间不许直接 use**:`arch_dep_check` 新增 memory / skills / persona_hint 互斥规则;
唯一的一处 `memory → skills`(重置时清自动技能)挪到 `skills::purge_generated_skills`,
由 `miyu reset --include-skills` 在上层接起来;门禁「无」。

**dsh 与 pi 的对照**(用户点名参考;两份完整报告在会话记录里,这里只留借鉴与不借鉴):

| 借鉴 | 来源 | 落在哪 |
|---|---|---|
| 模型面投影用显式白名单,不是"结构体减内部字段" | dsh `schemas()` allowlist | host_query 的 DTO 逐字段拼,不序列化 `ProviderConfig` |
| 受限门面永远不返回能反向拿到运行时的句柄 | dsh `denyContext` | 宿主查询只回 JSON DTO;端口 trait 不暴露 `DaemonState` |
| 密钥只以引用出现,查询只答"配了没有"不答值 | dsh credentials | `providers.*` 没有 api_key / base_url |
| 能力声明 + 未知即拒,不静默装 | pi 的空白(pi 没有兼容版本字段) | PM `requires-contracts` / `requires-capabilities` |
| 坏掉的单元列出原因而不是跳过 | dsh `AgentPreset.broken` | 脚本未知能力记 warn 并保留脚本(未做"列出原因"UI,记入待办) |
| 单一入口对象 + 每次注册可归属 | pi `ExtensionAPI` / `extensionPath` | 内置插件登记表按 id 归属;脚本已按文件归属 |
| 一次性令牌 + 进程退出即作废 | 自研(两者都没有:pi 全权限、dsh 承认沙盒不是边界) | `host_grants` |

不借鉴:dsh 的运行时服务定位(Rust 靠编译期分层门禁得到同样保证)、pi 的"扩展有整机权限"
信任模型、两者都没有的热替换承诺。

### 第四轮(用户批准死代码清理)

两阶段全靠编译器裁决:先把 lib 构建里 never used 的 192 个函数/方法/常量标 `#[cfg(test)]`
(`cargo check --all-targets` 仍过),再去掉全局 allow 跑测试构建,连测试都不用的 88 项删掉,
104 项只有测试在用保留 `#[cfg(test)]`;字段/变体/结构体逐个看(协议与 serde DTO 保留、内部从未读
的删、RAII 守卫改下划线)。`cargo fix` 清 `use`,三处只被测试用的 import 补 `#[cfg(test)]`。
`SurfaceTrust::Internal` 从未构造,删;`JudgeRequest.force_moderation_check` 有写无读,记为疑似 bug 不删。
清单与结果:`docs/plan/2026-09-16-dead-code-inventory.md`。todolist 的「core层和normal重构」已标完成。

### 第五轮(继续推进)

- 文件规模门禁转绿:`src/render/tests/timeline.rs`(1563 行,HEAD 旧账)按「面板形态」拆出
  `timeline_panels.rs`(500 行),共用夹具改 `pub(super)`;用例数不变。`refactor-check.sh` 五道门禁首次全绿。
- `agent → render` 两处反向使用消掉:`is_command_tool` 下沉到 `tools`(render 转发),
  `SPINNER_INTERVAL` 下沉到 `terminal`(render 转发);门禁加 `agent` 不许 use `render`。
- 端到端实测 `testkit/host-query/run.py`:隔离 home + 独立端口 daemon(18397)+ 桩 LLM,
  桩模型调 `host_probe` 脚本,脚本在 daemon 里拿令牌查 `providers.list` / `host.info` /
  `subsystems.enabled`。**9/9 过**:无令牌直接调用被拒、回合完成、脚本拿到令牌与授权集、
  providers.list 含 stub 且无 api key / 地址、激活选择正确、host.info 报契约版本、
  未授权方法 permission_denied、整段输出不泄漏密钥。踩的坑:桩模型要在任意 user 消息里找暗号
  (人格提醒也是 user 角色、运行时戳是尾部 system),脚本只能放老布局 `data/scripts` 让 daemon 迁移
  (两处都放会撞「迁移目标已存在」拒启)。

### 第六轮(继续推进)

- `persona_reminder` / `emotion` 两个人格开关进 `feature_catalog`(引导页、成员人格页可勾),
  机器侧没开就不摆;`apply_selection` 写回清单;测试 `persona_reminder_and_emotion_toggles_round_trip`。
- 回合尾巴收成一处:`Agent::assemble_turn_tail`(可信传输上下文 → 输入提示 → 联想记忆 → 表情包提醒)
  按固定顺序收齐后一次性 `extend`,收集期间 `messages` 不变,去重判据不变;stream 测试全过。
- MCP 服务器也能声明能力:`mcp.servers[].capabilities`,拉起时注入同一套令牌,令牌随服务器进程生灭
  (`McpSession` 持守卫);未知 id 记 warn 丢弃(与脚本同规则)。

### 第七轮(用户批准「三件都做掉」)

- **`AgentMode` 从回合引擎退役**:`CoreTurnSnapshot.mode` 换成 `dev: bool`(是不是保留人格 `dev` 的会话),
  场所传进来的 `AgentMode` 只在 `Agent::new` / `switch_mode` 边界折成这一位;`mode_system_prompt` →
  `persona_system_prompt(dev)`,`with_host_environment(dev, voice, …)`,`runtime_context(platform)` 去掉
  从不读的 mode 参数,`replace_request_mode_context` → `replace_request_system_prompt`(丢掉两个只 `let _` 的参数),
  `with_mode_reminder` / `AgentMode::reminder`(恒 None 的空操作)删除。`Agent::mode()` 由 `dev` 折回,
  对外 IPC / 会话记录 / CLI 的 `normal|dev` 词汇不动(那是协议)。agent 内部非测试代码的 `AgentMode` 引用
  42 → 21(全在 control.rs 传输控制、`new`/`switch_mode`/`mode()` 边界与 `QueuedPromptsConsumed` 事件字段);
  全仓 339 → 315(其余是场所层的传输词汇)。中途切模式的语义不变:`switch_mode` 仍不重新作用域化 config。
  请求形状量尺 `dev-owner` / `normal-external` / `normal-internal` 三张脸与基线逐字节相同。
- **`VOICE_PROTOCOL` 接语音快照**:`with_host_environment` 多一位 `voice`(`subsystems.voice`),属主分支只在它为真时
  追加 `\n\n<voice-protocol>…`。量尺新增 `normal-owner-voice` 脸(`voice.enabled=true`):与 09-16 前的
  `normal-owner` 字节逐字相同;`normal-owner`(默认配置语音关)= 旧字节减去那一段(已用脚本核对相等)。
  **缓存影响**:语音关着的机器(默认配置)与 voice=false 的人格(成员私有人格默认)升级后第一轮各冷启动一次,
  之后每轮少 ~120 字;语音开着的机器零变化。`SUBSYSTEMS` 表 voice 加 `SystemPrompt` 阶段。
- **测试专用符号(104 项)清理**:用编译器裁决——先把 104 项整体删掉跑 `cargo check --tests`,300 条错误按封闭函数归类
  (167 条测试、23 个测试 helper),再逐项看函数体。裁决规则:
  - 独立死逻辑、测试就是冲它写的 → 函数与测试一起删(`edit_file`+`ensure_editable_file_path`、`edit_lines`+
    `refresh_semantic_after_write`、`archive_book`、`clone_filtered`、`register_chat`、`refresh_skills`、
    `InputLayout::full`,以及 load_tools 旧 lazy 档的动态描述整条链 `dynamic_description`/`script_summary_xml`/
    `load_targets_xml`/`loadable_tools`/`load_target_tool_xml`/`xml_escape`/`group_summary`);**11 条测试删除**。
  - 薄包装(3~8 行,只是给生产函数补一个 `None`/默认值)→ 测试调用点改成直接调生产函数,包装删除
    (`format_daemon_log_line`、`recent_daemon_log_lines`、`set_token_usage`、`rename_platform_provider_references`、
    `host_environment_block(_with)`、`admission_for`、`reset_if_prompt_changed`、`parse_patch`、`call_mcp_tool`、
    `list_server_tools`、`register_readonly`、`readable_subagent_log_line`);覆盖不丢。
  - 其余 **75 项是测试夹具**(`for_test_with_actor`、`ConversationDb::open`、`detached`、`bind_platform_session`、
    `builtin_registry`/`dev_registry`、`empty_parameters`、`add_platform_access_grant` 一类:测试拿它们摆状态,
    再测活代码),引用它们的 ~150 条测试测的是活逻辑,连测试一起删等于删覆盖,**保留为 `#[cfg(test)]`**
    (生产二进制零字节)。它们该搬进测试模块,那是整理不是删除,另开。
  合计:符号 29 删 / 75 留;测试 −12(11 条专用 + 1 条 `with_mode_reminder` 的);`lazy_definitions`(留作夹具)
  不再重建 load_tools 描述(没有测试断言它)。`.test-count` 由 refactor-check 自动写入,HEAD 基线 2337 仍远低于现值。
- **一条误报(已撤回)**:我曾写「人格白名单只写不读」。错了——白名单是直接读字段强制的:MCP 在
  `tools/compose_providers.rs` 把 `manifest.plugins.mcp` 传给 `mcp::register`(拉 tools/list 之前就过滤,关掉的
  服务器不拉起);脚本在 `tools/scripts/refresh.rs::prepare_script_refresh` 读 `plugins.scripts`;技能在
  `skills/mod.rs::skill_allowlist` 读 `plugins.skills`。`script_enabled`/`skill_enabled`/`mcp_server_enabled`+`allowed()`
  只是没人用的重复实现,已连同专用断言删除(测试里关于解析 / 落盘 / dev 的活断言保留)。误报原因:grep 结果被 `head` 截掉后半段,
  且 `.plugins\n.skills` 这种跨行字段访问单行 grep 匹配不到。

- **引导页加「MCP 服务器」一格**(用户追加拍板):`FeatureKind::Mcp` + `FeatureSources.mcp_servers`(机器级 `mcp.enabled`
  开着才列、只列 `servers[].enabled` 的,显示名空退回 id,说明行是命令);`apply_selection` 写回 `plugins.mcp`
  (全勾 None、关过一台写明细;表上没这一格时手写名单原样保留)。终端引导 `section_of` 多一组;成员人格页没动
  (成员能用哪些 MCP 服务器是管理员的事,清单对成员一律常开)。单测 `mcp_servers_get_per_server_toggles_that_write_the_allowlist`;
  真二进制探针 `testkit/oobe/mcp_probe.py`(pyte 驱动 `miyu oobe`,断言分组/显示名/退回 id/关着的不出现/Ctrl+A 全关/persona.toml 落 `mcp = []`)**7/7**;
  单测 `oobe::ui::feature_tests` 用真实 `App::new` + `load_features` 钉住机器级开关。探针第一次 1/7 是我的误判:40 行终端下 MCP 组在滚动视口外,
  表头计数 33 其实已含它(我把脚本数错成 24),追了三轮(重建、加调试落盘)才想到看视口;探针改成先 End 滚到底、默认 60 行。

**第七轮验证**:`cargo check --all-targets` 零警告;lib 全量 **2422 过 / 0 失败 / 32 ignored**(cgroup MemoryMax=20G);
`refactor-check.sh` 五道门禁全绿(用例数 2425,HEAD 基线 2337;规模门禁无新增越线;跨层引用无);
请求形状量尺 5 张脸:`dev-owner` / `normal-external` / `normal-internal` 与基线逐字节相同,`normal-owner-voice` 与旧 owner 字节相同,
`normal-owner`(语音关)= 旧字节减 `<voice-protocol>` 一段;`cargo build` 后 `testkit/host-query/run.py` **9/9**。
第一次门禁跑到 `onebot::tests::notices` 两条并行时序抖动(群禁言缓存是进程全局 `OnceLock`,本轮没碰,串行复跑 6/6),第二次整套全过。
第一次全量还撞出一条真回归:`scripts_default_to_lazy_and_index_always_loaded_forces_top_level` 断言 load_tools 描述含脚本名——那是被删的
lazy 档描述,改成断言生产 `stub_definitions` 里有该脚本桩。

### 第八轮(用户拍板「开工」:拆 crate 前置——夹具搬家 + 全局态隔离)

- **夹具搬家**:生产文件里散落的 `#[cfg(test)]` 条目(fn / const / static / struct / 整块 impl,110 项、63 个文件,
  其中 66 个是 impl 块里的方法)全部搬进同模块的 `test_support.rs`(`#[cfg(test)] mod test_support;`,
  `mod.rs` 的放同目录,`x.rs` 的放 `x/test_support.rs`)。impl 方法按原 impl 头重新包一层;私有 / `pub(super)`
  放宽成 `pub(crate)`(只在测试编译);原文件用 `use test_support::*` 再导出,可见性与条目对齐
  (`pub(in crate::agent)` 一类);条目里的 `super::` 改成 `super::super::`。搬完生产文件里 `#[cfg(test)]`
  只剩 `mod tests` / `mod test_support` 声明、测试用 `use`、7 处语句级测试缝(`thread_local!`/`task_local!` 一类,
  不是夹具,不动)。脚本 `move_fixtures.py`(scratchpad),盘点 `fixture_inventory.py`。
- **全局态测试隔离**:onebot 群禁言缓存是进程全局 `OnceLock<Mutex<_>>`,`tests/notices.rs` 碰它的三条测试
  加 `MUTE_CACHE_TESTS` 串行锁(中毒照拿)。`claude_code::haiku_…` 那条只用 tempdir,没找到共享态,未动。
- **约定(写进扩展指南)**:测试夹具住 `test_support.rs`,生产文件里不再出现 `#[cfg(test)] fn`;这是拆 crate 的前置——
  跨 crate 后 `#[cfg(test)]` 的私有夹具互相看不见。
- **更正一条我说错的话**:我曾说「依赖门禁已把层序钉死且证明无环,拆 crate 是机械活」。不对——原门禁只是一张
  **禁止对**表(llm 不许 use web 之类),顶层模块图实际有一个 28 个模块的强连通分量。现已给 `arch_dep_check.py`
  加完整层序 `TIERS`(基础 → 配置 → 存储与协议 → 子系统 → 传输 → 工具与引擎 → 场所与展示 → 入口,同层互引允许,
  低层引高层违规,新顶层模块必须登记),按「谁该知道谁」放层(render 在 agent 之上;runtime 暂按 DaemonState
  那一半放场所层)。现存违规 **12 条边 / 64 处**写进白名单只减不增,每条 reason 就是拆 crate 前要做的那件事:
  `tools::workspace` task-local 与 `sandbox` 下沉(llm/state/memory/host_info → tools 共 20 处)、runtime 拆成
  ports + daemon 两半(tools/llm → runtime 15 处)、tools ↔ render 互引(10 处,工具元数据下沉)、
  agent → platforms::file_reader(8 处,改端口)、DEV_PERSONA 搬进 config、trim_process_memory 下沉、
  汇率抓取下沉、clipboard 测试夹具改用本模块(6 处)。这 64 处烧完,拆 crate 才是机械活。

### 第九轮(烧逆向边;用户拍板机械段委派 Opus 子代理)

施工单 `docs/plan/2026-09-16-crate-split-burndown.md`(每批:目标 / 步骤 / 验收 / 回滚,换谁都能做)。

- **第一批(我做)**:`DEV_PERSONA` → config(state 再导出老路径);`trim_process_memory` → `src/process.rs`;
  `fetch_rate` → `ledger::rates`,`tools/http_response.rs` 随之下沉为 `src/http_response.rs`(工具层 `pub(crate) use` 老名字);
  `video_mime`/`pdf_mime` → `src/media_mime.rs`;`tool_event_base_name` → `tools::tool_descriptions`(render 再导出),
  `format_seconds` → `src/durations.rs`,`subagent.rs` 的 `render::readable_tool_name` 改直接调 tools 自己的。
  白名单 12 边 / 64 处 → 8 边 / 49 处;`cargo check --all-targets` 零警告;定向 125 测试过。检查点 `round10a-*`。
- **第二批(Opus 子代理,约 16 分钟)**:`src/tools/workspace.rs` → `src/workspace.rs`、`src/tools/sandbox/` → `src/sandbox/`(git mv),
  `OriginTty` 从 `ipc/protocol.rs` 搬进 workspace(ipc 再导出),全仓 60 个文件改路径(114+33+22 处)。
  白名单 8 边 49 处 → 5 边 33 处(state/memory/host_info → tools 归零;llm → tools 16 → 4,施工单估漏了三条测试里的
  `builtin_registry`,已并进第四批)。门禁全绿 2424 过,定向 426 过。子代理照实报了估算偏差和 `git mv` 暂存导致
  `git diff` 漏改名的坑——之后检查点一律 `git diff HEAD`。
- **第三批(Opus 子代理,约 19 分钟)**:`runtime/{ports,host_grants,host_query,live_turn}.rs` → `src/host_ports/`(配置层),
  tools/llm 15 处改路径,runtime 再导出老名字,四份接口文档路径同步。tools/llm → runtime 归零,白名单 5 边 33 处 → 3 边 18 处。
  门禁全绿、定向 133 过、端到端 9/9。子代理发现施工单漏了 `random_token`/`random_id`,暂放 host_ports 并主动给出基础层备选——采纳,并进第四批。
- **第四批(Opus 子代理,约 26 分钟)**:终端文本原语 11 项 → `terminal/text.rs`(施工单只估了 2 项,传递闭包大得多,子代理照实报了);
  `tool_subject`/`tool_peek` 全家 → `tools/tool_display.rs`;`is_command_tool`/`tool_event_base_name` → `src/tool_names.rs`;
  `random_*` → `src/random_id.rs`;三条中转线测试 → `tools/relay_tests.rs`。31 个搬动条目逐条 byte-identical 核验。tools ↔ render、llm → tools 归零。
- **第五批(我做)**:`agent::PlatformTurn` 端口(`platform_types::PlatformToolContext` 的子 trait),`TurnInput.platform_context`
  改成 `Arc<dyn PlatformTurn>`,平台层实现;agent 不再 use platforms(测试夹具两行除外)。量尺五张脸逐字节相同,307 测试过。
  **白名单 12 边 64 处 → 1 边 2 处(测试夹具)。**

## 5. 未做与待拍板

| # | 事项 | 建议 |
|---|---|---|
| D1 | **Phase 2 Agent 快照**(`CoreTurnSnapshot` / `TurnRuntime` / `PromptContext` 拆分 `agent/setup.rs` 等) | 单独专项。碰提示词与缓存,须先建两轮 cache-usage 测具与 `token_diet_baseline` 红测再动;不与本轮混提交 |
| D2 | **Phase 3 子系统生命周期**(memory / persona hint / emotion / voice / skills 的 descriptor + 挂接表) | 先做计划 §9.1 的「悬空开关」红测(`persona_reminder`、`emotion` 只在 manifest/UI 被读),再决定补 hook 还是迁配置 |
| D3 | ~~Phase 6 死代码~~(第四、七轮已做:全局豁免撤销、88+29 项删除、`AgentMode` 退出回合引擎、`agent → render` 下沉并进门禁) | 75 项测试夹具仍以 `#[cfg(test)]` 留在生产文件里,搬进测试模块另开 |
| D8 | ~~人格白名单只写不读~~(误报,已撤回:三条白名单都在装工具面时直接读字段强制,见第七轮末条) | `PersonaManifest::{script,skill,mcp_server}_enabled` + `allowed()` 无人用的重复实现已删(用户拍板);原测试里解析 / 全开不落盘 / dev 不带 MCP 三条活断言保留为 `allowlists_parse_and_stay_off_disk_when_all_on` |
| D4 | **Phase 7 PM 预检**字段(`miyu_min`、组件类型、契约版本、所需能力、文件清单哈希) | 契约版本字段现在一个都没有(见 `compatibility.md`),先给 scripts 头部与描述 JSON 定版本规则再谈预检 |
| D5 | `refactor_size_report` 基线:`src/render/tests/timeline.rs` 1517 → 1563 是 HEAD 已有的红 | 要么拆那份测试文件,要么用户拍板抬基线;本轮不动 |
| D6 | `AGENTS.md` 第 63 行仍写 `scripts/refactor-check.sh`(实际在 `test_scripts/`) | 属用户文件,只报不改 |
| D7 | MCP 待改项(计划 §8):工具 id 折叠碰撞未检测;MCP 工具缺省 `ReadOnly` 权限 | 已在 `mcp-client.md` 标为现状,归权限专项 |

## 6. 用户验收步骤

1. `python3 test_scripts/arch_dep_check.py` → 「无」。
2. `cargo test --lib -- runtime::ports runtime::ipc_events tools::platform_outreach tools::shape_tests` → 全绿。
3. 真机(隔离 MIYU_HOME 或本机 daemon 重启后):
   - REPL 里让她 `speak` 一句 → 有声;`miyu run`(非 daemon)里调 `speak` → 报「只能在 daemon 里用」;
   - 终端会话 `send_qq_message` 文本 + `voice: true` 各一次 → QQ 收到文字与语音;
   - QQ 发一条语音 → 正文出现 `[语音] …`(语音唤醒开着)或 `[语音消息]` 占位(关着);
   - shellhook 后台任务跟进回写 → 终端能看到思考/工具/正文(走的是搬家后的解码 + 渲染表)。
4. 语音协议:语音关着的机器 REPL 一轮 → 请求 system 里没有 `<voice-protocol>`;打开 `voice.enabled` 重启 → 有。
   (`MIYU_REQUEST_SHAPE_OUT=/tmp/s.json cargo test --lib request_shape_probe -- --ignored` 看 `normal-owner` 与 `normal-owner-voice`。)
5. 验收通过后再 commit;面向用户的三条(语音协议随开关、人格页两个开关、脚本/MCP 查宿主 + pm 预检)已写进
   worktree 的 `next-release-note.md`,合并时随分支带回主检出。

## 收尾审计(09-17):41 条 `#[ignore]` 测试

按 AGENTS §5.2「量尺类测试标 #[ignore]」逐条看过,全部有正当理由,**不动**:

| 类别 | 条数 | 例子 |
|---|---|---|
| 量尺 / 规模基准(断言耗时或只打印数字) | 17 | `keyword_search_scaling` `usage_stats_scales_with_history_size` `app_config_clone_cost` `token_estimate::benchmark_*` `keepalive_snapshot_cost` |
| 要真网络 / 真模型 / 真 key | 8 | `exa_free_quota_live_search` `live_provider_smoke_test` `synthesize_mimo_real_api` `collect_meme_records_origin_end_to_end` `live_cli_catalog` |
| 要本地语音模型或样本 | 7 | `voice::e2e_tests::*`(5 条)+ 唤醒词实验台 2 条 |
| 产物倾倒 / 手工看图 | 5 | `dump_preview_artifacts` `dump_stream_preview` `render_table_sample` `render_font_sample` `transfer_canvas_size` |
| 探针 / 夹具生成器(按需手动跑) | 4 | `request_shape_probe` `write_registry_shape_fixture` `token_diet_baseline_probe` `real_config_window_source` |

进程全局态的测试隔离:拆 crate 后全量并行跑两遍(2427/2427)没再出现 onebot notices 与 render::math 的抖动;
`render/blocks.rs` 的块标记开关在测试态是线程局部的(现在对上游 crate 的测试也生效了),`onebot::tests::notices` 有串行锁。
剩下已知的抖动只有 `claude_code::haiku_*`(根因未找到,两次都没红)。
