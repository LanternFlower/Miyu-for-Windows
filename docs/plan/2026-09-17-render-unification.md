# 2026-09-17 终端渲染层统一调研

> 纯调研，未改代码。工作树 `.claude/worktrees/tui-tags-2026-09-17`，分支 `feat/tui-tags-2026-09-17`，顶 `32227cc0`
>（调研进行中该提交落地，`timeline.rs` 由 2418 行变为 2403 行；下文行号与规模全部按 `32227cc0` 重新核过）。

## 施工进度（09-17 更新）

分支 `feat/render-golden-2026-09-17`（基 `32227cc0`）。

| 阶段 | 权重 | 状态 | 提交 |
|---|---|---|---|
| 0 冻结现状 | 15% | ✅ 外加耗时始终报、不到 1s 报 ms | `6a39d401` |
| 0 补漏 | — | ✅ 后台样本的日志标签写错了，冻的是不存在的格式 | `51ca10cf` |
| 1 前台面板搬出 | 10% | ✅ 由拆 crate 工作树 A1 做掉 | — |
| 2 命名能力位 | 20% | ✅ 顺手兑现 todolist:21「不自动收起过程」 | `3038ce2f` |
| 2 去重 A/B/C | — | ✅ 命令两份内容 / 思考配色 / 提示词图标 | `770bc476` |
| 2 去重 E + §5 命名债 | — | ✅ 问答文案一处出；选面改叫名字；六个面的表 | `59bf70c8` |
| **3 后台面板收敛** | **35%** | ✅ 完成 | |
| 附带 todolist:11 | — | ✅ 两个开关都不再走旧 inline；S5 那个混合态没了；前台浮层也跟随（后台面板要等组装器） | `0e107c8a` `b61a895d` `1af272f5` |
| 附带 阶段 1 的一小片 | — | ✅ 图标那张表搬出 `timeline.rs`（2435 → 2315） | `1af272f5` |
| 附带 集成走查 | — | ✅ 真 PTY 两份走查全绿；修掉五条陈旧断言 | `cd9b4218` |
| └ 协议拆出（§6.1 路 A 第一步） | | ✅ `LogEvent` + `parse_log_line`，三处漂移变成三行 | `1f7db567` |
| └ 编码器/解码器对测 | | ✅ 这条链原来一个测试都没有 | `847e4bf4` |
| └ 标记也解成 `LogEvent`（`from_marker`）+ 两条路等价钉死 | | ✅ 抬头拼法从两份收成一份 | `2f947575` |
| └ 顺手：`tool_calls=full` 时标记原样打屏 | | ✅ 查词汇表时撞出来的真 bug | `4f11f1e8` |
| └ 步的类型改由事件决定，不看图标猜 | | ✅ 又撞出一个真 bug（装工具那一步出现两遍） | `0e92575b` |
| └ §6.3 第 3 项拆成「bug」与「拍板」两半，bug 那半做掉 | | ✅ 再撞出两处豆腐块 | `2ec1b41d` |
| └ §6.3 第 2 项：`[统计]` 不再算 tool | | ✅ 当 bug 做的，理由在提交里 | `6aa2ddce` |
| └ todolist:11 上半条：详细档的思考不再绕过时间线 | | ✅ 七处 `== Summary` 收成一个具名谓词 | `0e107c8a` |
| └ 更正：「不自动收起」那一档的步骤其实**点不开** | | ✅ 阶段 2 我写反了，已钉成测试 | `8553f697` |
| └ 一个组装器 · 排版规矩（§2.2 项 D） | | ✅ 两份状态机合成一份，golden 逐字节不变 | `42e44b38` |
| └ 一个组装器 · 步的类型词汇（`StepKind` 三处合一） | | ✅ 只动词汇不动行为，golden 逐字节 | `b59d5da6` |
| └ 一个组装器 · 收缩段（主线 ⊕ 前台面板） | | ✅ 见阶段 5 | `af95161e` |
| └ 一个组装器 · 收缩行点开的样子（三处 → 一处） | | ✅ `fold_open_lines` | 本次 |
| └ 一个组装器 · 步的模型（`LogStep` → `Step`）+ 后台那份收缩 | | ✅ 四份收敛到位，`LogStep` 退成纯解码产物 | `74af8425` `5d9ff1b7` |
| └ 整文件删 `overlay/log.rs` | | ⛔ 同上 | |
| └ §6.3 剩下的拍板 | | ⬜ 只剩第 1 / 3(b) / 5 项等用户；每项已收敛成一行 | |
| 4 后台改订事件流 | 12% | ✅ 用户 09-17 拍板**做**；两条进料口一个事件类型，日志留作退路 | `2757ae08` |
| 5 主线收缩共用 | 8% | ✅ **实测后判成「该做」**：两份规则一样，而面板那份还漏着尾巴与块 id 复用 | `af95161e` |

**整体 ≈ 97%**（15 + 10 + 20 + 35 + 12×0.8 + 8 = 97.6；阶段 4「建议不做且主要收益已另行拿到」按 8 成计。按「不做就是没做」算则是 88%）。

**六个阶段全部完成**（用户 09-17 拍板：§6.3 ① 都不带窥视 / ③b 统一成芯片 / ⑤ 先不动上限，改开 todo「面板按视口渲染」；阶段 4 做）。

### §6.3 第 5 项（步数上限）有实测数据了

后台面板按 256KB 字节截、前台按 400 步截。量一下 256KB 到底是多少步、画一帧多久
（debug 档，一次性探针，未入库）：

| 日志 | 轮数（每轮 ≈ 2 步） | 排出的行 | 首帧 |
|---|---|---|---|
| 16 KB | 73 | 293 | 354 ms |
| 32 KB | 144 | 577 | 671 ms |
| 64 KB | 286 | 1145 | 1.35 s |
| 128 KB | 570 | 2281 | 2.70 s |
| 256 KB | 1135 | 4535 | **5.39 s** |

**线性**（16× 数据 → 15× 时间），约 4.7ms/轮。而面板每 150ms 刷一次。

两点要说清楚：

- 这是 **debug 档**，release 快得多（这份数据只用来比两个上限的**相对**代价）。
  400 步 ≈ 200 轮 ≈ 0.95s，比 256KB 省 5.7 倍。
- 我**没能**验证「块登记处（`MAX_LINES = 4000`）装不下 4535 行，早期的步会被挤掉、
  点不开」这个猜想——取块 id 的路子没走通。它只是个猜想，别当结论。

所以第 5 项不只是"面板里留几步"的口味问题，还带着一条线性代价。要真治，得让面板
只渲染**可见的那几行**（今天是整份都渲染），那是另一件事。

阶段 3 拆成两半之后，**上半段不需要拍板也不需要等拆 crate，已经做完**：

- 「这一行说了什么」从「这一行长什么样」里拆出来（`LogEvent` + `parse_log_line`）
- 两个入口（日志行 / 进度标记）解出同一个事件，并有测试钉死它们一样
- 三处漂移收敛成组装器里三个具名常量，§6.3 第 3 项拍完改三行就够
- 写日志与读日志、抬头的拼法，各自从两份收成一份
- 步的**类型**改由事件决定（`StepKind`），不再看图标猜

### 上半段撞出来的三个真 bug

把「长相」从「事实」里拆出来的过程中，掉出三个此前没人发现的缺陷——都不是重构
引入的，是**原来就在、被那层混淆盖住的**：

| bug | 根因 | 影响 |
|---|---|---|
| 阶段 0 的后台样本冻住了一份不存在的日志格式 | 手写样本把渲染出来的抬头当成了日志标签 | 安全网是假的 |
| `tool_calls=full` 时 `__subagent_brief__` 原样打在屏幕上 | 认不出的标记掉进「按人话原样打出来」的兜底 | 用户吃一行 JSON |
| 子代理装工具那一步在面板里出现两遍、输出丢失 | 「是不是工具调用」是拿**图标**比出来的，而 `load_tools` 的图标与 `PROMPT_GLYPH` 撞了 | 天天走的路 |
| `MIYU_TUI_ASCII=1` 下面板里的图标一半是豆腐块 | 「已思考 / 跑砸了 / 认不出的工具」硬编码 Nerd 码位，漏了退路 | 没装 Nerd Font 的人 |
| 「提示词」那一行在**三个地方**都是豆腐块 | `PROMPT_GLYPH` 是常量，主线与两个面板共用同一份，一起漏 | 同上 |

五个都各有一条会红的测试钉着，且钉的是**整类**（词汇表逐个点名、类型由事件决定、
一块面板里的图标不许混模式）而不是漏过的那一个。第五个就是第四个那条测试**自己
抓到的**——写完跑另一种模式，当场红在一个我没想到的地方。

**这五个都不是重构引入的。** 它们是「长相」与「事实」搅在一起时的必然产物：类型
靠图标猜、格式靠人记、退路靠一处一处补。拆开之后，同类问题要么变成编译期的事，
要么变成一条会红的测试。

下半段（一个组装器）才是真卡着的：

### 最后一块做完了

原来判的三重阻塞（§6.3 第 1 项、「整份重解析 vs 增量」、合并顺序）里，前两条都
不成立——记录在此，因为我判错过五次，下一个人不该再判错第六次：

- **不卡 §6.3 第 1 项**：抬头是个字符串，谁拼都行。分歧留在拼抬头那一侧
  （后台的 `Screen::step_line`）就够，模型只存那个字符串。它现在是那儿的一个
  `if`，注释写着这是拍板点。
