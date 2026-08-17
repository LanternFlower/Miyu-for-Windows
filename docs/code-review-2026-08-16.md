# Miyu 代码审查报告（2026-08-16）

两轮只读深度审查的合并报告。

- **范围**：`src/` 全部 68 个 `.rs` 文件（约 17.6 万行）、`build.rs`、`web/app.js`（9564 行）+ `index.html`、`testkit/*`。
- **方法**：第一轮按模块分 11 个分区逐文件通读；第二轮五个专项维度（并发/锁、不可信输入可达性、token 计量端到端、前端、重试/错误路径）。所有高危结论均在源码现场二次验证（行号以 2026-08-16 版本为准，后续改动会有 ±20 行漂移）。
- **验证状态标记**：`[已验证]` = 审查后在源码逐行确认；`[已量化]` = 给出数字推算；`[疑似]` = 模式确认但触发条件依赖运行时竞态。
- **总原则备注**：release profile 未开 overflow-checks，算术回绕不 panic；切片越界/char boundary panic 在 release 同样触发；栈溢出与分配失败是 abort，任何上下文都全进程死。

---

## 优先级总表（两轮合并 Top 10）

| # | 问题 | 位置 | 等级 | 状态 |
|---|------|------|------|------|
| 1 | `split_at` 中文 panic："5分钟"即可触发 | `src/alarm.rs:43` | 高 | [已验证] |
| 2 | 压缩 cut=0 空转死循环，`/compact` 救不回，只能 `/clear` | `src/agent/compact.rs:258,419` | 高 | [已量化] |
| 3 | 5 处 `{`/`}` 反序切片 panic（含外部入群申请可达） | 见 §1.1 | 高 | [已验证] |
| 4 | MCP 阻塞 IPC 无超时，可耗尽 runtime 拖垮 daemon | `src/tools/mcp.rs:209` | 高 | [已验证] |
| 5 | actor 单线程 runtime 被 30s 阻塞网络请求冻结 | `src/agent/mod.rs:2449` | 高 | [已验证] |
| 6 | 断连不 drain pending，在途调用卡最长 180s | `src/platforms/onebot.rs:1150` | 高 | [已验证] |
| 7 | token 口径漏算 `tool_flow`，偏差约 57 倍 | `src/agent/mod.rs:4914` + `compact.rs:711` | 高 | [已量化] |
| 8 | 前端无限 resync 循环（刷新页面即触发） | `web/app.js:3573` | 高 | [已验证] |
| 9 | 嵌套重试放大：3×3=9 次全量重发无退避 | `src/llm/openai_compatible.rs:1697` | 高 | [已验证] |
| 10 | `strip_tagged_sections` 差一越界 panic，主流路径 | `src/llm/openai_compatible.rs:8569` | 高 | [已验证] |

---

# 第一轮：按缺陷类别

## 1. 溢出 / 下溢 / 越界

### 1.1 直接 panic（release 同样触发）

**P1 `src/alarm.rs:43`** — `split_at` 非字符边界 [已验证]
```rust
let (number, unit) = part.split_at(part.len() - 1);
```
模型对"5分钟后提醒我"传 `time: "5分钟"`（中文时长是自然输出，无需恶意）→ `split_at(6)` 落在"钟"（字节 4-6）内部 → panic。CLI 主任务崩进程；QQ 侧杀回合。修法：按 `char` 取末字符或 `strip_suffix`。

**P2 `find('{')`/`rfind('}')` 反序切片，全库 5 处** [已验证]
```rust
let start = trimmed.find('{').context(...)?;
let end   = trimmed.rfind('}').context(...)?;
serde_json::from_str(&trimmed[start..=end])?   // start > end 时 panic
```
| 位置 | 输入来源 | 可达性 | 附带后果 |
|------|---------|--------|---------|
| `src/platforms/plugins/real_context/judge.rs:530` | judge 模型输出（QQ 群消息可注入诱导） | 需模型配合 | 杀消息 task |
| `src/platforms/plugins/real_context/affection.rs:1350` | affection 模型输出 | 需模型配合 | **worker 死后队列静默积压直到重启**（`is_closed()` 因 Sender 未关而判 false 不重建，affection.rs:124） |
| `src/memory/organizer.rs:317` | organizer 模型输出 | 需模型配合 | 整理线程死，记忆整理永久停摆 |
| `src/tools/web_images.rs:2102` | vision 模型输出 | 需模型配合 | — |
| `src/platforms/onebot.rs:2467` | **任意外部用户的入群申请**→审批 LLM | 外部可构造输入 | 杀 spawn task |

统一修法：抽一个安全提取函数（`if end < start { bail }` 或 `text.get(start..=end)`）。同库 `deep_research.rs:527` 用深度计数器提取，是正确范本。

**P3 `src/llm/openai_compatible.rs:8569`** — `strip_tagged_sections` 差一 [已验证]
```rust
let content_start = text[start..].find('>')
    .map(|offset| start + offset + 1)
    .unwrap_or(start + open.len());          // open 17 字节，查找前缀 16 字节
let Some(relative_end) = text[content_start..].find(&close) else { ... }  // ← panic 点
```
输出恰以 `<system-reminder`（16 字节、无 `>`）结尾（`finish_reason=length` 截断在标签中间）时 `content_start > text.len()` 越界。`finalize_stream_result` 对每条 content/reasoning 无条件调用，主流路径。用户让模型"逐字输出 `<system-reminder`"即可命中。同文件 `extract_dsml_tool_calls`（5282）用前缀一致的长度，无此问题。

**P4 `src/tools/man.rs:102`** — `&sec[..1]` 多字节 panic [已验证]
section 来自模型 JSON，`"节"` 等中文首字符非 char boundary。改 `sec.chars().next()`。

**P5 `src/render/math.rs:245,270`** — LaTeX 递归无深度上限
`convert_sequence`/`sequence_box` 每层 `{` 递归一次，模型输出 `$` + 10 万层 `{` + `$` → 栈溢出 abort，CLI 渲染路径全进程死。建议 64 层上限超限回退原文。

### 1.2 release 下静默回绕

| 位置 | 表达式 | 触发 | 状态 |
|------|--------|------|------|
| `src/alarm.rs:46,59` | `amount * 3600`、`timestamp() + secs as i64` | `9999999999999999h` | [已验证] |
| `src/tools/xuanxue.rs:238` | `total + modifier`（modifier 为模型任意 i64） | 模型传极值 | [已验证] |
| `src/state/usage.rs:367` | `Days(n)` 的 `n - 1` | **当前不可达**（parse 只产生 7/30，无 serde 构造点），纯防御缺口 | [已验证] |
| `src/config_tui.rs:7938` | `content_w as u16` 先截断再 min | 选项 >65535 字符（门槛极高） | — |

### 1.3 解码炸弹

**`src/platforms/onebot.rs:5941,6040`** — `image::load_from_memory` 未设 `Limits`，几十 KB 的 30000×30000 PNG 解压约 3.6GB，分配失败 abort。可达性：需管理员回合 + 本地已被放置炸弹文件（远程来源 web_images/memes/WebUI 上游均有校验），**仅本地数据损坏触发**。解码结果仅用于校验即丢弃，同时是 async 内同步解码。

