# 拆 crate 前置:逆向依赖烧尽施工单(2026-09-16)

`test_scripts/arch_dep_check.py` 的 `TIERS` 是 crate 边界草图;`test_scripts/arch-dep-waivers.json`
里每一条白名单就是一件待办。全部烧完之后,按层拆 crate 才是机械活。本文档是给执行者
(人或另一个模型)的施工单:每批一节,写明目标、步骤、验收、回滚。

## 硬约束(每批都适用)

- 工作树:`/home/shorin/Documents/github/Miyu/.claude/worktrees/core-normal-2026-09-16`,所有路径写绝对路径,
  不要 `cd` 到主检出,不要碰 8300 端口的线上 daemon。
- **cargo 一次只跑一个**,而且必须套内存上限:
  `systemd-run --user --scope -p MemoryMax=20G -p MemorySwapMax=0 cargo <…>`。
- 不 commit、不 stash、不 `cargo fix`(它会删掉只被测试用的 import)。只做本批写明的搬动;顺手看见的别的问题记下来,不动。
- 每批开工前先存检查点:`git diff > ~/.cache/miyu-refactor-2026-09-16/checkpoints/<批名>-pre-tracked.patch`,
  `git ls-files --others --exclude-standard | grep -v "^testkit/host-query/home\|^testkit/host-query/out" | tar czf ~/.cache/miyu-refactor-2026-09-16/checkpoints/<批名>-pre-untracked.tgz -T -`。
  回滚 = `git checkout -- . && git clean -fd src && git apply <patch> && tar xzf <tgz>`。
- 编译期的 heredoc / 复合命令可能被拦,批量文本改动写成 python 脚本放 `/tmp/claude-1000/…/scratchpad/` 再跑;
  锚点替换前先 `count == 1` 断言,找不到就停下报告,别猜。
- 验收三件套,缺一不可:
  1. `cargo check --all-targets` 零 error 零 warning;
  2. `python3 test_scripts/arch_dep_check.py` 通过,并把 `arch-dep-waivers.json` 里对应条目的 count 降到目标值
     (只改 count / 删条目,不改别的条目;删条目时把 reason 一并删);
  3. `bash test_scripts/refactor-check.sh` 五道门禁全绿(套 cgroup 跑;单测偶发抖动见下)。
- 已知的并行抖动(与本工作无关,复跑一次即可):`onebot::tests::notices`(已加串行锁)、
  `render::math`(字体缓存预热)、`claude_code::haiku_…`。第二次仍红才算问题。
- 交付物:改了哪些文件(按批列出)、门禁三件套的原始输出、白名单前后 count、检查点路径;
  哪一步没做成就照实写,不要报「大致完成」。

## 第一批(已完成,09-16):五条小边

DEV_PERSONA → `config`;`trim_process_memory` → `src/process.rs`;`fetch_rate` → `ledger::rates`
(`http_response` 随之下沉到 `src/http_response.rs`);`video_mime`/`pdf_mime` → `src/media_mime.rs`;
`tool_event_base_name` → `tools::tool_descriptions`、`format_seconds` → `src/durations.rs`。
白名单 12 边 / 64 处 → 8 边 / 51 处。

## 第二批(已完成,Opus 子代理):`workspace` 与 `sandbox` 下沉

**目标**:`llm → tools` 16 → 1(只剩 `is_command_tool`,留给第四批)、`state → tools` 1 → 0、
`memory → tools` 1 → 0、`host_info → tools` 2 → 0。

**为什么**:`tools/workspace.rs` 是回合作用域的 task-local(当前会话、工作目录、回合来源、origin tty),
`tools/sandbox/` 是 Landlock 策略与路径守卫。它们是**基础设施**,llm / state / memory 都要用,
住在工具层就逼着底层反过来认识工具层。

**步骤**:

1. 存检查点(批名 `round10b`)。
2. 移动文件:
   - `src/tools/workspace.rs` → `src/workspace.rs`;
   - `src/tools/sandbox/`(整个目录,含 `tests/`、`test_support.rs`)→ `src/sandbox/`。
   用 `git mv`,保留历史。
3. `src/lib.rs` 加 `mod sandbox;` 与 `mod workspace;`(按字母序插在对应位置)。
4. `src/tools/mod.rs`:`pub mod sandbox;` 改成 `pub(crate) use crate::sandbox;`,`pub mod workspace;` 改成
   `pub(crate) use crate::workspace;`——工具层内部 `super::workspace::…` / `crate::tools::workspace::…` 的老路径先靠这两条继续编译。