- **不卡「整份重解析 vs 增量」**：那是「什么时候渲染」的差别，不是「渲染成什么」
  的差别。后台每帧重解析、在 `to_step` 里渲染；渲染完之后落进同一个 `Step`，
  排版、收缩、点开就都共用了。块 id 复用照旧按位置。

原文（判错的那版）留在下面备查。

### 原判：它到底卡在什么上

**不卡 §6.3 第 1 项。** 这话我说过两遍，是错的：抬头是个字符串，谁拼都行——
「已结算思考那一行带不带窥视」的分歧留在**拼抬头**那一侧（后台的 `step_head`）
就够，模型只存那个字符串。

**也不卡「整份重解析 vs 增量」。** 后台按 `(收缩行位置, 步位置)` 复用块 id，把这
份映射在转换时喂进 `Step.block` 即可；`fold_block_lines` 只要补一条「已登记的就
`blocks::update`」。

核对下来，两边的详情构造**本来就逐行相同**：

| 后台 | 主线／前台 | 结论 |
|---|---|---|
| `log_step_detail` 末尾的 `panel_step_detail(line, body)` | `step_detail` 非 fold 分支 | 同一个函数，`panel_step_detail == [line] + indented_body(body)` |
| `fold_detail` 末尾四行 | `step_detail` 的 fold 分支 | **已合**（`fold_open_lines`） |
| `fold_detail` 的逐步登记块 | `fold_block_lines` | 差一条 `blocks::update` |

所以剩下的是一件**机械活**：`log_steps` 的产物在渲染时转成 `Step`（抬头、正文
各自照旧拼，只是拼完存进 `Step`），`render_log` 改成 `to_steps → PanelEntry →
thread_panel`，然后删掉 `LogStep` / `fold_detail` / `log_step_detail`。约 250 行。

**真正挡着的只有合并顺序**：它整个落在 `overlay.rs`，而拆 crate 工作树正把这个
文件切成四份、且已在同一个 WIP 提交上停了很久（93 个文件未提交）。这一步做下去，
那边的 `log.rs` 会变成「这个文件不该存在」。那是用户的一个决定，不是技术障碍。

### 阶段 4 的账被实测改写了

报告 §6.1 把「后台面板不够流式」记在 150ms 文件轮询上，据此提出路 B（加一条 IPC
`Command::JobTrace`）。新走查 `testkit/tui/bg_latency.py` 量出来的是：

| | 改前 | 改后 |
|---|---|---|
| 思考条数 | 4 | 40 |
| 每条字符 | 20 / 20 / **612** / 207 | ~32 |
| 最长一次憋多久 | **12,045 ms** | 346 ms |

12 秒里轮询最多占 150ms——**主项是 `accumulate_stream` 那道「攒够 600 字符才落一
条」的闸**。加一道 300ms 的时间闸（十行）就拿到了真流式。路 B 剩下的收益只有
「去掉轮询」，按报告自己写的不做条件，那不值一条新 IPC。

---

阶段 3 的第二重阻塞是实打实的：拆 crate 工作树已把 `overlay.rs`（1428 行）切成
`overlay/{mod 508, log 501, screen 390, test_support 47}.rs`，而阶段 3 要**整文件
删掉** `overlay/log.rs`。在合并前的布局里做，等于把那 500 行的冲突再造一遍。

阶段 0 的 golden 已经把当前四个面逐字节冻住，是为那一刻准备的安全网：534 个文件
搬完之后先跑它，再开阶段 3。

---

## 0. 结论先行

| 问 | 答 |
|---|---|
| 能不能统一 | **能，但不是"一个组装器管全部"**。可行且值得做的是：一份与表现无关的**子代理过程模型**（前台/后台面板共用）+ 一组**命名的 surface 能力位**替换今天散落的四个模式位。主线时间线的 live 状态**不搬**。 |
| shellhook 与 inline 要不要合并 | **它们已经是一个东西**。渲染层零分支；宿主层共用 `try_run_remote_chat`，唯一差别是 `live: Option<&mut LiveReplTail>`（有没有活动区）。要做的只是改名和文档，外加把 `job_wake.rs:333` 那处"强设 `live_summary = true`"换成显式构造。 |
| 前台/后台面板能不能收敛 | **能，且是本次收益最大的一项**。两边已经共用画行原语，分叉在"从事件攒步"和"从日志行攒步"两套状态机。方案：一个 `Process` 模型 + 两个解码器（进度标记 → 事件；日志行 → 事件）+ 一个组装器。已发现三处两边**实际漂移**（见 §2.3）。 |
| 分几阶段 | 6 段（0 准备、1 搬家、2 能力位、3 面板收敛、4 后台改订事件流、5 主线收缩共用）。**阶段 1 已被拆 crate 工作树的 A1 做掉**（§10.1），实际要做的是 0→2→3，5 列为"实测后判"，4 需拍板。 |
| 与拆 crate 的先后 | **先合拆 crate，再做渲染统一**（§10.3）。拆 crate 已验证（2427 测试 / 探针逐字节 / 五道门禁），且它对 main 的合并债只有 ~8 个文件；反过来做会让它的 A1/A4/A6 三份拆分与 534 文件的搬迁全部重来。本方案的每一步都是 `hosts` 内部或 `hosts → engine` 向下依赖，**不需要端口**（§10.2）。 |
| 最大风险 | 阶段 3 会把两个面板之间**用户实测出来的差异**抹平——哪一边是对的必须逐条拍板（§6.3 差异表），不能以统一之名默认取其一。其次是合并顺序：main 在拆 crate 基线之后又动了 `timeline.rs / tool_summary.rs / command.rs / patch.rs / tests/timeline.rs`，其中 `command_peek` 的语义改动要手工搬进 engine 侧（§10.3）。 |

---

## 1. 现状：到底有几条"输出路径"

用户数的是五条：shellhook、inline、TUI、前台子代理浮层、后台子代理浮层。按代码里的判定谓词重新数，主线其实是**六种 surface**，浮层是**两个组装器**。

### 1.1 主线 surface 由四个模式位决定

`StreamRenderer` 的四个位（`src/render/stream/mod.rs:68-92`）：

- `plain: bool` — `--plain`
- `live_summary: bool` — 构造时 `io::stdout().is_terminal()`（`mod.rs:163`），daemon 回写时被硬改为 true（`src/web/actor/job_wake.rs:333`）
- `tool_call_mode: Hidden | Summary | Full` — 配置 `display.tool_calls`
- 全局 `blocks::enabled()` — 全屏后端进出场时切（`src/cli/repl/tail/screen/mod.rs:272` / `:990`），不是渲染器字段

两个派生谓词（`src/render/stream/timeline.rs:1107-1122`）：

```rust
timeline_enabled() = blocks::enabled() || timeline_static()
timeline_static()  = !blocks::enabled() && live_summary && !plain && tool_call_mode == Summary
```

组合出来的 surface：

| # | 名称 | plain | stdout 是终端 | 全屏 | tool_calls | 过程怎么画 | 宿主 |
|---|---|---|---|---|---|---|---|
| S0 | Plain | 1 | – | 0 | 强制 Hidden | 只有正文 | `--plain` |
| S1 | Pipe | 0 | 0 | 0 | Summary | 老的一行摘要 `~ 工具×1 ok`，无转轮 | stdout 接管道（`static_timeline.rs:470 piped_output_keeps_the_plain_summary`） |
| S2 | Cards | 0 | 1 | 0 | Full | `工具 x / 参数 / 结果` 整块卡片，无时间线 | 终端 + 配置 `tool_calls=full` |
| **S3** | **Static** | 0 | 1 | 0 | Summary | **静态时间线**：每步落 scrollback、无块、无 `Worked for` | shellhook（`src/cli/shell_bridge.rs:178`）、单次 `miyu "…"`、inline REPL（`MIYU_TUI=0`）、REPL 跟进后台唤醒（`src/cli/repl/wake.rs:42`）、daemon 回写原终端（`job_wake.rs:325`） |
| **S4** | **Full** | 0 | 1 | 1 | Summary | 可展开时间线 + `Worked for` 收缩 | 全屏 TUI（默认，`tail/screen/preference.rs`） |
| S5 | Full+Cards | 0 | 1 | 1 | Full | **混合**：命令走时间线（`tool_summary.rs:34`），其余工具打旧卡片（`tool_summary.rs:58-63`） | 全屏 + `tool_calls=full`——即 `todolist.md:11` 用户报的"用的旧版本的 inline" |

S5 不是设计出来的，是谓词组合的副产品：`timeline_enabled()` 为真但 `write_tool_call` 的非命令分支只认 `Summary`。这是"四个位派生谓词"这条路的典型病灶，也是 §4 里改成命名能力位的直接理由。

### 1.2 浮层：两个组装器，一套画行原语