## 2. 循环缺陷 / 挂死

1. **`src/tools/mcp.rs:173-216`** — `read_message` 的 `read_line` 无超时永久阻塞，且同步函数直接跑在 async 闭包里。MCP server 挂起即永久占用 tokio worker，多次触发耗尽 runtime，整个 daemon 卡死；`register()`（:56）启动期同样无界读，单个挂起的 server 让 daemon 启动挂起。每次调用还重新 spawn 进程 + 握手无复用。
2. **`src/tools/scripts.rs:566`** — stdin `write_all` 在 `tokio::time::timeout` 块之外，脚本不读 stdin 且输入 >64KB 管道缓冲时永久 pending，`timeout_secs` 完全失效（`kill_on_drop` 已设但 future 不返回就无法走到 drop）。
3. **`src/tools/deep_research.rs:212,787`** — xhigh 档 `max_revisions = usize::MAX` 且 `max_tool_steps = 0`，修订循环无轮数/时长/成本上限。
4. **`src/tools/caniplayonlinux_query.rs:76`** — 每次查询全量串行爬整站分页，页数来自远端 HTML 无上限。
5. **`src/agent/mod.rs:1673`** — 每轮对话 2-5 次全量历史重建 + 全量真实 BPE 重算（`effective_context_tokens` 每次都 `chat_messages()` 重建）；:1665 还有一次结果丢弃的纯副作用调用；3228-3231 在 provider 不上报 usage 时每工具轮再做两次全量分词；图片附件每次从 SQLite 重取并重做 base64（:4092）。
6. **`src/tools/default_tools.rs:445-505`** — `glob_files`/`grep_text` 超时后未设 `kill_on_drop(true)`，rg 孤儿进程继续扫盘（对比 scripts.rs:563 已正确设置）。

## 3. CPU 性能

1. **`src/llm/openai_compatible.rs:5054`** — DSML 隐藏前缀未闭合期间每个流式 delta 对整个累积文本重扫（`find` 从 emitted 扫到末尾），O(n²)。GLM 明文工具参数路径：数百 KB × 数千 delta = 数 GB 内存扫描。`hidden_end_after` 每次调用还 `format!` 两个 String（:5266/5268）。修法：记忆化上次扫描位置。
2. **`src/platforms/file_reader.rs:180`** — UTF-8 修整逐字节 `pop()` + 整缓冲重校验，非法字节在 128KiB 中部约 4.3×10⁹ 次扫描，群文件工具非管理员可重复触发的 CPU DoS。一行修：`Utf8Error::valid_up_to()`。
3. **`src/cli.rs:1935,2332`** — 两处 O(n²) 字节搬移（`drain(..=newline)` 逐行 / tail 的 chunk 前置拼接）。`daemon logs -n 100000` 在 50MB 日志下拷贝约 150GB。
4. **`src/state/conversation_db.rs:2157`** — `append_tool_reports` 每追加一条整体读-解析-重写 JSON 数组，O(n²·report 大小) 写放大；SELECT 与 UPDATE 不在同一事务（跨进程连接可交错）。
5. **`src/platforms/onebot.rs:1009,2382`** — 每条入站消息对整个 AppConfig 两次全量深拷贝（含未触发回复的普通消息）；:1253 每帧完整 `serde_json::Value` 深拷贝只为覆写 self_id。
6. **`src/config_tui.rs:2346`** — provider 列每按一次 j/k spawn 线程发一次 `/v1/models`（60s 超时不可取消）。
7. **`src/tools/web.rs:1425,1584`** — 搜索跳转解析串行逐个 await，最坏 20×15s=300s。
8. **`src/platforms/mod.rs:2307`** — `strip_inline_markup` 每行物化 `Vec<char>` 且每个 `[` 向后线性扫，O(k·n)；:1777 截断前先全量转义拷贝 + `chars().count()` 全扫。
9. **`src/web.rs:3969,5023`** — 持全局 manager 锁做同步磁盘 IO（读全部 persona 文档 / 多次 SQLite 写 + 32MiB 附件搬运），慢盘时全 daemon 请求排队；async handler 内系统性同步 SQLite/std::fs（bootstrap 全量历史、4356、5357、8922、4551 等）。
10. **`src/tools/knowledge_base.rs:659-732`** — 每次关键词搜索全库逐文件整读 + 整文件 lowercase 拷贝，同步执行在 async 内；`best_window`（:1373）事件密集时 O(n²)。
11. **`src/web.rs:8600,2804`** — 每次为非当前会话取状态/每次 IPC ToolCall 都重建完整 Agent + 工具注册表（skills 目录扫描），无缓存。
12. **`src/render/mod.rs:400`** — 命令输出每 40ms 全量 clone + grapheme 级 wrap 全部保留行，单行字节数无上限（`MAX_LIVE_LINE_CHARS` 只管未换行的 current）；:2756 流缓冲每行整体重建 O(行数×字节)。
13. 杂项：`src/agent/mod.rs:1716` trim 循环内每轮全量重序列化工具定义并分词；`src/agent/compact.rs:268` 折叠文本至多 3 次重复拼接分词；`src/tools/memes.rs:870` 缓存命中仍整表 clone；`src/tools/web_images.rs:1737` 排序比较器内重复重计算；`src/platforms/avatar.rs:28` 等每调用新建 reqwest Client；`src/tools/task.rs:380` 每行日志重开文件；`src/config_tui.rs:8369` 每字符 `to_string()` 分配。

## 4. 内存问题

1. **`src/state/usage.rs:232`** — usage-history.jsonl 只增不减无轮转，每次统计 `read_to_string` 整文件 + 全量解析；daemon 跑数月后每次统计页 O(文件) 峰值。:494 为取 limit 条做 O(n log n) 全量排序；:356 缓存命中仍 clone 两条 String 做 key。
2. **`src/state/conversation_db.rs:5604`** — 中断回合 journal 永不清除（单条上限 64MiB），`load_visible_turns` 每轮上下文构建全量查询反序列化，热路径无界增长。
3. **`src/tools/jobs.rs:673`** — `job_status` 支持 offset 却整文件读入再切片；GB 级日志直接进 RAM，列表模式每任务一次全量读。
4. **`src/platforms/plugins/group_management.rs:936`** — `reason_history` 只增不减且高频被禁者永不淘汰，key 的 JSON 无界膨胀。
5. **`src/tools/weather.rs:658`** — 两个 static 缓存过期条目永不移除无容量上限。
6. **`src/tools/web.rs:1149` 等十余处** — 爬虫 `.text()` 无字节上限仅 15s 超时；`image_generation.rs:168`、`protondb_query.rs:385`、`archlinux.rs:573` 同类。
7. **`src/tools/knowledge_base.rs:629`** — 先整文件读入再校验大小，超大文件先撑 RAM 再被拒。
8. **`src/agent/mod.rs:3004`** — keepalive 快照持有整段会话（含 base64 图片）常驻，后台 ping 每次全量 clone（:1120），每工具轮再 3 次全量 clone（:2982,3011）。
9. 慢性泄漏：`src/web.rs:372` 登录限流 HashMap 只增不减（IPv6 轮换）；`src/cli.rs:11112` wake/run HashSet 无淘汰；`src/cli.rs:4900` clipboard_texts 磁盘缓存只增不清；`src/llm/openai_compatible.rs:1730,2506` 请求路径无条件深拷贝整段消息历史；`src/platforms/plugins/meme_collector.rs:207` 算哈希先 `to_vec()` 整拷贝（可直接 `&*data`）。
10. `src/state/mod.rs:1417` `history(limit)` 全量加载（含隐藏回合）后只取尾部；:1619 为定位单 turn 加载全会话，崩溃恢复 O(k·n)；`conversation_db.rs:4092` prune 把受保护尾部也全量载入。