5. **全仓改路径**(门禁按字面 `crate::tools::workspace` 计数,只靠再导出不够):
   `crate::tools::workspace::` → `crate::workspace::`,`crate::tools::sandbox::` → `crate::sandbox::`,
   以及 `use crate::tools::{…, workspace, …}` / `use crate::tools::sandbox` 这类导入。工具层自己文件里的也一起改,
   改完若第 4 步的两条再导出变成 unused(编译器会报 warning),就删掉它们。
6. `OriginTty`:`src/ipc/protocol.rs` 里的 `pub struct OriginTty { path, shell_pid }`(连 doc 与 derive)搬进
   `src/workspace.rs`;`protocol.rs` 原处改成 `pub use crate::workspace::OriginTty;`;
   `src/workspace.rs` 里 `crate::ipc::OriginTty` 改成本模块名。这样 `workspace`(基础层)不再认识 `ipc`(传输层)。
7. 搬过去的文件里检查 `super::` / `crate::tools::`:`sandbox/mod.rs` 原第 81 行
   `crate::tools::workspace::effective_workdir()` 改成 `crate::workspace::effective_workdir()`;
   `sandbox/test_support.rs`、`sandbox/tests/*` 里若引用 `crate::tools::…` 的沙盒自身项,改成 `crate::sandbox::…`。
8. `test_scripts/arch_dep_check.py` 的 `TIERS` 第一层(基础)加 `"workspace", "sandbox"`。
9. `cargo fmt`;`cargo check --all-targets` 到零 error 零 warning。
10. `python3 test_scripts/arch_dep_check.py --warn` 看剩余边;把 `arch-dep-waivers.json` 里
    `llm->tools` 改成剩余 count(预期 1),删掉 `state->tools`、`memory->tools`、`host_info->tools` 三条;
    再跑不带 `--warn` 的门禁必须通过。
11. 定向测试:`cargo test --lib -- workspace sandbox llm::openai_compatible state::tests memory::tests host_info ipc`;
    然后整套 `bash test_scripts/refactor-check.sh`。
12. 存检查点(批名 `round10b-done`),按「交付物」格式报告。

**不许做的**:不改 `workspace.rs` / `sandbox` 的任何函数体与签名;不改测试断言;不动 `tools/mod.rs` 里别的声明。

## 第三批(已完成,Opus 子代理):`runtime` 的宿主端口下沉为 `host_ports`

**目标**:`tools → runtime` 14 → 0、`llm → runtime` 1 → 0。

**为什么**:`runtime/{ports,host_grants,host_query,live_turn}.rs` 是「下层拿上层能力」的窄接口,只依赖
config / paths / platform_types(`ports.rs` 头注释里的 `crate::web` / `crate::platforms` 只是注释),本该住在
工具层之下;`runtime` 剩下的 DaemonState / actor / run 那一半才是场所层。

**步骤**:

1. 存检查点(批名 `round10c`)。
2. 新建 `src/host_ports/`,`git mv` 四个文件进去:`src/runtime/ports.rs`、`host_grants.rs`、`host_query.rs`、`live_turn.rs`。
   写 `src/host_ports/mod.rs`:模块头注释说明「宿主能力端口:trait 在这里,实现由拥有能力的层在 daemon 启动时装入」,
   然后 `mod ports; mod host_grants; mod host_query; mod live_turn;` 与四条 `pub(crate) use xxx::*;`
   (`HOST_CAPABILITIES` 原来是 `pub`,保持 `pub use host_grants::HOST_CAPABILITIES;` 或让 `pub(crate) use` 覆盖后再单独 `pub use`)。
3. `src/lib.rs` 加 `mod host_ports;`(字母序)。`src/runtime/mod.rs` 删掉这四个 `mod` 与对应 `pub(crate) use`,
   加一行 `pub(crate) use crate::host_ports::*;`——web / pm 这些同层或更高层的调用方老路径 `crate::runtime::…` 不用改。
4. 改路径(门禁按字面计数):`src/tools/**` 与 `src/llm/**` 里所有 `crate::runtime::<符号>` 改成 `crate::host_ports::<符号>`,
   涉及符号:`voice_port` `qq_outreach_port` `qq_outreach_policy` `install_voice_port` `install_qq_outreach_port`
   `VoicePort` `QqOutreachPort` `QqOutreachPolicy` `issue_host_grant` `is_known_capability` `HostGrantGuard`
   `host_grant_capabilities` `enable_host_grants` `answer_host_query` `HostQueryError` `live_turn_host_tools_allowed`
   `LiveTurnHostToolsGuard` `HOST_CAPABILITIES`。改完 `grep -rn "crate::runtime" src/tools src/llm` 必须为空。