| | 前台子代理 | 后台子代理 |
|---|---|---|
| 数据源 | 进度标记 `__subtool_*` / `__subagent_*`（`tool_summary.rs:395-552`） | 同一批标记经 `spawn_subagent_log_bridge` 写成 `[思考]/[工具]/[结果]/[输出]/[准备]/[统计]/[提示]/[正文]` 行落盘（`src/tools/subagent.rs:441-527, 618-740`） |
| 状态机 | `SubagentLog`（`timeline.rs:677-711`），事件驱动、增量 | `LogStep`（`overlay.rs:516-537`），每 150ms 重读 256KB 尾巴、整段重解析（`overlay.rs:39-42, 200-226`） |
| 组装 | `subagent_lines` + `thread_panel` + `PanelEntry`（`timeline.rs:513-671`） | `render_log` + `Previous` 枚举（`overlay.rs:234-400`） |
| 收缩 | `collapse_subagent_segment`（`timeline.rs:416-454`） | `collapse_log_segment`（`overlay.rs:572-623`） |
| 收缩行点开 | `step_detail` 的 `fold` 分支（`timeline.rs:975-983`） | `fold_detail`（`overlay.rs:404-449`） |
| 思考步 | `flush_subagent_thought`（`timeline.rs:323-349`） | `log_steps` 的 `[思考]` 合并 + `step_head`（`overlay.rs:697-719, 905-920`） |
| 画行 | `panel_step_line` / `panel_live_step_line` / `panel_rail` / `panel_step_detail` / `render_speech_lines` / `summary_line` / `fold_line_open` / `peek_tail`（`timeline.rs`，全部 `pub(crate)`） | **同一份**（`overlay.rs:236-239, 305, 364-366, 409-417, 444-445, 605`） |
| 容器 | `Overlay::from_block`（`overlay.rs:115`） | `Overlay::from_file`（`overlay.rs:136`） |

体量：前台那套在 `timeline.rs` 里占 **426 + 305 ≈ 730 行**（`:286-711` 与 `:1887-2191`）；后台那套在 `overlay.rs` 里占 **222 + 471 ≈ 693 行**（`:228-449` 与 `:507-977`）。两套加起来 1400 行做的是同一件事。

### 1.3 "统一"的天花板在哪：输入侧早就统一了

- 直连路径：`AgentEvent`（`src/agent/mod.rs:80`）→ `handle_agent_event`（`src/cli/mod.rs:1350`）→ `StreamRenderer::write_*`。
- daemon 路径：IPC `Frame::Event{kind,data}` 在 `one_shot.rs:399-480` 被**映射回 `AgentEvent`** 再调同一个 `handle_agent_event`。
- 回放：`ReplayEntry`（`src/state/conversation_db/types.rs:37`）在 `cli/mod.rs:940-1010` 被喂进同一个渲染器。

所以分叉不在事件层、不在宿主层，**全部在 `StreamRenderer` 内部**——这决定了统一工程的边界：只动 `src/render/stream/` 与 `overlay.rs` 的组装部分。这也和 `docs/plan/2026-09-11-tui-implementation.md §5`「只换输出层，控制流不动」的既定裁决一致。

拆 crate 工作树把这一点做得更彻底：`handle_agent_event` 那张表已下沉为 `crates/miyu-hosts/src/render/agent_events.rs::apply_agent_event`，IPC 解码收敛为 `crates/miyu-hosts/src/runtime/ipc_events.rs`（原来 `one_shot.rs` 与 `wake.rs` 各抄一份、互相漏变体）。统一后的过程模型从这两个入口之后开始，正好是它们唯一的下游。

---

## 2. 事实核对

### 2.1 用户列的六条

| # | 说法 | 核对 | 证据 |
|---|---|---|---|
| 1 | 渲染层没有 shellhook/inline 之分，区别在宿主 | **成立** | `grep 'shell_intercept\|shellhook' src/render/` 只命中注释（`wait_spinner.rs:145,436`、`command.rs:429`、`timeline.rs:23`）。宿主侧：shellhook `run_shell_intercept` → `run_chat_with_options`（`direct.rs:159`）→ `try_run_remote_chat(live=None)`（`direct.rs:180`）；inline REPL `try_run_remote_chat(live=Some)`（`interactive.rs:1247`）。同一个循环，`live` 只决定输出走 `RenderOutput::Terminal` 还是 `Buffered` + `apply_renderer_frame`。 |
| 2 | 四个模式位被问的次数 | **成立**（非测试代码）：`timeline_enabled()` 35、`self.plain` 20、`blocks::enabled()` 15、`timeline_static()` 9、`live_summary` 9 | 见附录 A 逐处清单 |
| 3 | 画一行原语已共用，后台面板注释留着用户抱怨 | **成立** | `overlay.rs:359-361`「后台子代理和前台子代理应该是一回事啊」；`timeline.rs:1060-1091` 六个 `panel_*` 原语的注释「取数的地方不同，长相必须是同一份代码」 |
| 4 | 三个组装器 | **成立**，且比说的更碎：`Worked for` 收缩有**三份**实现，思考步有**三份**实现 | §1.2 表；主线 `cut_timeline`（`timeline.rs:2253-2323`）、`timeline_push_thought`（`:1316-1355`） |
| 5 | 文件规模 | **成立**（调研中途 `32227cc0` 落地后 2418 → 2403）；**门禁余量只剩 7 行** | 基线 `test_scripts/refactor-size-baseline.json`：`timeline.rs` 2400 → 现 2403（容差 10；且 2400 > 上限 1500，触发"超标文件不得变长"）。`overlay.rs` 1409 → 1427，但 1409 < 1500，不受该条约束。 |
| 6 | 交互能力差别落在 `blocks`；`Worked for` 只在全屏 | **成立** | `cut_timeline` 静态分支 `timeline.rs:2264-2275` 只落步 + 空行；`blocks::register` 在 `enabled()` 为假时返回 `None`（`blocks.rs:84-89`） |

### 2.2 补充发现：还有哪些是"同一件事写了几遍"

| 项 | 位置 | 说明 |
|---|---|---|
| A | `(detail, tail)` 三选一策略 | `mod.rs:553-557`（`interrupt_command_display`）与 `tool_summary.rs:141-153`（`write_tool_result`）逐字重复：`if static {(rows, [])} else {(timeline_detail, rows)}` |
| B | 思考正文配色 `\x1b[2m\x1b[38;5;10m` | `timeline.rs:335, 641, 1341, 1515`、`overlay.rs:950` 五处 |
| C | `PROMPT_GLYPH` | `timeline.rs:314` 与 `overlay.rs:560` 各一份，互相用注释指向对方 |
| D | 步与步之间连线 / 正文段前后空行的规则 | `thread_panel`（`timeline.rs:520-552`，`PanelEntry` 三态）与 `render_log` 循环（`overlay.rs:289-319`，`Previous` 四态）——同一套规矩两份状态机 |
| E | 「已回答 N 个问题 / 标题：答案」文案 | 静态版 `timeline.rs:1787-1816`（进步的正文）与全屏 `write_question_exchange`（`:1837-1883`，独立竖条块） |
| F | 转轮占位符 | 主线 `wait_spinner::BLOCK_MARKER`（`\u{1}`，由 `WaitSpinner` 每 tick 换帧）；面板 `LIVE_SPINNER_CELL`（`\u{10FFFD}`，由 `paint_overlay` 换帧，`overlay.rs:1297`）。两个动画驱动不同，是**本质差异**，但今天没有任何类型把这层意思说出来 |

### 2.3 补充发现：两个面板**已经**漂移的地方

这些不是猜测，是当前代码两边就不一样：

| 项 | 前台 | 后台 | 哪边像是用户裁定 |
|---|---|---|---|
| 图标退路 | 走 `nerd()` 开关（`timeline.rs:66-68`），`MIYU_TUI_ASCII=1` 退到 `⚙ ✗ ✳` | **硬编码** `\u{f013}`（还是已经被 `glyph_tool()` 换掉的旧齿轮）、`\u{f0768}`、`\u{f00d}`（`overlay.rs:555, 712, 816, 830`），不认 `MIYU_TUI_ASCII` | 前台（`timeline.rs:64-65` 注释是用户要求） |
| 已结算的思考步抬头 | `已思考 · 1.2s`，无窥视（`timeline.rs:340-346`） | `已思考 · 1.2s · …想到哪儿了`，取尾巴窥视（`overlay.rs:344-354`，注释记着用户实测「思考的窥视刷新似乎不太对」） | 后台（有用户实测记录） |
| `Worked for` 里的 tools 计数 | 只数 `subagent_tool` 落下的步（`timeline.rs:2025`） | 数 `!thinking && !speech && glyph != SUMMARY`（`overlay.rs:590-594`），`[统计]` 那一步也算进去 | 前台（`[统计]` 不是工具调用，`overlay.rs:565-567` 自己也这么说） |
| 运行中那一行 | `名字 · 运行中 · 秒数 · 窥视`（`timeline.rs:605-615`） | `名字 · 窥视 · 运行中`，无秒数（日志没有时间戳，`overlay.rs:331-335`） | 本质差异（数据源没有那个数） |
| 步数上限 | 400（`SUBAGENT_LOG_STEPS`） | 按 256KB 字节 | 无裁定 |

这张表本身就是"两个组装器必然漂移"的实证，也是阶段 3 的拍板清单。

### 2.4 `todolist.md` 里三条待办直接压在这个结构上

- `:11`「显示思考过程／显示工具调用信息 两个开关……子代理浮层里的也要跟随」→ 配置位得能传进面板组装器。今天面板组装只看 `blocks::enabled()`，没有入口。
- `:17,19`「把思考／命令从 Worked for 中分离」→ 收段规则要可配置。今天收段点散在 `write_chunk:254`、`prepare_for_external_output:316`、`finish:429`、`settle_tool_batch:286-288` 四处。
- `:21`「新增配置让 TUI 不自动收起 Worked for，做到和 shellhook 差不多」→ 这是**今天的谓词表达不出来的组合**：要"可展开（blocks）+ 步跑完就落地（今天只有 static 这么做）+ 不收缩"。`timeline_static()` 把这三件事捆成一个布尔。§4 的能力位就是为它准备的。

---

## 3. 分叉分类：本质 vs 历史包袱