## 5. 共性模式

- **async 内同步阻塞 IO**（应对齐项目已有的 `spawn_blocking` 先例）：`tools/clipboard.rs:93`、`platforms/file_reader.rs:125`、`tools/write.rs:42`、`edit_replace.rs:65`、`todowrite.rs:28`、`package_advisor.rs:47`、`tools/memory.rs:325`、`tools/alarm.rs:112`、`diagnostics.rs:1749`（无超时 pacman）、`default_kb.rs:122`（无超时 `git ls-remote` 在 REPL 启动路径）、`ipc.rs:1084`（无限期 flock）、`tools/memes.rs:503`、`src/memory/mod.rs:843,2399`（同步 SQLite 全表扫描/逐行 UPDATE 挂在高频路径）、`llm/cache_log.rs:87`、`platforms/plugins/real_context/*`（persona 每判次读盘、affection 每 turn 4-6 次序列化+写库）。
- **每调用新建 reqwest Client**：`exchange_rate.rs:41`、`man.rs:48`、`archlinux.rs:123`、`avatar.rs:28`、`web_images.rs:1431`（重定向每跳新建，最多 9 次/图）。

---

# 第二轮：专项维度

## 6. 并发 / 锁（新维度）

**架构背景**：所有 turn 跑在 actor 专用线程的 current_thread runtime + LocalSet（web.rs:6111），该线程上任何同步阻塞都冻结全部并发 turn。

1. **`src/agent/mod.rs:2449` + `models_cache.rs:823`** — `compact_now`（async）在缓存未加载时调 `refresh_blocking`：持全局 `REFRESH_LOCK` 做 reqwest::blocking 30s 网络请求 → actor 单线程 runtime 全局冻结最长 30s，所有会话所有 turn 停摆；后台刷新持锁时还要先等锁。修法：走已有的 `spawn_background_refresh` 或 `spawn_blocking`。
2. **`src/platforms/onebot.rs:5839`** — 全局单例 `file_store_lock` 跨 60s 文件下载持有，一个慢文件让所有群所有用户的文件下载排队（队头阻塞）。下载写临时文件无需持锁，容量清点落盘后再互斥即可。
3. **`src/agent/mod.rs:1113` + `web.rs:6628`** — per-turn Agent 无 Drop 取消 keepalive，turn 结束后孤儿任务继续发 20 次真实计费 ping 并驻留整段消息快照（默认 `keepalive_seconds=0` 关闭，开启才触发）。
4. **`src/web.rs:6110,6222`** — actor 命令通道 unbounded，本地 StartTurn `spawn_local` 无并发上限（平台 turn 有信号量），连发可积压。
5. **`src/tools/task.rs:370`** — unbounded channel + 每事件同步 open/write/close，从 turn 内启动时即跑在 actor 单线程 runtime 上。
6. **`src/tools/jobs.rs:374-407`** — ledger 锁外 read-merge-write，并发 sync 交错丢条目（内存 map 自愈，低）。
7. **排除项（可信的负面结论）**：全库 299 处锁守卫扫描未发现真实的跨 `.await` 持 std 锁；锁序单向（manager→conv_db 等）无死锁；OnceLock 缓存均为标准 double-checked；CAS/原子序用法正确。

## 7. token 计量端到端（量化）

三套口径：**A**（`effective_context_tokens`，实际发送字节，准确）、**B**（`turn_context_tokens`，trim 扣减，**漏 tool_flow**）、**C**（`turn_to_text`，压缩预算，**漏 tool_flow**）。

- **[已量化] 偏差幅度**：典型工具密集轮（4 次 bash 各 20k 字符输出）真实 22,700 tokens，B/C 只计 400——**57 倍**。
- **trim 侧**：想释放 0.05W 实际驱逐约 0.89W，最坏清空全部可驱逐历史（轻量场景也有 3-4 倍）。
- **[已量化] 压缩死循环**：C 口径下保尾预算 16,384 ≈ "40 轮"，40 轮真实约 90 万 tokens（7 倍窗口）→ 压后复查仍超 → 再压时 `find_cut_index` 返回 0 → `perform_compact` 返回 `Ok(None)`（compact.rs:258）→ **两个失败闩计数器都不累加，compact_stuck 永不触发**，每轮空转压缩事件；手动 `/compact` 走同一逻辑同样无效，**只剩 `/clear`**。
- 机械 prune（conversation_db.rs:4081）只折叠 `tool_reports` 从不碰 `tool_flow`，v20+ flow 轮 reports 多为空 → 对工具密集会话完全无效，压力全转嫁给付费压缩。
- 其他：图片按 765 tokens 平铺不看尺寸（实际 1.5k-5k+），多图会话触发线晚到；`turn_context_tokens` 无条件计 reports 而回放仅在 `tool_flow.is_empty()` 时发（mod.rs:4952 vs 3985，注释声称同步但未同步）；摘要轮 `token_total` 记的是生成摘要的 API 用量而非摘要体积（conversation_db.rs:4234）；服务端 `usage.prompt_tokens` 从不回馈校准；chat-template 每消息开销（`<|im_start|>`、Anthropic XML 包装）未计；`trim_at_ratio=0.8` 与 target 0.85W 之间 5% 死区；摘要失败降级为机械占位符时用户提示与成功逐字相同（compact.rs:371）。
- **修法方向**：B/C 补 tool_flow（用回放同源函数派生计数）；prune 扩展到 flow；A 口径用上轮 `usage.prompt_tokens` 做比例校准；`find_cut_index` 返回 0 时视为失败计入闩。

## 8. 前端（web/app.js）

