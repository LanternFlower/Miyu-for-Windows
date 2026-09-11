# token 瘦身专项（token-diet）：调研结论与基线

日期：2026-08-21。目标：降低 Miyu 每回合 token 消耗，不改变功能与语义。三个方向：①工具输出 JSON→md；②精简工具描述；③精简工具 schema。

## 基线（真实 o200k 计数）

量尺：`cargo test --lib token_diet_baseline -- --ignored --nocapture`（src/tools/mod.rs 的 `token_diet_baseline_probe`，默认 AppConfig）。

| 模式 | 默认 stub 形态（每请求实付） | 全展开上限 |
|---|---|---|
| normal（REPL/daemon/WebUI owner） | 51 工具 = **3622 tok** | 8325 tok |
| dev | 15 工具 = **1737 tok** | 2959 tok |
| QQ 受限底座（不含平台内联工具） | 13 工具 = **1339 tok** | 2575 tok |

结构拆分（normal/stub 3622 tok）：
- 18 个常驻工具全文 ≈ 2300 tok，其中 task 416 / run_command 369 / use_meme 264 / load_tools 185
- 35 个懒加载 stub 行 ≈ 1300 tok（每行 ~40 tok，其中 ToolDefinition JSON 包壳 `{type,function:{name,description,parameters:{type:object}}}` 就占 ~25 tok）
- QQ 回合另加 ~20 个平台内联工具（全部 always_loaded 默认值），描述 6988 字符 + schema ~9KB，估算 **+2500~3000 tok**，QQ 每请求工具目录合计约 4000 tok
- load_skill 在 QQ 侧带动态技能目录：396 tok

工具**输出**侧基线（真实库 08-11~08-21，410 次调用，833KB）：
- 输出大头（web_search 52% / web_fetch / vision_analyze / run_command）**已是 md/纯文本**（08-17 改造）
- JSON 形态仅占输出字节 17.6%；其中结构开销（键名/括号/转义）46.8%，= 全部输出的 **8.2%**（理论上限）
- 浪费集中在三家（占可省空间 80%+）：todowrite（65% 开销，回显整表）、load_tools（54%，回显整份契约含 schema）、聊天历史类工具（31–57%，逐条重复键名）

## 想法 1（输出 md 化）可行性结论

协议层零障碍：三条线（openai-chat/responses/anthropic）的 tool result content 都是裸字符串，无 JSON 要求。但**全量 md 化性价比低**（上限 8%），且有 9 处本地功能代码解析输出 JSON（成败判定 `agent/artifacts.rs:9`、load_tools 加载清单 `agent/reports.rs:62`、read_clipboard 图片管线 `agent/images.rs:33`、artifact 自动发布、tool_report 持久化提取等）+ WebUI 两个面板（todos.js/shared.js）+ 一批测试。

**建议路线（定向打击，不做全量）**：
1. `todowrite`：不再回显全表 JSON，返回一行文本确认 + 变更摘要（WebUI 待办面板改走既有 `GET /api/sessions/{id}/todos`，todos.js 注释自证同构）
2. `load_tools`：不再回显整份契约（schema 白白重复一遍，模型下一请求本来就会在 tools 数组里看到全文），返回文本清单；`agent/reports.rs` 的加载解析改走结构化侧信道或文本解析
3. 聊天历史类（search_real_chat_history 等）：JSON 数组 → md 表/行格式，键名只出现一次
4. 历史兼容：旧回合 tool_flow 逐字节回放，旧 JSON 形态解析器保留（参照 render/command.rs:789 双兼容先例）

## 想法 2（描述精简）