| 分叉 | 类别 | 理由 |
|---|---|---|
| 能不能点开（blocks） | **本质** | 没有 alt screen 就没有点击与回翻 |
| 步跑完是立刻落 scrollback 还是留在 live 区到收缩 | **本质**（但要从 static 里解绑） | 没有回翻时 live 区超过一屏擦不干净（`timeline.rs:1268-1272`）；全屏可以等收缩一起写。**但**「全屏 + 立刻落地」是合法组合（todolist:21），所以它是独立能力位，不是 static 的同义词 |
| 有没有 `Worked for` 收缩行 | **本质→配置** | 点不开的把手是废话（用户裁定）；可点开也可以选择不收（todolist:21） |
| 详情就地印 vs 藏在块后面；印多少 | **本质** | 静态版印命令本身不印输出（`command.rs:401-435`，09-17 用户裁定），全屏尾巴留命令、点开是命令+输出 |
| 面板前置「提示词」抬头 | **不是 surface 差异，是过程差异** | 子代理有差事、主线没有。它该是模型字段 `header: Option<Step>`，不是能力位（`timeline.rs:596-601`、`overlay.rs:282-284` 两边都把它当"不进时间线的抬头"） |
| 谁驱动转轮动画 | **本质** | 主线由 `WaitSpinner` 重画 live 区；面板内容是静态 ANSI，由 `paint_overlay` 替换占位格 |
| 有没有输入框/活动区 | **本质，但在宿主层** | 渲染器完全不知情，已经隔离干净 |
| shellhook vs inline | **历史命名** | 见 §5 |
| 前台面板 vs 后台面板 两套状态机 | **历史包袱** | 后台读日志的理由是「省掉一个专门的 IPC 往返」（`src/tools/jobs/mod.rs:177-179`）；原始事件流其实已经存在（§6.1） |
| 三份 `Worked for`、三份思考步、两份连线规则 | **历史包袱** | 逐个功能补出来的 |
| `job_wake.rs:333` 硬改 `live_summary` | **历史包袱** | 用字段冒充"stdout 是终端"来触发静态版 |
| S5 混合形态 | **历史包袱** | 谓词组合副产品 |

---

## 4. 目标架构

原则：**模型与表现分离；能力位命名而不派生；主线 live 状态不搬**。

### 4.1 文件与类型

按**拆 crate 之后**的布局写（理由见 §10.3；main 布局下的对应物在括号里）：

```
crates/miyu-engine/src/tools/subagent/
├── log.rs              编码器（已有：readable_subagent_log_line_timed / subtool_summary / prompt_header_line）
└── protocol.rs         [新] SubagentEvent 线型 + from_marker / to_marker / from_log_line（编解码成对，round-trip 测试同文件）
crates/miyu-base/src/durations.rs      + parse_seconds（format_seconds 的逆）

crates/miyu-hosts/src/render/stream/
├── mod.rs              StreamRenderer（字段照旧）
├── surface.rs          [新] SurfaceCaps —— 命名能力位
├── timeline/mod.rs     主线核心：Step / Timeline / push_* / settle / cut（A1 已拆，781 行）
├── timeline/live.rs    live 区 + 共用自由函数 render_speech_lines / summary_line / peek_tail / indent_body（623）
├── timeline/subagent.rs  Process(=今天的 SubagentLog) + panel_lines / fold_segment（A1 已拆，691；即原方案的 subagent_panel.rs）
├── timeline/question.rs / glyphs.rs（166 / 136）
├── tool_summary.rs     write_tool_* 入口（标记解析改调 protocol::from_marker）
└── reasoning_phase.rs  （不变）

src/cli/repl/tail/screen/overlay/
├── mod.rs              容器：Source::Block / Source::File，取数、滚动、画框（508）
├── screen.rs           impl Screen（390）
└── log.rs              [删] LogStep / log_steps / collapse_log_segment / log_step_detail（501）→ 由 protocol::from_log_line + timeline::subagent::Process 取代
```

依赖方向（`test_scripts/arch_dep_check.py` `TIERS`）：`render` 在第 6 层「场所与展示」，`tools` 在第 5 层「工具与引擎」，`cli` 在第 7 层。上面每一条依赖都是**向下**的：`cli → render → engine → base`。`FORBIDDEN["render"] = {web, cli, platforms}`、`FORBIDDEN["agent"]` 含 `render`——所以共享组装器只能在 `render`，线型只能在 `engine` 或更低，`overlay/` 调 `render`。今天已经是这个方向（`overlay.rs` 大量 `crate::render::timeline::*`）。

### 4.2 `SurfaceCaps`（`surface.rs`）

```rust
pub(crate) struct SurfaceCaps {
    /// 有登记处、能点开（今天：blocks::enabled()）
    pub expandable: bool,
    /// 步跑完立刻落 scrollback（今天：timeline_static()）
    pub commit_immediately: bool,
    /// 段末写 `Worked for …`（今天：!timeline_static()）
    pub fold: bool,
    /// 详情放哪：Inline（就地印）/ Behind（块后面）
    pub detail: DetailPlacement,
    /// 转轮占位符：主线 BLOCK_MARKER，面板 LIVE_SPINNER_CELL
    pub live_marker: char,
}
impl StreamRenderer {
    pub(crate) fn caps(&self) -> SurfaceCaps   // 从四个位算，语义与今天逐字节等价
}
pub(crate) const PANEL_CAPS: SurfaceCaps = …;   // 两个面板共用
```

第一步只是**改名**：`caps().expandable` 替换 `blocks::enabled()`，`caps().commit_immediately`/`.fold`/`.detail` 按各处真实意图替换 `timeline_static()`。等价性用穷举测试钉死（§7 阶段 2）。之后 todolist:21 的「TUI 不收起」= 在 `caps()` 里加一行 `fold = expandable && !config.keep_timeline_open; commit_immediately = !expandable || config.keep_timeline_open`——**不再需要碰任何组装代码**。

`caps()` 保持为方法而不是构造时快照：`blocks::enabled()` 在测试里是线程局部、用例内开关（`blocks.rs:45-48`，`tui_blocks.rs:9-18`），快照会把这批测试全打翻。

### 4.3 过程模型与事件（`subagent_panel.rs` / `subagent_events.rs`）

`Process` 就是今天 `SubagentLog`（`timeline.rs:677-711`）的形状，一个字段都不必发明——它已经同时有 `steps`、`segment: Timeline`、`reasoning/speech` 缓冲、`preparing/running` 活态、`has_prompt`、`step_blocks`。要做的是给它一个**唯一入口**：

```rust
pub(crate) enum ProcessEvent<'a> {
    Prompt(&'a str),
    ThoughtDelta(&'a str),
    Preparing { tool: &'a str },
    ToolStarted { tool: &'a str, display: String, args: &'a str },
    ToolFinished { tool: &'a str, display: String, args: &'a str, ok: bool,
                   output: &'a str, elapsed: Option<Duration> },   // 前台量墙钟，后台读日志
    SpeechDelta(&'a str),
    Stats { text: &'a str, tokens: Option<&'a str> },
    Finished,
}
impl Process { pub(crate) fn apply(&mut self, event: ProcessEvent<'_>, now: Instant) }

pub(crate) fn from_marker(message: &str) -> Option<ProcessEvent<'_>>   // 今天 tool_summary.rs:395-552 那一串 strip_prefix
pub(crate) fn from_log_line(line: &str) -> Option<ProcessEvent<'_>>    // 今天 overlay.rs:694-900 log_steps 的逐行分支
```

`from_log_line` 是 `readable_subagent_log_line_timed`（main `subagent.rs:684-740`；拆 crate 后 `crates/miyu-engine/src/tools/subagent/log.rs:267`）的**逆函数**，两者放进同一个 round-trip 测试：`decode(encode(ev)) == ev`。这是后台面板收敛的核心验收手段——它把"日志能不能还原成事件"从肉眼比对变成机械断言。

线型与编解码器住 **engine**（`tools/subagent/protocol.rs`）而不是 hosts，理由见 §10.2：engine 是标记串与日志文件两者的生产者，编码器本来就在它手里；hosts 只消费。`ProcessEvent` 在 hosts 里只是 `SubagentEvent` 的别名或薄包装，不再有第二份定义。

组装器：

```rust
pub(crate) fn panel_lines(p: &mut Process, caps: &SurfaceCaps) -> Vec<String>   // = subagent_lines ∪ render_log
pub(crate) fn fold_segment(p: &mut Process)                                     // = collapse_subagent_segment ∪ collapse_log_segment
```

### 4.4 主线怎么接

**不搬**。主线 live 状态（`tool_stats` / `reasoning_text` / `command_display` / `tool_preparing`）与 `WaitSpinner`、`CommandLiveDisplay` 的 inline 卡片模式、Full 模式、Pipe 模式五套输出体制纠缠（`reasoning_phase.rs:170-226` 的 `tick_spinner` 一眼可见），面板只有一套。把主线塞进 `Process` 要碰全部 20 处 `self.plain` 与 9 处 `live_summary`，可见收益为零。

主线只共享三样：`Timeline` 计数（面板早就通过 `SubagentLog.segment: Timeline` 在用了）、`fold` 的**展开内容构造**（`cut_timeline:2285-2307` 与 `collapse_subagent_segment:434-451` 做的是同一件事，落点不同）、`SurfaceCaps`。

---

## 5. shellhook 与 inline：该不该合并

**已经合并了，剩下的是命名债。**