5. 搬过去的四个文件内部若有 `super::`/`crate::runtime::` 指向彼此的,改成 `crate::host_ports::` 或 `super::`。
6. 文档里的路径同步:`docs/interfaces/host-capabilities.md`、`docs/interfaces/README.md`、`docs/architecture.md` 里
   `runtime/ports.rs` `runtime/host_grants.rs` `runtime/host_query.rs` `runtime::…` 这些字样改成 `host_ports/…`;
   源码注释里(`src/tools/mcp.rs`、`src/tools/scripts/mod.rs`、`src/web/tests/ipc_bridge.rs`)提到的也一并改。
7. `test_scripts/arch_dep_check.py` 的 `TIERS` 第二层(配置)加 `"host_ports"`。
8. `cargo fmt`;`cargo check --all-targets` 到零 error 零 warning。
9. 白名单:删 `tools->runtime`、`llm->runtime` 两条;门禁必须通过。
10. 定向测试:`cargo test --lib -- host_ports runtime:: tools::platform_outreach tools::mcp tools::scripts web::tests::ipc_bridge`;
    然后整套 `bash test_scripts/refactor-check.sh`;再跑 `cargo build` 后 `python3 testkit/host-query/run.py`(端到端,9 项)。
11. 存检查点(批名 `round10c-done`),按「交付物」格式报告。

## 第四批(已完成,Opus 子代理):`tools ↔ render` 余下 6 处 + `llm → tools`

**目标**:`tools → render` 6 → 0、`llm → tools` 1 → 0。

**归属已定**:

- `clip_to_display_width`(`src/render/command.rs`)与 `strip_ansi_text`(`src/render/markdown.rs`)是终端文本工具,
  搬到 `src/terminal/text.rs`(新文件,`terminal/mod.rs` 里 `mod text; pub(crate) use text::*;`);
  render 原处改成 `pub(crate) use crate::terminal::{clip_to_display_width, strip_ansi_text};`(保持老路径),
  `src/tools/subagent.rs` 里改调 `crate::terminal::…`。
- `tool_subject` / `tool_peek` 及它们的私有帮手(`args_peek` `command_peek` `safe_inline_subject` `safe_url_subject`
  `string_arg` `image_basename` `read_page_label`,以及只被它们用的常量)是「工具调用的可读摘要」,归工具层:
  新建 `src/tools/tool_display.rs`,整体搬过去(`pub(crate)`),`src/tools/mod.rs` 加 `mod tool_display; pub(crate) use tool_display::*;`;
  `src/render/tool_display.rs` 原处改成 `pub(crate) use crate::tools::{tool_subject, tool_peek, …};`,
  `src/render/stream/timeline.rs` 里的 `command_peek` 若也搬了同样再导出。搬完 `grep -rn "crate::render" src/tools` 必须为空。
  搬动时**函数体一字不改**;它们用到的 `t()`(i18n)、`readable_tool_name`、`is_command_tool`、`tool_event_base_name` 在工具层都能拿到。
- `is_command_tool`(`src/tools/mod.rs`)与 `tool_event_base_name`(`src/tools/tool_descriptions.rs`)都是工具名的纯判定,
  搬到新基础模块 `src/tool_names.rs`(`lib.rs` 登记,`TIERS` 基础层加 `"tool_names"`),tools 原处 `pub(crate) use crate::tool_names::…;`
  保持老路径;`src/llm/openai_compatible/cli_relay/mod.rs` 改调 `crate::tool_names::is_command_tool`。
- `llm → tools` 还剩三条测试引用:`src/llm/openai_compatible/{antigravity,claude_code,codex}/mod.rs` 各自测试模块里的
  `every_deduplicated_name_is_a_real_tool` 调 `crate::tools::builtin_registry` 断言去重名单都是真工具。把这三条测试搬到
  `src/tools/relay_tests.rs`(新文件,`#[cfg(test)] mod relay_tests;` 挂在 `tools/mod.rs`),它们需要的去重名单常量若是私有的,
  在 llm 侧改成 `pub(crate)`;测试断言一字不改。