1. **无限 resync 循环（高）** — `beginRunReplay`（:3573）置 `lastEventId=0` 后 `connectEventSource(0)`；daemon 发布超 4096 条事件后 `after=0` 必收 `resync_required` → bootstrap → 又有活动 run → 又 replay……无上限无退避。**刷新页面时若有回复进行中即触发**，本地回环每秒数轮 bootstrap + 全量 DOM 重建。修法：从 run 起点事件 id 重放，或 resync 连续 N 次即停。环形未滚动时则全量重放 ≤4096 条且被丢弃的事件也全部 `JSON.parse`（:7953）。
2. **流式 O(n²)（中）** — 每帧对累积全文全量 `renderMarkdown` 重建 DOM，KaTeX 每帧重算无缓存（:6156,4611）；reasoning 每 delta 全量重写 textContent + 全段 collect（:6298）。
3. **每秒全量轮询（中）** — `refreshViewSnapshot`（:7773）每秒拉全量 turns 并两次 `JSON.stringify` 对比。
4. **无虚拟化（中）** — `renderConversation` 每次 `replaceChildren` 全量重建；DOM + JS 字符串双份常驻（闭包持有 `live.assistantText` 与每工具 200KB raw）。
5. 低级项：人格图片 ObjectURL 不 revoke（:2168）；设置页每键全量序列化配置 + 重建模型池（:1158）；session 事件双重复建侧栏（:3647）；UTF-16 代理对截断切半 emoji（:6343,6440,2746）；头像缓存 `?v=Date.now()` 击穿（:1019）；隐藏状态仍加载壁纸（index.html:128）；usage 图表 innerHTML 未转义（:9185，低风险）。
6. **build.rs**：`rerun-if-changed` 绑整个 src+web，任何改动重跑 jieba/FST 秒级构建（:20）；重复词条静默覆盖（:73）。base64/XOR/o200k 断言均验证正确。

## 9. 重试 / 错误路径（新维度）

1. **`src/platforms/onebot.rs:1150-1166`** — 断连清理只有 `remove` + `writer.abort()`，从不 drain pending map；:6243 注释声称"断连即释放所有等待者"为假（消息处理器持有 ConnectionHandle 克隆，Arc 不归零）。断连后所有在途 API 调用各等满超时——带附件发送最长 **180s**，错误统一误报 "timed out"。修法一行：drain pending 使所有 tx drop。
2. **`src/llm/openai_compatible.rs:1697-1838`** — 外层端点循环（填充到 ≥3 次尝试）无 sleep、不查 `LLM_SCHEDULER.is_ready`，`mark_failure` 刚标 120s 冷却立即零间隔重发全量 prompt；流阶段错误（:2177,:2330）不经过内层 2s/4s 退避。持久 5xx = 3×3=9 次请求/18s，每次全量 clone + 序列化。`onebot.rs:2535` 审批解析重试 ×（3 端点×3 发送）再乘一层，一次审批最坏几十次补全。
3. **`src/llm/openai_compatible.rs:2848`** — Responses 截断裸 `bail!` 未包 `TransportFailure`（chat 路径 :2330 特意包了）：重试照发但冷却缺失，截断端点永不被降权。
4. **`src/llm/openai_compatible.rs:1546`** — keepalive 提示端点绕过冷却过滤（低）。
5. **[疑似] `src/agent/mod.rs:3358-3375`** — `finish_reason=length` 分支 `continue` 跳过 :3376 的续传簿记，旧 response id 跨轮残留，下轮 400 走自愈白费请求。修法：簿记移到 continue 前。
6. **[疑似] `src/agent/mod.rs:3159`** — supersede 后队列恰为空时无新输入直接重开一轮生成，偶发"没问却答"。
7. **[疑似] `src/llm/openai_compatible.rs:844,1655`** — 全冷却探测失败 `mark_failure` 覆盖式后移冷却截止，密集重试让端点永不出冷却（若非有意语义）。
8. **`src/platforms/onebot.rs:1166`** — `writer.abort()` 丢弃已入站帧（fire-and-forget 发送静默丢失，与 #1 同根）。
9. **[疑似] `src/platforms/onebot.rs:5541`** — 禁言窗口 QQ 服务端墙钟与本地时钟比较 + TTL 以 API 调用前时刻起算（≤3s 偏差）。
10. 良性确认：`chunk_tx` 背压丢弃（mod.rs:3016）核查无害；`wait_after`/`wait_for_reserved_ingress` 防丢唤醒写法正确；溢出恢复一次性屏障正确；并行 task 波次计数无空转；`PendingRedoGuard` 回滚正确。

---

# 已检查无问题（负面结论摘要）

- **算术安全**：全库系统性使用 `saturating_/checked_/clamp`；usage 累加全链路饱和；SQL 端 `COALESCE(MAX(seq),0)+1`；时间戳体系无混用；renderer 像素数学全 checked + `get_pixel_mut_checked`。
- **UTF-8 边界**：终端渲染按 grapheme/unicode-width；流式解析 char-boundary 回退齐全（`append_bounded` 等）；`\n` 切分不切多字节；jieba DP 索引一致；`to_ascii_lowercase` 字节等长映射安全。
- **有界缓存**：RecentImageLedger、RateWindow、GroupName/Mention/Mute/Role、TurnResourceCache(16 LRU)、EventHub(4096 环形)、subagent checkpoint(TTL+32)、message_recall 四层 TTL+上限、html_conversion 限额矩阵、web_images 下载/解码限额——均不无界。
- **死循环**：agent 主循环/REPL 事件循环/TUI 循环/迁移循环/organizer 退避/merge_summaries_tree 无进展守卫——均有界或有出口；`read_live_repl_input` 的 80ms 裸 poll 是针对 crossterm HUP 的刻意设计（有注释）。
- **shell 模块**：offset/consumed 全 char-boundary，`remove_source_block` 用 `get()` 探测，无越界无死循环。
- **前端**：SSE 原生重连带 Last-Event-Id 续传正确；165 处事件监听随节点回收；rAF/interval 清理齐全；id 全字符串比较无 2^53 风险；附件限额与 revoke（composer 路径）齐备。
- **锁**：无跨 await 持 std 锁；锁序单向；OnceLock double-checked 标准。

---

# 第三轮：动态验证（2026-08-16）

方法：运行项目测试套件 + rustc 探针程序（release 语义，逐字复刻涉事代码）+ 真实 release 二进制沙箱黑盒触发（`MIYU_HOME` 指向临时目录）。全部中间产物已清理。

## 基线

`cargo test`：**1454 单元测试 + 3 集成测试全部通过**，7 ignored。测试套件健康，以下缺陷均未被现有测试覆盖。

## 实证结果

### 全部命中的（4/4）

| 结论 | 验证方式 | 实测输出 |
|------|---------|---------|
| alarm `split_at` 中文 panic | **真实 release 二进制** `__alarm-worker --time "5分钟"`（沙箱） | `panicked: end byte index 6 is not a char boundary; it is inside '钟' (bytes 4..7)` |
| `{`/`}` 反序切片 panic | 探针复刻 judge.rs 三行模式，输入 `}abc{` | `panicked: byte range starts at 4 but ends at 1` |
| `strip_tagged_sections` 差一越界 | 探针逐字复刻函数，输入 `text before <system-reminder`（恰以前缀结尾） | `panicked: start byte index 29 is out of bounds for string of length 28` |
| alarm 乘法 release 回绕 | 探针（rustc -O），输入 `9999999999999999h` | 静默返回 `Ok(17553255926290444784)`，无 panic 无报错 |

对照组 `"5s"` 在真实二进制上正常运行（等待响铃后被 timeout 结束），确认 panic 非环境误伤。