| 层 | shellhook（`miyu --shell-intercept`、单次 `miyu "…"`） | inline REPL（`MIYU_TUI=0`） |
|---|---|---|
| 回合循环 | `try_run_remote_chat(live = None)` | `try_run_remote_chat(live = Some(tail))`（`interactive.rs:1247`） |
| 渲染器 | 同一个 `StreamRenderer::new(...)`（`one_shot.rs:90`） | 同上 |
| 输出去向 | `RenderOutput::Terminal` 直写 stdout | `use_buffered_output()` → `live.apply_renderer_frame`（`one_shot.rs:98-104`） |
| 谓词 | `timeline_static()` 真 | `timeline_static()` 真 |
| 交互 | 无输入框，Ctrl+C 走 `RemoteTurnCancelled` | 有活动区（输入框/footer/任务条），回合中可排队 |

渲染侧零成本——本来就零分支。宿主侧要动的：

1. **术语**：代码注释与 `docs/` 一律说"shellhook 静态版"（`timeline.rs:23-27`、`command.rs:429`、`static_timeline.rs:1`、`testkit/static-timeline/run.py`）。改成「**Static surface**（宿主：shellhook／单次／inline REPL／唤醒跟进／daemon 回写）」。这是文档改动，不是代码。
2. **`job_wake.rs:333`**：`renderer.live_summary = true` 是拿"stdout 是终端"这个字段冒充 surface 选择。阶段 2 引入 `SurfaceCaps` 后改成 `StreamRenderer::for_tty_writeback(...)` 或 `set_surface(SurfaceCaps::static_timeline())`，语义就摆在名字上。`static_timeline.rs:18-31` 的测试夹具同款。
3. 不该做的：把 shellhook 的无交互形态再往 REPL 靠（例如给它加活动区）。它是阅后即焚进程，`origin_tty` 回写机制（`one_shot.rs:57-63`）依赖它退出。

用户问的「为什么要分两个」——答案是**从来没分过两个，只是文档一直用宿主名字称呼一个 surface**。

---

## 6. 前台／后台面板收敛

### 6.1 数据源：后台的原始事件流其实已经在

`publish_job_progress`（`src/tools/jobs/mod.rs:229-244`）对每条 `__subtool_*`/`__subagent_*` 标记做三件事：追加进 `job.trace`（上限 `MAX_TRACE = 4000` 条）、调 `progress_hook`、（日志桥另行）落盘。`progress_hook` 由 `job_wake.rs:36` 接上，发成 SSE `job.progress`——**WebUI 后台子代理面板就是靠它流式的**，与前台同款。HTTP 还有 `job_trace_http`（`src/web/turns/mod.rs:537-545`）整段回放。

IPC 没有对应命令（`src/ipc/protocol.rs:127-215` 只有 `JobsOverview` / `FollowRun` / `StopJob`）。REPL 走的是 1s 轮询 `JobsOverview` 拿 `log_path` 再读文件（`overlay.rs:1017-1041`，`screen/mod.rs:1334`）。

所以"两边数据源怎么统一成同一个模型"有两条路：

- **路 A（推荐先做）：两个解码器、一个模型。** 后台仍读日志，但 `log_steps` 退化成 `from_log_line` 逐行解码，喂进与前台相同的 `Process::apply`。日志是 append-only，改成按字节偏移增量读（今天 `reload_file` 已经记 `size`，只是读完整段重解析）；尾巴被截时（256KB 之外）重置——与今天 `overlay.rs:277-280` 同一处理。不改 IPC，不改 daemon，不改日志格式。
- **路 B（拍板后做）：加 IPC `JobTrace { job_id, after: u64 }`**，REPL 直接拿原始标记喂 `from_marker`，与前台完全同路；日志解码器留作 daemon 重启后／老任务的退路。收益：后台面板真正逐 delta 流式（今天 `[思考]/[正文]` 是按段落攒的，`subagent.rs:583-597` `accumulate_stream` 600 字符一落），去掉 150ms 文件轮询。代价：`MAX_TRACE` 4000 条封顶、新 IPC 命令、REPL 侧订阅循环。

路 A 先做的理由：它把"后台面板 = 一个解码器"这件事定下来，路 B 只是再加一个解码器入口，不推翻任何东西。

### 6.2 后台**命令**任务不在此列

`render_log` 头部那段（`overlay.rs:242-275`）——日志里没有 `[思考]/[工具]` 标签时按裸文本折行、顶上铺 `$ 命令`——是给后台**命令**（`kind == "command"`）的，不是子代理。那是另一种 job，输出本来就是流水，"排成梯子"是用户否掉的（`overlay.rs:240-241`）。它留在 `overlay.rs`，不进 `Process`。

### 6.3 收敛时必须拍板的差异（不能默认取一边）

| # | 项 | 建议 | 可见影响 |
|---|---|---|---|
| 1 | 已结算思考步要不要窥视 | 取后台的（有用户实测记录），前台跟上 | 前台面板每个 `已思考` 行多一截尾巴 |
| 2 | `[统计]` 算不算 tool | 取前台的（不算） | 后台 `Worked for` 偶尔少数一个 |
| 3 | 图标退路 | 取前台的（认 `MIYU_TUI_ASCII`） | 仅 ASCII 模式下后台面板变化；老日志（无 `\t` 工具 id）从 `\u{f013}` 变 `glyph_tool()` |
| 4 | 运行中行的秒数 | 有就报、没有就省（`elapsed: Option`） | 无 |
| 5 | 步数上限 | 统一 400 步 | 超长后台任务面板从"按字节截"变"按步截" |
| 6 | 收缩段起点 | 两边规则已一致（上一段正文之后／提示词之后，`timeline.rs:404-410` vs `overlay.rs:577-585`），无需拍板 | 无 |

每一项拍板后，对应的 golden 文件才允许更新（§7 阶段 0）。

---

## 7. 分阶段施工单

门禁：`test_scripts/refactor-check.sh` 五道（格式不倒退／编译／用例数不减／**超标文件不得长于基线 +10**／依赖方向不新增）+ `arch_dep_check.py` 层序 + `request_shape_probe`（拆 crate 后的三件套，§10.4）。

**顺序前提（§10.3）：先合拆 crate。** 施工单以拆 crate 后的布局为准；阶段 1 在那边已经做完，跳过。若最终裁定顺序反过来（先统一后拆），阶段 1 按原文做，且 main 上 `timeline.rs` 基线 2400、现 2403，余量 7 行——阶段 1 必须最先落。

每阶段一个 commit，可独立回滚（`git revert` 单提交）。

### 阶段 0 — 冻结现状（只加测试，零行为）

- 新建 `src/render/tests/golden/`：用固定的 `AgentEvent` 脚本（含 `__subtool_*` 标记、`__patch_preview__`、`__todo_table__`、问答、中断）分别在 S1/S3/S4 三个 surface 下跑一遍 `StreamRenderer`，输出字节存成 `.ansi` 文件；后台面板用一份固定日志文件跑 `open_log_overlay` + `overlay_rows_ansi`，存 rows。时间字段（`\d+\.\ds`、`\d+m \d\ds`）比对前统一打掩码。
- 断言"当前输出 == golden"。这就是 AGENTS §5.1 的"先证明现象"：之后每一阶段不改 golden 就必须逐字节相同。
- 行数：只在 `src/render/tests/`、`src/cli/tests/` 增加（约 +300），受总量 3% 约束（181k × 3% ≈ 5.4k）无压力。
- 验收：`cargo test --lib render:: cli::tests::static_timeline cli::tests::tui_blocks`；golden 生成两次结果一致（确定性）。

### 阶段 1 — 前台面板搬出 `timeline.rs`（纯搬家，零行为）— **拆 crate 工作树 A1 已完成**

拆 crate 工作树（`docs/plan/2026-09-16-file-split.md` A1）已把 `timeline.rs` 切成 `timeline/{mod 781, live 623, subagent 691, question 166, glyphs 136}.rs`，`timeline/subagent.rs` 就是下面要建的 `subagent_panel.rs`（内容一致：`SubagentLog`、`subagent_lines`、`collapse_subagent_segment`、`thread_panel` 与全部 `subagent_*` 方法）。合入后本阶段**跳过**。以下原文仅在「先统一后拆」的反向顺序下才需要执行：

- 新建 `src/render/stream/subagent_panel.rs`：搬 `LiveRow` 之外的 `:286-711`（`SubagentLog`、`PanelEntry`、`thread_panel`、`subagent_lines`、`collapse_subagent_segment`、`flush_subagent_thought`、`seal_subagent_speech`、`render_speech_lines`、`segment_start`、`trim_subagent`、`subagent_title`、`LIVE_SPINNER_CELL`、`panel_live_step_line`、`SUBAGENT_LOG_STEPS`、`PROMPT_GLYPH`）与 `impl StreamRenderer` 的 `:1887-2191`（`subagent_thought` … `finish_subagent_log`，含 `subagent_prompt`、`subagent_content`、`subagent_stats`、`subagent_tokens_label`、`refresh_subagent_panels`、`publish_subagent`）以及 `:2226` 的 `subagent_overlay_id`。
- `timeline.rs` 里被两边用的小件（`step_line_in`、`step_line_failed_in`、`rail`、`thread`、`indented_body`、`wrap_detail`、`detail_width`、`panel_step_width`、`glyph_*`、`timed_label`）改 `pub(super)`。
- 行数：`timeline.rs` 2403 → ≈ 1675（过红线 2000，仍超上限 1500 — 门禁只禁变长，不要求达标）；新文件 ≈ 750（< 目标 800）；`mod.rs` +1 行 `mod`。
- 验收：阶段 0 golden 逐字节不变；`render/tests/timeline.rs` 41 例、`tool_summary.rs` 36 例、`tui_blocks.rs` 33 例全绿；`refactor-check.sh` 第四道余量从 7 行变成七百多行。
- 回滚：revert 即可，不涉及数据。
- 坑（AGENTS §6.3）：`use super::*` 语义变化——`subagent_panel.rs` 的 `super` 是 `stream`，不是 `timeline`；`t()` 从 `crate::render::t` 拿。