- 第三批把 `random_token` / `random_id` 两个函数暂放进了 `src/host_ports/mod.rs`(它们与宿主端口无关,只是 id 生成器):
  搬到新基础模块 `src/random_id.rs`(`lib.rs` 登记,`TIERS` 基础层加 `"random_id"`),函数体一字不改;
  `host_ports/mod.rs` 与 `runtime/mod.rs` 各留 `pub(crate) use crate::random_id::{random_id, random_token};` 保持老路径,
  `src/tools/vision/inline.rs` 改调 `crate::random_id::random_id`。
- 白名单:删 `tools->render`、`llm->tools`;`cargo check --all-targets` 零警告;定向测试
  `cargo test --lib -- render::tool_display render::stream tools::subagent llm::openai_compatible::cli_relay terminal`;
  整套 `refactor-check.sh`;检查点批名 `round10d`。

## 第五批(已完成,我做):`agent → platforms` 改端口

**目标**:`agent → platforms` 8 → 只剩测试里造真平台上下文的那几处(白名单 reason 改成「测试用真上下文,换假实现另开」)。

**设计**(沿用 `platform_types::PlatformToolContext` 那套「下层定义窄 trait、平台层实现」的端口模式):

- agent 对平台上下文只用四件事:是不是平台回合(`is_some()`)、平台名(`usage_source`)、
  `take_queued_files(prompt_id)`、`file_reader::register(registry, context, files)`;外加把它交给 `vision::register_scoped_platform`
  (那里已经收 `Arc<dyn PlatformToolContext>`)。
- 新建 `src/agent/platform_port.rs`:
  `pub(crate) trait PlatformTurn: PlatformToolContext { fn platform_name(&self) -> &str; fn take_queued_files(&self, prompt_id: &str) -> Vec<PlatformContextFileRef>; fn register_file_reader(self: Arc<Self>, registry: &mut ToolRegistry, files: Vec<PlatformContextFileRef>); }`
  (`ToolRegistry` 在工具层,所以 trait 只能住 agent,不能住 `platform_types`)。
- `TurnInput.platform_context: Option<Arc<dyn PlatformTurn>>`;`set_platform_context_images/files` 的参数同改;
  `usage_source` 改调 `platform_name()`;`parallel.rs` 改调 trait 的 `take_queued_files`;`setup.rs` 的
  `crate::platforms::file_reader::register(&mut tools, context, files)` 改成 `context.register_file_reader(&mut tools, files)`;
  `input.rs` 传给 vision 的 `Arc<dyn PlatformTurn>` 靠 trait 向上转型直接当 `Arc<dyn PlatformToolContext>` 用(rustc ≥ 1.86,本机 1.99)。
- `src/platforms/turn_context.rs` 加 `impl crate::agent::PlatformTurn for PlatformTurnContext`(平台层实现 agent 的 trait,方向向下):
  `platform_name` = `conversation.platform`,`take_queued_files` 已有,`register_file_reader` = `file_reader::register(registry, self, files)`。
- `src/agent/mod.rs` 的 `use crate::platforms::{PlatformContextFileRef, PlatformContextImageRef, PlatformTurnContext}` 改成
  从 `crate::platform_types` 拿两个附件类型,去掉 `PlatformTurnContext`。
- 测试:`agent/tests/{prompt,vision}.rs` 的 `use crate::platforms::{ConversationKind, PlatformConversation}` 改成 `crate::platform_types::…`;
  `shared.rs` 里 `OutboundMessage`/`PlatformAdapter` 同理(`SendReceipt` 看定义在哪);真 `PlatformTurnContext::new(...)` 与
  `PlatformPluginRegistry` 保留,计入白名单。
- 验收:请求形状量尺 `request_shape_probe` 五张脸逐字节相同(这批不该动任何字节);`cargo test --lib -- agent::tests platforms::tests web::tests`;整套门禁。
- 结果:`src/agent/platform_port.rs` 新增 `PlatformTurn` trait,`platforms/turn_context.rs` 实现;agent 生产代码对 platforms 归零,
  白名单只剩 `agent->platforms` 2 处(tests/shared.rs 两行夹具)。量尺五张脸逐字节相同;agent/platforms/web 307 测试过。

## 收官

五批烧完:白名单从 12 边 / 64 处降到 1 边 / 2 处(测试夹具),生产代码里再无低层引高层。
新基础模块:`process` `durations` `media_mime` `http_response` `workspace` `sandbox` `tool_names` `random_id` `terminal::text`;
配置层新增 `host_ports`;工具层新增 `tools/tool_display.rs`、`tools/relay_tests.rs`。下一步按层拆 crate。