### 机制实证（同形模拟）

- **math.rs 无深度递归**：与 `convert_sequence` 同形的非尾递归（返回值在调用后拼接，无法尾调用消除）在 **10 万～20 万层之间栈溢出 abort**（8MB 主线程栈；tokio worker 默认 2MB 栈阈值再低约 4 倍）。模型单条消息即可携带 10 万+ 个 `{`。注意：简单尾递归形状会被编译器优化成循环不溢出——真实函数的帧更大，阈值只会更低。
- **cli.rs:5013 第 10 张图丢失**：静态亲验 `strip_prefix(|c| c.is_ascii_digit())` 只剥一位数字，`[Image 10: ...]` 解析必为 None，静默丢弃。

## 校准与降级（动态实测修正静态估算）

现代 CPU + std 的 SIMD 加速让三个"高 CPU"项实测远低于第一轮估计，**降级处理**：

| 项 | 第一轮估计 | 实测（本机） | 修正后等级 |
|----|-----------|-------------|-----------|
| file_reader.rs:180 UTF-8 pop 循环 | "数秒 CPU，4.3×10⁹ 扫描" | 128KiB/非法字节居中：**59ms**（比 `valid_up_to` 慢 **1117 倍**） | 中（可重复触发的浪费，非秒级 DoS） |
| cli.rs:1935 drain O(n²) | "5MB≈6GB 拷贝，冻结数秒" | 2500 行×2KB（5.1MB）：**89ms**（比偏移切片慢 **360 倍**） | 中（随体量平方恶化，10MB≈350ms） |
| openai_compatible.rs:5054 DSML 重扫 | "数 GB 内存扫描" | 4000 delta×100B：累计扫描 1.9GB 仅 **38ms**（memchr 对多字节缓冲极快）+ 8000 次格式化分配 | 低-中（结构性 O(n²) 成立，当前体量下时间影响有限） |

panic 类结论不受影响——全部精确命中。

## 新发现（顺带）

- `parse_alarm_seconds` 不支持复合单位：`"1h30m"` 被 `split_at(len-1)` 切成 `"1h30"` + `"m"` → 解析失败返回 Err。模型需输出空格分隔（`"1h 30m"`）才行。健壮性小问题，非 panic。
- CLI 未知子命令会直接进入 REPL 并把参数当作对话内容发给 daemon（本次黑盒误触发的根源，属设计行为，测试时需注意）。

## 环境影响披露

首次黑盒探测（`alarm --help`）因未设置 `MIYU_HOME`，被 REPL 当作用户消息路由到了正在运行的生产 daemon，产生了**一轮真实对话**及少量 usage 记录（`~/.miyu/state/conversation.db-wal`、`usage-history.jsonl` 有对应写入）。未删除任何数据（删除用户数据风险大于收益）；如需清理可在 WebUI 中删除该条 `alarm --help` 会话。此后全部动态测试均在 `/tmp` 沙箱内进行。中间产物（探针程序、沙箱目录、核心转储）已全部删除；`target/` 内测试编译产物属正常运行缓存。

---

# 第四轮：逐行二次走查（正确性视角）

方法：对最大的 12 个文件（约 8.5 万行生产代码）做逐行第二遍走查，换正确性视角——逻辑反转、条件边界、复制粘贴错、状态机漏洞、静默吞错、协议边角、日期时区。每份报告附逐段覆盖清单。行号以走查时版本为准（代码在持续修改，±20 行漂移）。

## 高优先级新发现（均已抽查验证）

### 正确性 / 功能损坏

1. **`src/agent/mod.rs:1735,1748`** — `switch_mode`/`reload_config` 不刷新 `preset_dialogs`：切人格后每个请求仍注入**旧人格**的预设对话，Normal↔Dev 切换后 dialogs 永久错位（Dev 带人格 dialogs 违反设计；Dev→Normal 永远没有）。
2. **`src/agent/mod.rs:1980`** — redo 不清空上一修订的 `tool_flow`（`begin_redo` 的 UPDATE 唯独漏这列）：新修订无工具调用时，**旧修订的工具流被冒名回放**，模型看到从未发生的工具交互，并压掉新修订的问答对与工具报告。
3. **`src/state/conversation_db.rs:2270`** — redo 完成后 `replay_journal` 永不更新：重开 REPL 显示的是**被弃用的旧转写**而非最终回复（`session_replay` 优先用 entries）。每次 redo 必现。
4. **`src/config_tui.rs:389`** — `edit_plugin_detail` 检查 `index == 13` 但 `plugin_names()` 恰好 13 项（下标最大 12）：**api_quota 账号管理界面不可达**，整组编辑函数成死代码，只能手改 JSON。[已验证]
5. **`src/config.rs:348-477`** — qq_group_join_approval 的 `text_models` 不参与 provider/model 引用维护：删除/重命名被引用的 provider 后引用残留 → 保存时 validate 失败 → **整个 TUI 崩出，本次全部修改丢失**，不手改 JSON 无法恢复。顶层 `embedding` 同源遗漏（悬空引用静默失效记忆召回）。
6. **`src/cli.rs:4176`** — 多选模糊菜单（`/models`）的 **Esc 提交修改而非取消**（返回 `Ok(Some(active))`），与所有姊妹选择器语义相反；:4283 单选版搜索后回车选不中高亮项（`marked` 恒为 initial，fallback 分支不可达）。
7. **`src/cli.rs:436`** — `extract_debug_flag` 扫描 `--` 之前的全部参数：`miyu 你好 --debug` 的消息正文被截成"你好"并意外开 debug。
8. **`src/platforms/onebot.rs:6219`** — `upload_file_source` 用构造期快照 `self.conn` 而非 `self.connection()`：NapCat 重连换代后所有文件上传报 writer closed 失败，直到新消息重建 context。
9. **`src/platforms/onebot.rs:2629`** — 入群审批的 `filtered` 标记只写日志不参与分支：被过滤标记的请求照常走 LLM 审批，白耗并发额度与模型调用（意图显然是跳过，未接上）。
10. **`src/render/mod.rs:4180`** — 模型正文路径完全不过转义状态机（`normalize_stream_text` 只归一换行）：输出含 `\x1b[2J`/OSC 8 等时直接生效——清屏、藏光标、伪造 UI（命令输出有 sanitize，正文没有）。[已验证]
11. **`src/llm/openai_compatible.rs:3709`** — `AnthropicStreamDelta` 无 `stop_reason` 字段，`message_delta` 只合并 usage：**Anthropic 路径 finish_reason 永远 None**，`max_tokens` 截断的工具参数照常执行（该保护对 Anthropic 完全失效）。[已验证]
12. **`src/llm/openai_compatible.rs:946`** — `timeout_seconds.clamp(5,30)` 只用作 **connect_timeout**，主聊天路径无响应体/流空闲超时：服务器发完响应头后停发数据 → `stream.next()` 永久挂起；用户设 3600 也被截成 30s 且只管 TCP 连接。[已验证]

### 安全 / 运维