### 阶段 2 — 命名能力位（改名为主，零行为）

- 新建 `src/render/stream/surface.rs`：`SurfaceCaps` + `DetailPlacement` + `StreamRenderer::caps()` + `PANEL_CAPS`。
- 机械替换（附录 A 的清单逐处过）：
  - `blocks::enabled()` render 侧 14 处 → `self.caps().expandable`（`cli/mod.rs:921, 928, 943, 1028, 1035` 五处在 render 外，保留原样或给 `render::current_surface_expandable()`）；
  - `timeline_static()` 9 处按意图分三类：`settle_new_steps:1257`、`commit_static_steps` 调用点、`cut_timeline:2264` → `commit_immediately` / `!fold`；`timeline_push_thought:1336`、`timeline_push_question:1787`、`write_tool_result:141,243`、`interrupt_command_display:553`、`timeline_push_tools:1129`（决定 `:1155` 那段静态版不留统计详情）→ `detail == Inline`；
  - `timeline_enabled()` 35 处不动（它本来就是"有没有时间线"，语义清楚）。
- 顺手去重：项 A 的 `(detail, tail)` 抽成 `fn command_step_parts(&self, display, ok) -> (Vec<String>, Vec<String>)`（−10 行）；项 E 的问答文案抽 `answered_lines(request, answers, width)`（−15 行）；`job_wake.rs:333` 改显式构造。
- 行数：`surface.rs` +80；`timeline.rs` −25；`tool_summary.rs` −8；`mod.rs` −4；`job_wake.rs` ±0。
- 验收：golden 不变；**新增穷举测试**：对 `plain × live_summary × blocks × tool_call_mode`（2×2×2×3 = 24 组）断言 `caps()` 推出的 `commit_immediately == 旧 timeline_static()`、`expandable == 旧 blocks::enabled()`、`fold == 旧 !timeline_static() && timeline_enabled()`。
- 这一阶段落地后，todolist:21「TUI 不收起 Worked for」= `caps()` 里两行 + 配置项，可单独做。

### 阶段 3 — 后台面板改用同一个模型（路 A）

- 新建 `crates/miyu-engine/src/tools/subagent/protocol.rs`：`SubagentEvent`（§4.3 的 `ProcessEvent`）、`to_marker`/`from_marker`（从 hosts `tool_summary.rs:395-552` 抽出 `strip_prefix` 那一串；`to_marker` 必须与今天 `subagent_runner.rs` 拼的字符串逐字节相同——标记串已落库在 `tool_flow`/`job.trace`，AGENTS §2.3 双兼容）、`from_log_line`（从 `overlay/log.rs` 的 `log_steps` 抽出逐行分支，与同目录 `log.rs` 的编码器成对）；`parse_seconds` 放 `miyu-base/src/durations.rs` 与 `format_seconds` 作伴。
- hosts `tool_summary.rs`：`write_tool_progress` 改成 `if let Some(ev) = protocol::from_marker(msg) { self.subagent_apply(name, ev) }`。
- hosts `timeline/subagent.rs`：`Process::apply(SubagentEvent, now)` 作为唯一入口；`ToolFinished.elapsed` 为 `None` 时量墙钟（今天 `tool_since`），为 `Some` 时直接用。
- 根包 `overlay/mod.rs`：`Source::File` 增加 `process: Process` 与 `offset: u64`；`reload_file` 改为增量读新字节、按 `\n` 切完整行、`from_log_line` → `apply`，再 `panel_lines(&mut process, &PANEL_CAPS)`。**整文件删除 `overlay/log.rs`**（`LogStep`、`log_steps`、`collapse_log_segment`、`log_step_detail`、`step_head`、`split_elapsed`、`with_elapsed`、`strip_result_status`、`is_tool_step`、`subject_of`、`split_tool_line`、`split_thought_elapsed`、`STATS_GLYPH`；`read_tail` 改成增量版留在 `mod.rs`）；`mod.rs` 里的 `render_log`/`fold_detail`/`step_blocks`/`fold_blocks` 随之删，只剩后台命令那一段。
- 行数：`overlay/log.rs` −501（删）、`overlay/mod.rs` −120；`protocol.rs` +200（含 round-trip 测试）；`durations.rs` +20；`timeline/subagent.rs` +60；`tool_summary.rs` −120。净 −460 左右。
- 验收：
  - round-trip：对 `subagent.rs` 的 `readable_subagent_log_line_timed` 输出跑 `from_log_line`，覆盖八种前缀 + 老格式（无 `\t` id、无耗时）+ `[思考] 1.2s\t…` + `\u{1}` 多行提示词。
  - `tui_blocks.rs` 14 个 `job_panel_*` / `a_log_*` 用例；后台 golden **只在 §6.3 拍板项上**允许更新，且每项更新单独一个 diff 说明。
  - `an_open_expansion_survives_a_panel_refresh`（块 id 按位置复用）必须仍过——`Process.step_blocks` 是前台方案，后台改用它后 id 稳定性同款。
  - 真机：`testkit/tui/live.py` 借真配置跑一个后台子代理，点开面板对比阶段 0 抓屏（时间打码）。
- 回滚：revert 单提交；日志格式未动，daemon 不用重启。

### 阶段 4 — 后台改订原始事件流（路 B，需拍板）

- `src/ipc/protocol.rs` 加 `Command::JobTrace { job_id, after: u64 }`，`ipc_server.rs` 返回 `job.trace[after..]` 与新游标；REPL 的 `spawn_jobs_poll_thread` 在面板开着时附带拉取，`Source::Trace { job_id, cursor, process }` 走 `from_marker`。日志解码留作 `Source::File` 退路（daemon 重启、老任务）。
- 行数：`protocol.rs` +15、`ipc_server.rs` +40、`cli/repl/jobs.rs` +60、`overlay.rs` +40。
- 验收：桩模型（`testkit/repl-smoke/stub_llm.py` 分阶段工具调用）跑后台子代理，量"事件发出 → 面板行变化"延迟应从段落级（≤600 字符一落）降到单 tick；文件轮询关闭后 `strace -e openat` 面板期间不再周期性打开日志。
- 不做的条件：如果实测段落级刷新用户不介意，路 B 的收益只剩去掉轮询，不值一条新 IPC。

### 阶段 5 — 主线收缩与面板共用构造（可选，实测后判）

- 抽 `fold_block_lines(summary, steps: &[Step]) -> Vec<String>` 供 `cut_timeline:2296-2318` 与 `collapse_subagent_segment:434-451` 共用；主线仍 `write_expandable` 直写，面板仍 push 回 `steps`。
- 行数：`timeline.rs` −60，`subagent_panel.rs` +30。
- 只有当 todolist:17/19「思考／命令从 Worked for 分离」真要做时才值得——那时收段规则改一处比改两处省。否则不做。

### 行数总账（拆 crate 后布局；main 合入的 +69/+35 已计入起点）

| 阶段 | `timeline/mod.rs` | `timeline/subagent.rs` | `tool_summary.rs` | `overlay/{mod,log}.rs` | 新文件 | 净 |
|---|---|---|---|---|---|---|
| 起点 | ≈ 800 | 691 | ≈ 943 | 508 / 501 | – | – |
| 0 | 0 | 0 | 0 | 0 | hosts `render/tests/golden.rs` +300 | +300（测试） |
| 1 | 已由 A1 完成 | | | | | 0 |
| 2 | −20 | 0 | −8 | −5 | `stream/surface.rs` +80 | +47 |
| 3 | 0 | +60 | −120 | −120 / **−501** | engine `subagent/protocol.rs` +200、base `durations.rs` +20 | −461 |
| 4（拍板） | 0 | 0 | 0 | +40 / – | `ipc/protocol.rs` +15、`ipc_server.rs` +40、`cli/repl/jobs.rs` +60 | +155 |
| 5（可选） | −40 | +20 | 0 | 0 | – | −20 |

拆 crate 后没有文件超过 1500 上限，第四道门禁只剩「新文件不越红线、总量 +3%」两条；`protocol.rs`/`surface.rs`/`golden.rs` 都在几百行量级。合入拆 crate 后要先 `refactor_size_report.py --write-baseline` 重设基线（那边 §6 已重设过一次，合 main 后再设一次）。

若顺序反过来（先统一后拆），行数总账按原 main 布局：阶段 1 `timeline.rs` 2403 → ≈1673，阶段 3 `overlay.rs` 1427 → ≈740，其余同上。

---

## 8. 风险与「实测后判不做」

### 8.1 风险