- 待编辑稿：`descriptions-draft.md`（65 个 JSON 工具 14677 字符 + 23 个内联 6988 字符，按计费权重排序：常驻→懒加载→平台内联）
- 用户改完后由 Claude 解析写回：JSON 工具→descriptions/*.json；内联→Rust 字面量
- 注意：描述改动会一次性打破前缀缓存（新会话重建，属预期）；stub 模式字节稳定性契约不受影响（改的是常量本身）

## 想法 3（schema 精简）

- 审计表：`schema-audit.md`，用户在"裁定"列拍板
- 收益集中在常驻 + 平台内联工具；懒加载工具 stub 模式不发 schema，只挑离谱项（如 scientific_calculator 的单值枚举）
- 附带死资产清理项（edit_file.json 孤儿、share_file.json 未进宏、query_token_usage 重复常量）

## 想法 4（注入式 hint，用户追加）

94 条注入模板共 22,910 字符已提取进 `descriptions-draft.md` 第四节（带触发频率标注）。token 权重 TOP：
1. QQ system_context 四件套 ~1.9K 字符，**QQ 每请求全文重发**（identity.rs:424 / turn.rs:534-538）
2. load_tools/load_skill 动态目录框架 XML，hybrid/stub 下常驻 tools 数组，随条目线性放大
3. QQ 历史记录行格式：逐消息放大，预算上限 80K 字节，群聊绝对大头
4. compact 摘要提示词 2003 + fork 注入 756 字符，长会话高频
5. goal 续轮/收尾模板 ~500-620 字符/条，**append-only 永驻历史**，goal 驱动下每自主轮一条

## 额外发现（第五方向，待拍板）

stub 行瘦身：35 个 stub 每行 ~40 tok，`(stub entry, e.g. {...})` 后缀与 JSON 包壳均有压缩空间；normal 模式潜在 -300~500 tok/请求。属独立小项。

## 执行日志

- 08-21 A：附带项落地——stub 后缀 `(stub entry)`→`(stub)`（normal/stub 3622→3583 tok）；edit_file.json 删除；share_file.json 转正进宏（groups 归零保语义，补中文显示名"分享文件"）；query_token_usage 描述收敛为 usage_query.rs 共享常量。
- 08-21 B：想法 1 A 组落地（用户拍板"AI 看得到的才改，结构即功能不改"）：
  - **todowrite**：输出改一行确认（`todo list replaced: N item(s) — …`）；表格数据走 `__todo_table__` progress 侧信道（REPL tool_summary 拦截渲染，观感不变）+ WebUI 实时卡片/舞台面板改从 `GET /api/sessions/{id}/todos` 取（app.js attachLiveTodoPanel）；旧 JSON 回放两端解析器保留。历史回合卡片对新格式不再显示表格（快照不可得，已知取舍）。
  - **load_tools**：`loaded_targets:`/`loaded_tools:` 文本头 + 契约 `### name / 描述 / schema: 紧凑JSON` 渲染（stub 模式下契约是模型取参数的唯一渠道，保留但去 pretty-print 与转义）；agent/reports.rs 的 loaded_items/compact 报告双兼容解析。
  - **聊天历史三查询 + qq_group_manage_history_query**：逐条 JSON → 行格式（与 `<qq-history-format>` 同构：`[时间] 名字(QQ:号) [msg=id]: 内容`），跨会话查询带会话前缀；文件 media_id、reply-to、@mentions、撤回标记全保留；不可信字段过 safe_prompt_field 防伪造记录行。
  - **use_meme**：search 候选行格式；show → `sent meme {id}: {desc}`，compact_sent_meme_report 双兼容。
  - **task**：成功路径文本化（`result:` 之后为子代理结论本体，不再 JSON 转义）；错误路径保留 ok:false JSON（成败判定结构即功能）；tool_report 持久化提取双兼容。
  - **qq_group_manage 缓做**：输出与 `<qq-group-management>` hint 的 "success=true" 条款及非管理员确认流耦合，须与 hint 改动同批处理。
  - 全量 1634 测试绿。

- 08-21 C：想法 3 schema 裁定全部落地（schema-audit.md 用户裁定为准）：
  - JSON 侧删参：use_meme(width/height/library/limit)、web_search(provider)、web_fetch(timeout)、scientific_calculator(operation 单值枚举)、print_image(width/height)、search_web_images(preview_count)、check_issue(launch_timeout_seconds)；连带修掉 print_image.size 与 scientific_calculator 描述里的残留引用。
  - 内联侧删参（处理器一律保留兼容解析）：search_real_chat_history(user_id/group_id 别名)、delete_real_chat_history(group_id)、get_real_chat_activity_ranking(include_bot，用户裁定)、qq_group_manage(title duration)、qq_group_manage_history_query(sort_order)；job.offset 描述精简。
  - 用户裁定改留：run_command.title、manage_script 加载策略三参数。
  - 死资产：write_file/edit_string 模块+JSON 整删（用户裁定"apply_patch 一个顶所有"）；artifact_candidate_paths 的 write_file 旧输出回放分支与显示名 match 臂保留（历史回合仍要认）。
  - 基线：normal/stub 3622→**3465** tok，QQ 受限底座 1339→**1212** tok（platform 内联删参另计，不在此量尺内）；1625 测试绿（-9 为死代码自带测试）。

- 08-21 D：统一化专项第一批（unification-and-persona.md 决策清单，用户拍板 Edit/Read 统一+文风方向+重建 DB 需先查因）：
  - **claude_code 委托工具整删**（src/tools/claude_code/ + JSON + 宏行 + SUBAGENT_EXCLUDED；中转供应商线与 plugins.claude_code 配置保留，config_tui 表单保留）
  - **get_avatar 重设计**：必传 user_id/group_id 二选一；一律下载返回本地路径 + emit 图片；download 参数删除；输出文本化
  - **delete_real_chat_history 去两步确认**：单步执行；DeleteConfirmations/挑战码/短语复述机制整删；admin + live_admin_message 双门槛保留；输出改一行删除统计
  - 1612 测试绿（-22 为随死代码/确认流删除的测试）
  - 生图 bug 修复（B/C/D）与 DB 重建待损坏根因取证结论后同批执行

- 08-21 E：大合批（用户全绿灯）：
  - **描述定稿落盘**：51 个 JSON + 12 处内联 Rust 按用户中文定稿译回英文（文风规范：短句、无分号串联）；发言排行输出顺手行格式化（B 组）。
  - **DB 重建**：corrupt 三件套保全至 state/corrupt-2026-08-21/，.recover 重建 integrity ok，473 turns/65 sessions 零丢失，daemon 轮换 QQ 重连。
  - **损坏根因取证**（机理已复现）：08-18 引入的 open 时 incremental_vacuum × wal/主文件配对破坏（fs::copy 备份掉 POSIX 锁 + SIGKILL 孤 wal + 人工动库文件）；硬件/文件系统排除。
  - **根因修复**：.bak 改 VACUUM INTO；quick_check 失败跳过 vacuum + 启动 error 亮相；freelist<64 页不跑 vacuum（放大器降频）；B（落库失败 error+事件）/C（print_image 说真话）一并落地。遗留跟进：优雅退出显式 checkpoint、子代理审计复用 StateStore。
  - **Edit/Read 统一（方案 C）落地**：`edit`（原 apply_patch，认 artifact:/kb: 命名空间，kb 写路由 import_file/remove 索引不绕过，内容守卫保留）+ `read`（原 read_file，artifact:/kb: 前缀，artifact: 裸前缀=列清单）；create_artifact/apply_artifact_patch/read_artifact/upload/edit_kb/read_kb/remove_kb 七工具退场；present_artifact/search_knowledge_base 保留；claude_code 工具移除；get_avatar 重设计；delete_real_chat_history 去两步确认；消费者全部双兼容（spill 豁免/artifact 发布/持久化报告/REPL/WebUI 前端）。
  - **最终基线**：normal/stub 3622→**3073** tok（−15%），normal/full 8325→6728，dev 1737→1484，QQ 底座 1339→1199；WebUI 会话另减 3 个常驻 artifact 工具（约 −430 tok/请求）；QQ 内联描述精简另计。1612 测试绿。
  - 待办移交下一批：hint/注入文风改写（94 条已标注）+ persona-ab A/B 实验;docs/wiki 06/15 的工具清单过期需重写。

- 08-21 F：收尾批（文风 hint + WebUI 图片卡 bug + wiki）：
  - **hint 文风批**：QQ 常驻四件套精简 25-40%（身份policy/历史格式×2/历史图片/引用语义，语义点逐条保留）；中文注入英文化（撤回规则/群管规则/审核初判/身份冒用警告/长回复转图/private_tool_memory/reasoning 包装/spill 提示/占位符——同语言同语域污染最毒的全部退出人格语域）；群管 hint 与 success=true 字面解耦；load_tools stub 描述 −45%；技能目录/加载目录/script_summary XML 全部单行化；LaTeX 说明 −45%；群历史行时间戳 [HH:MM:SS]→[HH:MM]（逐条消息生效）；webui 标题 prompt 13 连空格修复。人格相关 hint（persona-reminder/简短条款/表情包提醒）与 goal/compact 模板一字未动。
  - **判不做修正**：qq_group_manage 输出文本化——hint 解耦后收益仅 ~6KB/月,确认流+聚合重试语义改造风险不成比例（不要为了优化而优化）。
  - **WebUI 图片工具卡永久"进行中" bug**：根因=桥路径(claude-code 中转/脚本内 tool-call)的 tool.image 用合成 id `bridge_{tool}_{seq}` 惰性建卡,系统里不存在该 id 的 tool.finished;修复=bridge_progress 在 image/artifact 事件后补发同 id 的 tool.finished（成败如实）。普通回合内直调不受影响本就正常。
  - **wiki**：06-内置工具与插件整篇按现状重写（54 内置+18 平台，幻觉工具名清零）；15-扩展指南修订 4 处并新增"编辑/读取类能力优先并入 edit/read 命名空间"。
  - **实验说明**：persona-ab 现有测具为干净体制（tools/platforms 剥离），测不到 QQ 注入变化；QQ 体制 A/B 需伪 NapCat 挡板（后续项）。本批全部为语义保真的翻译/精简，每条可独立回滚。
  - **最终基线**：normal/stub **3040** tok（专项起点 3622，−16%）；QQ 受限底座 **1104**（−17.5%）；QQ 真实回合另享四件套/历史行/内联描述的叠加节省。1612 测试绿。

- 08-21 G：验收失败复查——用户侧 `cargo test` 挂的是 `origin_tty_gates_and_writeback_against_real_pty`（PTY 测试，与本专项改动无关的既有 flaky，重负载下三中二复现）。双层根因：①python 父进程在子进程完成 login_tty 的 dup 前就读 `/proc/{pid}/fd/0`，把继承管道的错误路径一次性交给 Rust（之后重试无救）；②Rust 侧无等待直接断言。修复：python 侧轮询等 fd/0 变成 /dev/pts/ 再上报 + Rust 侧 2s 轮询断言。高负载（并行 release 编译）全量六连绿。

- 08-21 H：验收失败第二轮——用户挂的是三个 math 渲染测试。根因：**既有环境敏感**（git 溯源确认来自 main 的 561a888d）——`kitty_graphics_supported()` 读开发者终端的 TERM，在 kitty 里跑 `cargo test` 时数学块改走图形协议路径，半块断言必挂；沙箱 TERM=tmux-256color 所以此前一直绿。修复：该探测在测试构建（cfg!(test)）恒 false——跑测试的终端不该改变测试结果；kitty 序列生成要测直接调 render_math_kitty。验证：默认环境与 TERM=xterm-kitty+KITTY_WINDOW_ID 全环境模拟各两连全量 1612 绿。

## 验收方式

改动前后各跑一次 `token_diet_baseline` 量尺对比三模式 tok 数；输出侧用 stub-LLM 测具跑 todowrite/load_tools 回合对比 tool result 字节；1637 项全量测试绿。

## 归档说明（08-23）

本目录的工作稿（描述编辑稿 / schema 审计表 / 统一化与文风方案 / 输出全量分析 / schema dump）已随专项收官删除，定论收敛于仓库根 AGENTS.md《项目注意事项》与本文执行日志。仍有约束力的遗留对照：

- 输出改造缓做名单：generate_image / search_web_images（投递管线依赖结构字段）、deep_research（内部管线单独评估）；"结构即功能"留 JSON 清单：写入类工具（edit/kb/artifact 的 ok/files）、artifact 发布、ask_question、read_clipboard、share_file、send_message_to_user、goal 三件、alarm/manage_skill/manage_meme 等 CRUD 确认。
- schema 用户裁定改留：run_command.title、task.max_steps / resume_id、manage_script 加载策略三参数。