13. **`src/web.rs:401`** — 第 65 个登录令牌触发**全员登出**（超限 `sessions.clear()` 而非淘汰最旧）；无 logout 端点、token 永不过期。[已验证]
14. **`src/web.rs:10592`** — daemon 只监听 SIGINT：systemd/`kill` 的 **SIGTERM 直接跳过全部优雅停机**（运行中回合不落盘、IPC lease 不清理）。[已验证]
15. **`src/web.rs:10479`** — `origin_is_allowed` 用请求自带 `Host` 头构造期望 Origin：**DNS rebinding 可绕过 CSRF 校验**；叠加 :1834 默认绑定 `0.0.0.0` 且密码只能来自 CLI 参数（无 `-p` 即无认证公开监听）。
16. **`src/state/mod.rs:121`** — 平台授权索引是进程内一次性缓存：另一进程 grant/revoke 后 **daemon 直到重启都基于旧缓存**（撤销不生效）。

## 中低优先级新发现（按文件）

**cli.rs**：后台报告轮询用错会话键（11242，daemon 当前会话而非 REPL 会话）；两条输入路径 Esc 语义相反（12076 清空 vs 9548 永不清空）；零宽字符折行/光标宽度算法不一致（12761 vs 12792）；手写 `visible_width` 把变体选择符计 2 列（13311）；SIGTERM `process::exit(0)` 绕过 Drop 终端留在 raw mode（7185）；stdout 重定向时 REPL 启动失败（11044）；挂断时唤醒回合被误 Cancel（11540）；inline 菜单不滤 CONTROL 修饰符（Ctrl+Q 退出菜单，3271 等 4 处）；请求监控占位路径/关闭吞错（1893/1961）；Esc Esc 中断失败无反馈（5747）；stdin 截断按字节切可断 UTF-8（5228）；菜单布局 Resize 后陈旧（3213）；`/pop` 连查两次全量（7839）；follow 唤醒回合失败静默（11735）；`need_create` 死分支（4863）。

**web.rs**：`update_config` 预约前快照旧 config，并发保存互相静默回滚 secret（4463）；回合 setup 窗口期第二条消息吃 409 而非入队（5331）；resync 时向客户端谎称"回合已取消"实际继续跑（3539，可致双跑双计费）；平台票据 `acquire().expect` 可 panic（8725）；多回合冷启动 TurnEngineState 互相翻转（6566）；PATCH 双字段部分更新无回滚（3070）；ApplyConfig 等 5s 超时后照常迁移运行中会话（6193）；登录限速按 IP（NAT 互锁，373）；switch_actor_session 半提交（7538）；标题提示词嵌 13 连空格（7029）。

**agent/mod.rs**：trim 对 interrupted turn 漏算 `context_messages` 化石（1700）；附件读取吞错图片静默消失破坏前缀缓存对齐（4099）；question 计数在解析前累加+超限时兄弟工具收到矛盾指令（3388/3398/3737）；并行波次在 tools Mutex 锁内发事件做同步 SQLite 写（1382）；缺 usage 估算两份口径不一致（3228）；tool-limit 提前 return 丢弃已收集 artifact 候选（3324）；`%A` 恒英文星期（6109）。

**openai_compatible.rs**：`merge_anthropic_usage` 在 message_delta 只报 output 时丢失未缓存 input tokens（4952）；`<think>` 流式期按 Content 发出、finalize 才搬进 reasoning（流式 UI 与最终结果不一致，5042）；Anthropic 回放 tool_use 参数解析失败静默替换 `{}` 执行（3377，可能产生破坏性默认行为）；尾部无换行 `[DONE]` 被误判截断（2304）；keepalive 保留键漏 `max_tokens` 可双写（43/1575）；keepalive 记账 key_index 硬编码 0（1610）；`attr_value` 子串匹配命中 `username=` 之类（5376）；tool_call_id 缺失静默空串（3183）；Responses 空串 arguments 覆盖已累积参数（3919）；读体失败吞成空 body 丢失分类信号（1594 等 6 处）。

**onebot.rs**：@提及解析外层 3s 包内层 10s，取消时 pending echo 表项泄漏（1734）；`/reset`/`/stop` 票据获取失败 `.ok()` 吞掉状态机照常推进（3481/3618）；response_target 私聊被静默丢弃（6018）；语音/视频/转发段对模型完全不可见无占位符（5099）；CQ image 空 file 不回退 file_id 与另两处不一致（5131）；触发词大小写敏感/无词边界/空串恒真（4703）；退群/入群不进观察器只识别 kick（2079）；文件占位 id 跳号（1523）。

**state**：删除 subagent 会话不清 queued_prompts 孤儿行（1771）；persona 删除/改名漏同步多类键（737，REPL 指针静默丢失）；persona 指针校验不查 archived（610，潜伏）；未知 status 归一为 Running 但读点承认 'failed'（73/4512）；关键列 JSON 损坏被 unwrap_or_default 静默吞（5362 等 4 处）；队列身份回退子查询跨会话关联（4564）；stale 轮恢复一坏全坏 + DEFERRED 事务（4623）；`discard_stale_queued_prompts` 保护分支恒死代码（3681）。

**config/config_tui**：编辑任意模型静默切换 active_provider（2554）；config 保存非原子写（4020，断电留截断 JSON，项目其他处有原子写先例）；表单数字输错 `?` 崩出丢全部修改（7352 等）；legacy 限流键无条件覆盖新键（2190）；删除 provider 无确认+内置供应商复活（2514）；超时/窗口/温度输错静默取默认（7019/7222/7237）；serde default 与 Default 实现互相矛盾 4 处（3054 等）；"不保存退出"不回滚已落盘的提示词/人格文件编辑（1364）；单元素循环 `for pool in [&mut settings.text_models]` 是漏同步根因（355）；active_provider 被静默重置（4099）；`text_provider_model_choices` 忽略 default_model（4861）；textarea 编辑即提交（7629）；emoji id 列表被单值覆盖（5925）；新增供应商重置激活池（5542）。