1. **以统一之名抹掉实测结论。** 两个面板之间的六处差异（§6.3）每一处都有可能是某次截图后的裁定。对策：golden 冻结 + 拍板表；未拍板的项 golden 不许改。
2. **门禁余量只有 7 行（仅 main 布局）。** `timeline.rs` 2403 对基线 2400。阶段 1 之前任何给它加 8 行以上的改动都会红——这条线在调研期间就被 `32227cc0` 踩过一次（2418 → 2403 才回到绿）。合入拆 crate 后这条风险消失（最大文件 ≈ 800）。
3. **`blocks` 是进程级全局 + 测试线程局部。** 任何把 `caps` 做成构造时快照的写法都会让 `with_blocks` 系列测试翻车。`caps()` 必须保持为按需计算。
4. **回放路径用的也是同一个渲染器**（`cli/mod.rs:940`）。golden 脚本必须含一段 `ReplayEntry` 序列，否则 `replay_tool_elapsed`、`summary_without_timing_drops_the_duration` 这类只在回放触发的行为没有网。
5. **后台面板从"整段重解析"改"增量喂"。** 日志尾巴被截（>256KB）时今天靠 `steps.len() < step_blocks.len()` 兜底重置；增量版要在偏移回退时清 `Process` 重建，否则块 id 错位。测试 `a_log_fold_opens_into_a_timeline_and_the_running_step_spins` 与 `an_open_expansion_survives_a_panel_refresh` 盯住。
6. **`from_log_line` 对老日志的兼容。** 老格式没有 `\t` 工具 id、没有耗时；round-trip 测试要含这两种，且解码失败要退回"裸文本行"而不是丢行（今天 `overlay.rs:867-883` 的规则：跟在思考／正文后才算续行，跟在工具后丢掉）。

### 8.2 判不做（或不在本次）

| 候选 | 理由 |
|---|---|
| 主线 live 状态搬进 `Process` | 五套输出体制纠缠（§4.4），可见收益零，触碰 29 处 `plain`/`live_summary` |
| 统一 `BLOCK_MARKER` 与 `LIVE_SPINNER_CELL` | `\u{1}` 在 `wait_spinner.rs:171-184` 还兼作多块布局的分隔符；动画驱动本来就是两个；用 `SurfaceCaps.live_marker` 把意思说出来就够 |
| 给 shellhook 加活动区／把 inline 往 shellhook 靠 | 阅后即焚与 `origin_tty` 回写依赖进程退出；surface 已同一 |
| 把 S1 Pipe 的一行摘要也换成时间线 | 管道里没有转轮没有回翻，用户没提；`piped_output_keeps_the_plain_summary` 钉着 |
| 把 WebUI 时间线（`web/*.js`）也接到 `Process` | 方向对（它消费的就是同一批 SSE 标记），但跨语言；留作后话，不在终端统一范围 |
| 阶段 4 路 B | 段落级刷新是否够用要先问用户；不够再做 |
| 修 S5 混合形态 | 它是 todolist:11 的一半，应随「显示工具调用信息开关语义」一起拍板（Full 在时间线下该是"默认展开"而不是"旧卡片"），不夹进重构提交（`confirm-before-deleting-redundancy` 原则） |

---

## 9. 验收手段总表

先三件套（拆 crate 后的硬要求，AGENTS §8.5），再渲染专用量尺，最后才是 testkit 走查：

| 手段 | 位置 | 盯什么 |
|---|---|---|
| **① `request_shape_probe`** | `MIYU_REQUEST_SHAPE_OUT=/tmp/x.json cargo test --lib request_shape_probe -- --ignored`（engine crate） | 每阶段五张脸**逐字节零变化**——本方案不碰请求组装，任何差异都是误伤；阶段 3 动了 `tools/subagent/`，必须跑 |
| **② `refactor-check.sh`** | `systemd-run --user --scope -p MemoryMax=20G -p MemorySwapMax=0 bash test_scripts/refactor-check.sh`（`--workspace`） | 五道：格式不倒退／编译零警告／用例数不减／规模不恶化／依赖方向；一次只跑一个 cargo |
| **③ `arch_dep_check.py`** | `python3 test_scripts/arch_dep_check.py` | 新增模块都是子模块、不需登记 `TIERS`；盯 `render` 不新增对 `cli/web/platforms` 的引用、`tools`（engine）不引 `render`；白名单只减不增 |
| golden 字节比对（新） | `crates/miyu-hosts/src/render/tests/golden.rs`（main 布局：`src/render/tests/golden/`） | 阶段 0–5 每一步"表现未变"的机械证据；时间打码；这是「做不到零字节时量尺要能指出只变了那一段」（理念 §十）在渲染层的落地 |
| 穷举等价测试（新） | `src/render/tests/surface.rs` | 阶段 2：24 组模式位下 `caps()` ≡ 旧谓词 |
| round-trip（新） | `src/render/tests/subagent_events.rs` | 阶段 3：日志编码↔解码互逆 |
| 既有单测 | `render/tests/timeline.rs`(41) `tool_summary.rs`(36) `command.rs`(24) `reasoning.rs`(19) `cli/tests/static_timeline.rs`(10) `tui_blocks.rs`(33) | 每阶段全绿，用例数不减（门禁第三道） |
| 静态 surface 走查 | `python3 testkit/static-timeline/run.py`（含 Ctrl+C 中断第二轮） | 阶段 1/2 后 `screen-*.txt` 与阶段 0 产物 diff（时间打码） |
| 全屏走查 | `python3 testkit/tui/run.py`、`testkit/tui/todo_table.py`（tmux 抓屏） | 同上；`todo_table` 盯 1e112bbe 刚修的"表跟在收缩行后" |
| 真模型面板走查 | `testkit/tui/live.py`（借真配置跑沙箱） | 阶段 3/4：前台、后台各开一次面板，对比抓屏 |
| 后台事件延迟（阶段 4） | 桩模型 + 面板行变化时间戳 | 段落级 → tick 级 |

---

## 10. 拆 crate 工作树（`core-normal-2026-09-16`）对本方案的影响

只读该工作树（基于 `843c169a`，未提交，`git status` 842 条 / 580 处改名）。它已通过自己的验收：`cargo test --workspace` 2427 过 / 0 败、`request_shape_probe` 五张脸归一后逐字节相同、五道门禁全绿（`docs/plan/2026-09-16-crate-split.md §6`）。

### 10.1 它已经替本方案做了什么

| 拆 crate 那边 | 对应本方案 | 结论 |
|---|---|---|
| A1：`timeline.rs` → `crates/miyu-hosts/src/render/stream/timeline/{mod 781, live 623, subagent 691, question 166, glyphs 136}.rs` | 阶段 1（抽 `subagent_panel.rs`） | **已完成**。`timeline/subagent.rs` 的内容清单（`SubagentLog`、`subagent_lines`、`collapse_subagent_segment`、`thread_panel`、全部 `subagent_*`）与阶段 1 一字不差 |
| A6：`overlay.rs` → `src/cli/repl/tail/screen/overlay/{mod 508, log 501, screen 390}.rs` | 阶段 3 要删的 `LogStep`/`log_steps`/`collapse_log_segment`/`log_step_detail` | 正好被隔进 `overlay/log.rs` 一个文件，阶段 3 变成**整文件删除** |
| A4：`tools/subagent.rs` → `crates/miyu-engine/src/tools/subagent/{mod, log 319, audit, tests}.rs` | 阶段 3 的编解码成对 | 编码器（`readable_subagent_log_line_timed` 等）已独立在 `log.rs`，解码器放同目录 `protocol.rs` 即成对 |
| `format_seconds` → `miyu-base/durations.rs`；`tool_peek`/`readable_tool_name`/`command_peek` → `miyu-engine/tools/tool_display.rs`、`readable_names.rs` | 编码器对 render 的反向引用 | engine 已零引用 render（grep 空），编解码器住 engine 不会新增跨层边 |
| `cli/mod.rs::handle_agent_event` → `crates/miyu-hosts/src/render/agent_events.rs::apply_agent_event`；IPC 解码 → `runtime/ipc_events.rs` | §1.3「输入侧已统一」 | 更强：阶段 0 的 golden 可以在 hosts crate 内直接喂 `AgentEvent` 脚本，直连/IPC/回写三条路一网打尽 |
| `blocks.rs` 线程局部改 `cfg(any(test, feature = "testkit"))`，根包 dev-deps 打开 `testkit` | 阶段 2「`caps()` 必须按需计算」的前提 | 不变，但**根包里的测试**（`cli/tests/tui_blocks.rs`、`static_timeline.rs`）看到的 `blocks` 线程局部依赖 `testkit` 特性——golden 若放根包必须确认特性已开（`Cargo.toml:67`） |

### 10.2 过程模型落哪一层：向下依赖，不是端口

AGENTS §8.3 的端口是给「**下层要上层的能力**」用的（`VoicePort`、`QqOutreachPort`、`PlatformToolContext`：tools 要 platforms/web 的东西）。本方案的数据流是**engine 产出 → hosts 消费**，方向向下，走普通 `use` 即可——`render/agent_events.rs` 今天就是这么 `use miyu_engine::agent::AgentEvent` 的。

于是分两半放：

| 东西 | 层 / crate | 位置 | 为什么 |
|---|---|---|---|
| **线型** `SubagentEvent` + `to_marker`/`from_marker` + `from_log_line`（编解码） | 工具与引擎 / `miyu-engine` | `tools/subagent/protocol.rs`（与 `log.rs` 编码器同目录） | engine 是 `__subtool_*` 标记串（`subagent_runner.rs`）和 `[思考]/[工具]…` 日志（`subagent/log.rs`）**两个产出物的生产者**；编码器本来在它手里，解码器放对面就成对，round-trip 测试不跨 crate。纯数据，不碰图标/宽度/颜色，不会反向需要 render |
| `parse_seconds` | 基础 / `miyu-base` | `durations.rs`（`format_seconds` 旁） | 逆函数与正函数同住 |
| **视图模型** `Process`(=`SubagentLog`)、`Step`、`SurfaceCaps`、`panel_lines`/`fold_segment` | 场所与展示 / `miyu-hosts` | `render/stream/timeline/subagent.rs`、`render/stream/surface.rs` | 只有 render 关心"画成什么样"；`web`（同层）以后想要 JSON 版可以直接 `use`，也不需要端口 |
| 容器 `Overlay`（滚动、画框、点击） | 入口 / 根包 | `src/cli/repl/tail/screen/overlay/` | 终端交互是入口层的事；它向下 `use` render 的 `Process` |