**render/real_context**：todo 表格 `\|` 转义渲染端不识别列错位（4015）；表格宽度与文本 wrap 两套体系不一致 emoji 错位（3494）；todowrite 成功清空全部 tool_stats 波及并行工具（1342）；未闭合 OSC 吞掉其后全部输出（737）；`wrap_ansi_text` 非 SGR 转义整行宽度记 0（3470）；代码块围栏带尾随文本误闭合（2810）；math 块内 ``` 行先命中围栏分支（2810）；表格多出单元格静默丢弃（3384）；子代理事件解析失败静默（1497）；`accept_followup` 缺 normalize targets 无界增长（1295）；受保护昵称大小写/全半角敏感可绕过（2413）；affection 快照硬传播错误 DB 抖动毁整轮（1042）；可解析图片查询失败无日志降级（991）；judge 缺失维度按 0 分计入（424）；heat 无上限累积（1972）。

**memory/skills/ipc/platforms-mod**：organizer 反序切片 panic 杀线程后记忆整理静默停摆（311，已知模式的第 6 处确认）；组织批次不排除 forgotten 日记可"复活"为长期记忆（1111）；毒批次无重试上限每 5 分钟烧 LLM（209）；pending_events 死代码且缺归属列（1058）；清空外溢上下文不清 embedding 孤儿向量（559）；skills 发布锁 flock 无超时（1464）；目录上限实为 258>256（191）；可选字段空值判整个技能非法（672）；括号抑制只认全角（1367）。

## 与前三轮的关系

- 前三轮聚焦性能/溢出/并发/重试，本轮补上**业务逻辑正确性**——两类问题正交：本轮高优先级项多数不影响性能但直接损坏功能（redo 冒名回放、api_quota 界面死代码、配置保存崩出）或安全（SIGTERM、DNS rebinding、正文 VTE 注入）。
- 反序切片 `{`/`}` 家族新增确认：memory/mod.rs 与 onebot.rs 审批为已知，organizer.rs:311 为同模式第 6 处（线程级停摆后果）。
- 负面结论同样有价值：SQL 参数顺位、FTS 转义、衰减公式、ipc 帧边界、skills 指纹、RateWindow、SessionTurnTicket 锁序等已逐行核查无问题。

---

# 第五轮：中型文件逐行正确性走查

方法：把第四轮的正确性视角推广到全部剩余文件——plugins 全家（renderer/message_history/group_management/affection/judge/access_manager/message_recall/meme_collector/reply_processor）与整个 tools/ 目录（35 文件）、根目录杂项（question_tui/math/wait_spinner/models_cache/paths/shell/alarm/clipboard 等 24 文件），约 4.5 万行。每份报告附逐文件覆盖清单。

## 高价值新发现（均已抽查验证）

### 功能从未工作 / 语义错位

1. **`src/tools/diagnostics.rs:713`** — `/proc/net/unix` 解析列号差一：表头 8 列（Num RefCount Protocol Flags Type St **Inode** Path），`nth(7)` 取到 Path 却存入 `x11_inodes`，后续只与纯数字 token 匹配——**has_x11/has_wayland 恒 false，显示模式 socket 探测从未工作过**。应取 `nth(6)`。[本机 /proc/net/unix 实测验证]
2. **`src/platforms/plugins/message_history/tools.rs:1011`** — 工具参数 `all_groups`（名义"所有群聊"）与 `all_conversations` 完全等价，落到 `HistoryScope::Account` 含**全部私聊**：模型按参数名语义删"群历史"时会波及私聊，删除是破坏性操作且管理员确认短语显示的是 all_conversations。
3. **`src/tools/exchange_rate.rs:41`** — 付费 API 网络层失败（401/网络）经 `?` 直接返回，**免费 fallback 永不生效**（错误消息还声称有 fallback）；只有 200 但 result!=success 才走 fallback。
4. **`src/tools/image_generation.rs:200`** — rightcode 的 `("1:1", _) => 1024x1024` 是第一个匹配臂吞掉所有 resolution：**1:1 + 2K/4K 请求静默生成 1K 图**（其他比例都有 2K 分支）。
5. **`src/tools/man.rs:59`** — 搜索结果 URL 未在 href 闭合处截断：输出形如 `https://man.archlinux.org/man/systemd.1.en">systemd(1)</a>`，**链接带 HTML 垃圾直接不可用**。
6. **`src/tools/memes.rs:615`** — `update_meme` 把内置条目的相对路径（`images/expr_x.jpg`，相对内置目录）复制进用户 overlay，用户目录下不存在：**对内置表情做任何更新后 show 必失败**；且 disabled 条目被 `find_meme` 过滤，`enabled=true` 无法重新启用（只能 true→false 单向）；`collect_meme_from_local_image` 缺去重可造出同 id 双条目。
7. **`src/tools/apply_patch.rs:628`** — 行匹配首命中即用**无唯一性检查**（fallback substring 路径有 count>1 拒绝，主路径没有）：重复/样板代码块场景补丁落到第一处而非目标处，返回 ok。配套：CRLF 文件匹配成功后写回 LF 行造成混合行尾（652）；漏写 `@@` 头的块静默丢弃（303）；纯插入无锚点静默落到文件尾（552）。
8. **`src/platforms/plugins/real_context/judge.rs:455`** — `moderation_min_severity: 0.0` 合法（校验 0.0..=10.0）且使 `severity >= 0.0` 恒真：**所有消息被判违规**，正常模式退化为每条必回复。
9. **`src/tools/deep_research.rs:237`** — thinker 空产出时 `stop_reason="thinker_failed"` 但仍写报告并返回 `"ok": true` + 空结论——模型可能直接把空报告交付用户。
10. **`src/tools/registry.rs:411`** — `replace_script_tools` 删除名单不区分当前 occupant：MCP 工具与用户脚本同名时，任何一次脚本热重载会把 MCP 工具静默换回脚本版。
11. **`src/tools/knowledge_base.rs:481`** — remove 不存在的文件返回 `{"ok": true}`（DELETE 0 行也 Ok），模型误以为删除成功；导入失败文件被无提示丢弃无失败清单；`reject_non_kb_upload` 关键词子串匹配过宽（含 "memory allocator"/"configuration" 的正当笔记被拒）。
12. **`src/tools/todowrite.rs:182`** — `todo_write` 不校验 status/priority（update 路径有校验）：enum 外值静默入库，pending 计数错乱；文件非原子写 + 解析失败静默清零（28-42），崩溃可致任务清单无声消失。
13. **`src/platforms/plugins/group_management.rs:496`** — `enable_record=false` 只挡 GroupBan 落库，**踢人记录仍无条件写**（kick_one 与 GroupDecrease 分支无守卫）——同一开关对两类动作行为相反。
14. **`src/platforms/plugins/mod.rs:98`** — 敏感动作免确认门槛用**缓存** roster 判群管理员，执行校验用 fresh 查询：刚撤权的用户在一个缓存窗口内仍可跳过二次确认。
15. **`src/tools/diagnostics.rs:1400`** — launch probe 启动的 GUI 进程不回收（无 kill_on_drop），诊断会"弹出并常驻"一个应用。
16. **`src/tools/default_tools.rs:522`** — `timeout_seconds: 0` 立即超时（命令不执行），>120 静默钳到 120 长构建被误杀，schema 均未说明。
17. **`src/render/math.rs:368`** — `parenthesize` 把"两端有括号"误判为已保护：`\frac{(a)+(b)}{c}` 转写成 `(a)+(b)/c`，**数学语义反转**（应为 `((a)+(b))/c`）。
18. **`src/shell/fish.rs:162`** — `trap` 是 fish 3.6+ builtin：老 fish 下变未知命令 → 触发自己的 command_not_found 钩子 → **把 `trap __miyu_restore_cursor ...` 当用户消息发给 AI**（每次两次），光标恢复失效。
19. **`src/clipboard.rs:84`** — `has_image` 按任意 `image/*` 判定但读取硬编码 `image/png`：复制 JPEG/WebP 后粘贴**静默丢失**。
20. **`src/shell/mod.rs:28`** — 修改用户 `.bashrc`/`.zshrc` 用裸 `fs::write` 非原子（同文件已有原子写先例）：写回瞬间崩溃可截断 shell 启动文件。
21. **`src/tools/scripts.rs:129`** — index.json 单点损坏使**两个目录的全部**脚本工具失效（另一目录完好的也被放弃）。
22. **`src/tools/mcp.rs:56`** — server 启动/list 失败静默 continue 无日志，用户无从排查工具为何消失。
23. **`src/tools/weather.rs:100`** — 未识别的 query_type 静默归 Forecast；`days` 参数对 air_quality 完全无效（模型以为在控制）。
24. **`src/tools/archlinux.rs:767` + `caniplayonlinux_query.rs:721`** — 实体解码 `&amp;` 排第一造成双重解码（正确顺序是最后）；caniplay 的 tier/antiCheat 等结构化字段靠**整页文本子串匹配**，导航/页脚提及即误报。
25. **`src/tools/archlinux.rs:402`** — 找不到 AUR monitor 时拿第一个 monitor 冒充（状态页结构变化时错误归属）。
26. **`src/tools/package_advisor.rs:268`** — `foo.pkg.tar.zst.sig` 同样 `contains(".pkg.tar")`：签名文件可能被传给 `pacman -U`。
27. **`src/tools/protondb_query.rs:81`** — reports 拉取失败被伪装成 `total: 0`（error 字段写进兜底 JSON 但从未进入输出），"没人评论"与"拉取失败"不可区分。
28. **`src/platforms/plugins/meme_collector.rs:93`** — 抽样索引在 event.media 过滤序上，消费索引在 `message_images()`（会因超限/下载失败跳项）返回序上：**多图消息可能收错表情**。
29. **`src/tools/write.rs:42`** — 覆盖非 UTF-8 文件时 `unwrap_or_default` 把原文件当空串：diff 显示"全文新增"，真实旧内容丢失无提示。
30. **`src/tools/hash_codec.rs:27`** — "all"/"mainstream" 展开不含 blake3（描述却声称支持）；拼错算法名不报错混在 success 结果里。
31. **`src/tools/alarm.rs:84`** — id=`{毫秒}-{pid}` 同毫秒碰撞覆盖；cancel 先删记录再 kill，kill 失败时状态与实际矛盾。
32. **`src/tools/subagent_runner.rs:387`** — 估算 token 硬编码按精确值展示（绕过 `≈` 前缀机制）。
33. **`src/default_kb.rs:122`** — 更新检查失败静默但时间戳已写：离线一次后 24h 内不重试，提示滞后。
34. **`src/platforms/plugins/message_recall.rs:19`** — `scope.sent` 只写不读的死状态，`capture_outgoing_messages`/`max_messages_per_conversation` 两配置实际无效。
35. **`src/platforms/plugins/message_history/mod.rs:323`** — `record_external_bot_message` 的 reply_to 未走 `event_message_id` fallback：空 message_id 时整条校验失败，外部 bot 消息历史静默缺失。
36. **`src/platforms/plugins/real_context/affection.rs:1306`** — user_id 归一化拼接全部数字："abc12df34"→"1234" 可能静默命中错误档案。
37. **`src/tools/jobs.rs:577`** — subagent spawn 先 spawn 后 insert（与 spawn_background 相反），正确性依赖单线程 runtime 的隐式假设；锁内做全量日志 IO（877）；stop 的 ledger 除名时序可留孤儿（三重条件）。
38. **`src/tools/default_tools.rs:410`** — 每删一条路径做一次全回收站枚举，结果 `let _ =` 丢弃（纯浪费）。
39. **`src/tools/knowledge_base.rs:1166`** / `web_images.rs:226` / `moegirl.rs:67` / `web.rs:1852` 等 — 静默吞错/误导类小项（详见各代理报告）。

### 复核确认无问题（负面结论补充）

question/question_tui 全链索引与校验、math 映射表逐码位、models_cache 合并/众数/冲突取最小、paths 迁移 journal 回滚链、wait_spinner 状态机、token_counter BPE（catch_unwind 包裹）、shell token 解析、memes find_meme 短 id 歧义、kb edit_lines splice、scripts 超时/kill_on_drop、registry levenshtein、affection clamp 配置校验（min/max 顺序全部有 config 校验兜底，无 clamp panic 路径）、judge 权重全零已被 config 强制 weight_sum>EPSILON 兜住（**修正第一轮的 NaN 发现：实际不可达**）、message_history 删除确认令牌 turn 级内存不可跨轮重放。

---

# 修复建议排序（五轮合并）

1. **一行级速修**：alarm.rs:43（char 取末位）、onebot.rs:1150（drain pending）、man.rs:102、file_reader.rs:180（valid_up_to）、web.rs:10592（SIGTERM）、diagnostics.rs:713（nth(6)）、man.rs:59（URL 截断）。
2. **统一函数级**：6 处 `{`/`}` 反序切片（judge/affection/organizer/web_images/onebot 审批/memory）抽一个安全 JSON 提取器。
3. **功能损坏（用户可感知）**：redo 残留 tool_flow 与旧 replay_journal（agent 1980 + db 2270）、preset_dialogs 切人格不刷新（agent 1735）、api_quota 界面死代码（config_tui 389）、join-approval/embedding 引用维护遗漏致保存崩出（config 348/4725）、`/models` Esc 语义反转（cli 4176）、onebot 上传用旧连接句柄（6219）、memes update 损坏内置条目+禁用不可恢复（615/880）、apply_patch 首命中无唯一性检查（628）、exchange_rate fallback 死代码（41）、image_generation 1:1 分辨率被吞（200）。
4. **语义/权限错位**：all_groups 实为含私聊的全量范围（message_history tools 1011）、moderation_min_severity=0 恒违规（judge 455）、enable_record 只挡禁言不挡踢人（group_management 496）、确认门槛用缓存 roster（plugins/mod 98）。
5. **正确性根因**：token 口径补 tool_flow + prune 扩展 + find_cut_index=0 计入闩；Anthropic stop_reason 解析（openai 3709）；timeout_seconds 改为真请求超时（openai 946）；finish_reason 传递链修复（2293）。
6. **安全**：默认绑定改 loopback 或无密码告警 + Origin 校验改白名单（web 1834/10479）；正文路径接 sanitize 状态机（render 4180）；授权缓存加失效机制（state 121）；shell rc 原子写（shell/mod 28）。
7. **稳定性**：MCP 异步化+读超时+失败日志（56）；compact_now 的 refresh_blocking 改异步；strip_tagged_sections 差一；前端 resync 循环加上限；config/todowrite 原子写（config 4020/todowrite 28）。
8. **成本/体验**：嵌套重试补退避；AppConfig clone 改读快照；TUI 表单错误不崩出；65 令牌淘汰最旧；deep_research 空产出不返 ok（237）；kb 三项静默吞错（481/198/1166）。
9. **长期**：usage 轮转、journal 淘汰、前端虚拟化、usage 校准回路、forgotten 日记过滤、毒批次死信、fish 版本探测（shell/fish 162）。