**什么时候才会需要端口**：如果 engine 想知道"我现在的宿主是哪种 surface"来决定发不发 `__subtool_*`（例如 Static 下不发以省 IPC）——那是下层要上层的信息，得走 `host_ports`。本方案不提议这么做：标记一律发，surface 只在 hosts 决定画不画（今天在 Static 下 `SubagentLog` 也在攒、只是 `register_overlay` 返回 `None`，`blocks.rs:77-82`）。

不放 `miyu-core`（传输层）的理由：标记串是 `AgentEvent::ToolProgress.message` 的载荷，`AgentEvent` 本身在 engine；core 不认识子代理。

### 10.3 先后顺序：先合拆 crate，再做渲染统一

**冲突面**（main 在 `843c169a` 之后的提交 `deb977b5 / 172ffb95 / 1e112bbe / 32227cc0` vs 拆 crate 的搬迁）：

| main 改的文件 | 拆 crate 那边的去向 | 合并方式 |
|---|---|---|
| `src/render/stream/timeline.rs`（+69：`timeline_ends_after_tools` 收段意图、`command_peek` 读 `title`、编辑步抬头 `+N -M`） | 搬到 hosts **并切成五个文件**（A1）；`command_peek` 更被挪到 engine `tools/tool_display.rs` | git 认不出改名+拆分，**手工**：收段意图 → `timeline/mod.rs` + `stream/mod.rs`；`command_peek` 语义改动 → engine `tool_display.rs`（跨 crate）；`+N -M` → `tool_summary.rs`/`timeline/mod.rs` |
| `src/render/stream/tool_summary.rs`（+35）、`src/render/patch.rs`（+55）、`src/render/stream/mod.rs`（+20） | 原样搬到 hosts | git 改名检测应能自动合，复核即可 |
| `src/render/command.rs`（+47） | hosts 里已再切成 `command.rs` 890 + `command/` 目录 | 手工 |
| `src/render/tests/timeline.rs`（−146）+ 新 `command_step.rs`、`todo.rs`、`tests/mod.rs` | hosts 里同一份测试被切成 `timeline.rs` + `timeline_panels.rs` | **两边各切了一刀**，`tests/mod.rs` 与 `timeline.rs` 必冲突，手工 |
| `src/tools/command_guard.rs`（新 345 行）、`src/tools/mod.rs`（+6）、`descriptions/run_command.json`、`fixtures/registry-shapes.json` | `tools/` 搬到 engine，`tools/mod.rs` 被 A2 切成四份；JSON 资源留原处 | 新文件放 `crates/miyu-engine/src/tools/`；`mod.rs` 手工；资源不动 |
| `src/cli/repl/{direct,editor}.rs`、`remote/interactive.rs`、`cli/tests/*` | 留根包 | 自动 |

合计 **≈ 8 处手工合并**，其中 3 处跨 crate（`command_peek`、`command_guard`、`tools/mod.rs`）。这是拆 crate 落地本来就要付的账，与本方案无关。

**为什么不反过来**：若先在 main 做渲染统一（改 `timeline.rs`/`overlay.rs`/`tool_summary.rs`/`subagent.rs`），拆 crate 那边的 A1/A4/A6 三份拆分要在新内容上重做（各 ~13 分钟子代理，便宜），但更贵的是 534 文件搬迁要在改动过的这四个文件上重新过 `split_crates.py` + 19 轮可见性放宽 + 整套验收（2427 测试、探针、端到端 9+7 项）。而且 `crate-split.md §5` 明写「拆 crate 期间不合任何别的改动」。反向顺序唯一的好处是 main 上少一次"7 行余量"的门禁纠缠——可它在拆完之后根本不存在。

**本方案对拆 crate 的零冲突面**：阶段 0/2/3/5 全在 hosts 内部或 hosts → engine 向下；阶段 3 在 engine 加一个新文件、不改 `log.rs` 现有函数签名；阶段 4 加 IPC 命令（core）+ 根包。没有一步需要改 `TIERS`，没有一步会让白名单变多。

### 10.4 验收改用三件套

见 §9 表首三行。补两条本方案特有的口径：

- `request_shape_probe` 的期望是**零差异**（不是"只变一段"）：渲染层不参与请求组装，任何差异都是误伤——阶段 3 动了 `tools/subagent/`，这一条是它的护栏。
- golden（阶段 0）是渲染层的「五张脸」：S1/S3/S4 三个主线 surface + 前台面板 + 后台面板各一份，时间打码后逐字节；理念 §十说的「做不到零字节变化的改动，量尺要能指出只变了那一段」在这里就是"§6.3 拍板项之外 diff 必须为空"。

### 10.5 施工单落到新布局后的差异摘要

- 阶段 1 取消（A1 已做）。
- 阶段 2 的 `surface.rs` 放 `crates/miyu-hosts/src/render/stream/`；`timeline_enabled`/`timeline_static` 在 `timeline/mod.rs`；`job_wake.rs:333` 的硬改在 `crates/miyu-hosts/src/web/actor/job_wake.rs`（同 crate，可直接给 `StreamRenderer` 加构造函数）。
- 阶段 3 新文件在 engine（`tools/subagent/protocol.rs`）；删的是根包 `overlay/log.rs` 整文件；`overlay/test_support.rs`（47 行）里与 `LogStep` 相关的夹具随之删。
- 阶段 0 的 golden 放 hosts `render/tests/`，通过 `agent_events::apply_agent_event` 喂脚本；后台面板 golden 仍在根包 `cli/tests/tui_blocks.rs` 旁（它要 `Screen`）。
- 门禁基线：合入拆 crate 后先 `--write-baseline`，再开工。

---

## 附录 A：模式位消费点（非测试代码，main `32227cc0` 布局）

`timeline_static()`（9 处调用）：`mod.rs:553`；`timeline.rs:1108`（`timeline_enabled` 体内）`, 1129, 1257, 1336, 1787, 2264`；`tool_summary.rs:141, 243`；定义 `timeline.rs:1117`。

`blocks::enabled()`（render 侧 14 + cli 侧 5）：`mod.rs:270, 326, 349, 363, 405, 413, 488`；`timeline.rs:1108, 1118, 1427, 1846, 2048`（`:1105` 是注释）；`tool_summary.rs:624`；`cli/mod.rs:921, 928, 943, 1028, 1035`（回放帧与合成轮提示，render 之外）。

`live_summary`（9）：`mod.rs:92`（字段）`, 163`（初始化）；`tool_summary.rs:44, 170, 289, 649`；`reasoning_phase.rs:390`；`timeline.rs:1119`；`job_wake.rs:333`（硬改）；`static_timeline.rs:29`（测试夹具）。

`timeline_enabled()`（35）：`mod.rs:386, 442, 533`；`tool_summary.rs:34, 129, 201, 246, 289, 312, 343, 408, 420, 439, 447, 469, 484, 518, 577, 610`；`reasoning_phase.rs:35, 188, 191, 211, 230, 384, 385`；`timeline.rs:1736, 1888, 1904, 1932, 1958, 2097, 2149, 2165, 2182`（各 `subagent_*` 入口与 `timeline_push_question` 的首行守卫）。

`self.plain`（20）：几乎全是"打不打"的守卫（`mod.rs:210, 226, 263, 287, 400, 485, 510, 521, 693`；`tool_summary.rs:15, 78, 113, 297, 357`；`reasoning_phase.rs:14, 97, 364, 390, 420`；`timeline.rs:1120`）。

## 附录 B：宿主侧构造点

| 位置 | 宿主 | surface |
|---|---|---|
| `src/cli/repl/remote/one_shot.rs:90` | shellhook / 单次 / inline REPL / 全屏 REPL（daemon 路径） | S1–S5 按环境 |
| `src/cli/repl/direct.rs:71, 258, 675, 793` | 同上的直连（无 daemon）退路 | 同上 |
| `src/cli/repl/live_turn.rs:154` | 直连 REPL 回合 | S3/S4 |
| `src/cli/repl/wake.rs:42` | REPL 跟进后台唤醒 | S3/S4 |
| `src/web/actor/job_wake.rs:325` | daemon 回写 shellhook 原终端 | S3（硬设） |
| `src/cli/mod.rs:940` | 会话回放帧 | 跟随当前 `blocks` |

## 附录 C：已有测试对各 surface 的覆盖

- S3 Static：`cli/tests/static_timeline.rs` 10 例（含 `a_thought_is_one_line_and_there_is_no_worked_for_handle`、`a_command_still_running_at_the_end_is_folded_in_as_interrupted`、`a_written_back_turn_keeps_the_reply_off_the_spinner_row`），`render/tests` 内 `live_summary = true` 11 处。
- S4 Full：`render/tests` 内 `set_enabled(true)`/`with_blocks` 35 处；`cli/tests/tui_blocks.rs` 33 例。
- 前台面板：`render/tests/timeline.rs` 28 处 `subagent_*` 调用（`a_subagent_panel_opens_with_the_task_it_was_given`、`a_subagent_panel_folds_its_steps_once_it_starts_talking`、`the_fold_opens_into_a_timeline_not_an_indented_body`、`panel_speech_is_markdown_rendered` …）。
- 后台面板：`tui_blocks.rs` 14 例 `job_panel_*` / `a_log_*`。
- S1 Pipe：`piped_output_keeps_the_plain_summary` 1 例。S0/S2/S5：无专门用例（S5 本就是漏网之鱼）。
