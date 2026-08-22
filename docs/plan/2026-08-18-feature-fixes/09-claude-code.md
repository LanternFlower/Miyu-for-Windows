# 09 · 接入 Claude Code（使用其订阅登录态）

## 1. 现状与本机事实

- 本机安装 Claude Code `2.1.220`（`claude --version` 已验证），并已有 OAuth 订阅登录（`~/.claude.json` 存在 `oauthAccount`）。
- Miyu 当前只有自己的 provider/LLM 客户端，没有任何调用本地 `claude` CLI 的工具。
- Claude Code 提供稳定的 headless 入口：`claude -p --output-format json`；该模式使用用户既有 Claude 登录/订阅，非交互目录跳过 workspace trust 提示。

## 2. 推荐形态（D10）

新增一个独立工具 **`claude_code`**，而不是塞进 `task` tier：

- `task` 是 Miyu 自己的子代理循环，tier 语义是“模型档位”；Claude Code 是外部 agent runtime，输入输出、权限、会话续跑、成本模型都不同，混在一起会让两者都难维护。
- 只在本机 owner 面注册：终端/WebUI 本地会话（`platform_context.is_none()`），QQ 等通讯平台**不注册**，避免把订阅和本机权限暴露给群聊。
- 工具不在子代理中递归开放（加入 `SUBAGENT_EXCLUDED` 同族排除表）。

## 3. 工具设计

### 3.1 Schema（模型可见）

```json
{
  "name": "claude_code",
  "description": "Run the locally installed Claude Code CLI in headless mode using the user's Claude subscription login. ...",
  "parameters": {
    "type": "object",
    "properties": {
      "prompt": {"type": "string", "description": "Complete task prompt for Claude Code. Include required context; the CLI does not see Miyu's conversation unless explicitly included."},
      "cwd": {"type": "string", "description": "Optional working directory. Defaults to the current session workspace."},
      "model": {"type": "string", "description": "Optional Claude model alias or full name. Omit to use the user's default subscription model."},
      "append_system_prompt": {"type": "string", "description": "Optional extra system prompt appended to Claude Code's default."}
    },
    "required": ["prompt"],
    "additionalProperties": false
  }
}
```

### 3.2 执行语义

1. 用 `tokio::process::Command` 调 `claude`（从 PATH 解析；允许配置 `plugins.claude_code.binary` 覆盖）。
2. 参数：`-p --output-format json --permission-mode <configured>`；`cwd` 为会话 workspace；环境继承当前 Miyu daemon（含 HOME，才能读到订阅登录态）。
3. 超时：配置 `timeout_seconds` 默认 600，tool 层用 `tokio::time::timeout` 包裹，超时 kill 进程组。
4. 输出限制：stdout 上限（默认 512 KiB）；超限截断并注明，不把无限 CLI 输出灌进上下文。
5. 进程内互斥/并发：同一 session 同时最多 1 个 `claude_code`（默认），防止多个订阅会话并发抢额度；配置可调。
6. 错误分类：binary missing / timeout / 非零退出 / JSON 解析失败，返回英文错误并保留原始 tail。

### 3.3 返回内容

解析 `--output-format json` 的 `result` 字段作为工具输出；附上可选的：

```json
{
  "ok": true,
  "result": "...",
  "session_id": "claude session id if present",
  "cost_usd": 0.0,
  "duration_ms": 123,
  "truncated": false
}
```

具体字段以本机 Claude Code 实际 JSON 输出为准，实现时先用 `claude -p` 的文档/本地 schema 核实。

### 3.4 记账与审计

- 不把 Claude 用量塞进 Miyu `Usage`（那是 OpenAI 兼容 usage 口径）。新增独立统计：`claude_code` 调用写入隐藏审计会话（类似 subagent），字段含 `cost_usd`、`duration_ms`、模型、session id、prompt 长度。
- REPL/WebUI 显示 Claude Code 成本时明确标注 `$`，不与 token Σ 混加。

## 4. 配置

`plugins.claude_code`：

```jsonc
{
  "enabled": true,              // 默认推荐 true；注册时自动探测 binary，缺失时工具返回明确错误
  "binary": "",                 // 空 = PATH 上的 claude
  "permission_mode": "acceptEdits", // D17 待确认；可选 default/plan/acceptEdits/bypassPermissions
  "timeout_seconds": 600,
  "max_output_bytes": 524288,
  "max_concurrent_per_session": 1
}
```

## 5. 修改文件清单

- 新 `src/tools/claude_code/{mod.rs,runner.rs,audit.rs}`（保持小文件）。
- `src/tools/mod.rs`：注册与排除表。
- `src/web/turns/task.rs`：`platform_context.is_none()` 时按配置注册。
- `src/config/tool_plugins.rs`、`src/config/defaults.rs`、`src/config_tui/plugin_settings.rs`：配置项与 TUI。
- `src/state/*`：Claude Code 审计会话/统计列。
- `src/tools/descriptions/claude_code.json`（英文）。

## 6. 缓存与安全

- 该工具 schema 只在本机 owner 注册，不影响平台 restricted registry。
- 工具目录在本机会话内保持字节稳定，不要把当前 model/cost 拼进 description。
- 权限由注册面承担，不在 prompt 里写“允许/禁止”。

## 7. 验收

1. PATH 放一个假 `claude` 脚本（fixture），断言 Miyu 调用参数、cwd、env、超时 kill、JSON 解析与截断。
2. QQ 群/私聊会话的工具目录不含 `claude_code`；终端/WebUI 本地会话包含。
3. 真实环境手动 1 次最小调用由用户验证订阅可用（**测试不自动跑真实订阅**）。
4. 审计记录成本与模型，不污染 token Σ。
5. `bash scripts/refactor-check.sh` 全绿。

## 8. 待确认

- D17：默认 permission mode（推荐 `acceptEdits`；`plan` 更安全但体验割裂）。
- D18：默认启用还是默认关闭（推荐启用但自动探测 binary）。
- D19：是否允许 `claude_code` 递归调用 Claude Code 自己的 subagent（默认允许，由 Claude 内部处理）。

---

## 9. 施工记录（2026-08-20，claude-code 分支）

用户在 08-20 把范围从"独立工具"扩大为**三件套全做**（D10 改判），并限定
"仅本人渠道"。全部落地并经真机订阅验证：

### 交付物

1. **`claude-code` 供应商协议**（中转层核心，`src/llm/openai_compatible/claude_code/`
   四小件：mod/session/payload/stream）：
   - 传输 = `claude -p` 子进程 stream-json 双向流；`--system-prompt` 整体替换
     （人格原样过去，无 CLI 身份与 CLAUDE.md 注入,实测首请求仅 ~246 input tok）；
     内置工具全关（`--tools ""`）。
   - **会话续传**：进程内逐消息哈希链匹配 append-only 前缀,命中则 `--resume`
     只发增量（真机实测第二轮 stdin 仅一条新输入）；redo/compact/重启自动整段
     转写重放（`<conversation-history>` 块）；claude 侧会话丢失（"No conversation
     found"）一次性自愈重放。
   - 用量按 Anthropic 口径归一（prompt = input+cache_read+cache_write）,进现有
     cache-usage 记账;订阅限流/登录失效翻译为 429/401 进端点冷却与故障转移；
     流空闲看门狗杀进程组。辅助请求（compact/记忆整理等 scope≠chat）走
     `--no-session-persistence` 一次性会话,不挂桥。
   - 平台门禁：`with_platform_delivery` 在平台回合拒绝该协议端点（订阅条款）。
   - 校验豁免：该协议 `base_url` 可为空（io.rs）。
2. **`miyu mcp-serve`**（隐藏子命令，`src/cli/mcp_serve.rs`）：MCP stdio server,
   与 `miyu tool-call` 同源——daemon 存活走 IPC ToolCatalog/ToolCall（会话→模式
   →registry,guard 管线齐备）,直连本地兜底。`ToolCatalog` 新增 `full` 位一次拿
   全量合同。供应商自动以 `--mcp-config` 挂桥（env 显式带 MIYU_SESSION/
   MIYU_TURN_ORIGIN/MIYU_HOME/XDG_RUNTIME_DIR）。
3. **`claude_code` 委托工具**：按本文 §3 落地,偏离两处经用户同意——审计走
   JSONL（`logs/claude-code-usage.jsonl`）不建 DB 表；新增 `resume` 参数。
   D17=acceptEdits、D18=默认启用、D19=允许。
4. 配置 `plugins.claude_code`（工具与供应商共用）,TUI 协议下拉加 `claude-code`。

### 顺手修的存量 bug

- **工具桥在阅后即焚(ask)会话里全 404**：ToolCall/ToolCatalog 的会话解析只认
  user kind,单次 CLI 形态下 run_command 脚本调桥同样中招；改用 TURN_TARGET_KINDS。

### 真机验证（订阅 haiku,08-20）

| 验证项 | 结果 |
|---|---|
| 纯文本中转（一次性会话） | "回复 OK"→"OK",思考流正常透传 |
| 同会话第二轮续传 | `--resume` 命中,stdin 只有增量一条（testkit/claude-code/run.py,PASS） |
| MCP 工具闭环 | `sqrt(7.317)*ln(93.4)` 经 claude→mcp-serve→daemon→计算器,回答 12.272270123540252 与本地计算逐位一致 |
| MCP 握手/目录/调用（不花额度） | initialize/tools/list 59 个工具全合同/tools/call 3978 |
| 平台门禁/限流分类/binary 缺失/改写重放 | 假 claude 测具 5 项 + 会话链/载荷 4 项,全绿 |

已知限制：①订阅无按 token 计费,成本列仅供参考;②转写重放时人格预设对话会作
为历史进入 claude 上下文（模型可正确区分,但"上一条"语义可能指向预设）;③
`claude_code` 工具在中转模式下经 MCP 可见,存在 claude 套 claude 的理论递归
（D19 本就允许,深度由订阅限流自然约束）;④平台侧辅助请求（qq-judge 等）未
挂平台门禁,如平台会话把主池配成 claude-code 需自行避免。

## 10. 形态整改（2026-08-20 第二轮验收反馈）

用户裁定:Claude Code 不该是"协议"这个用户概念,应是**内置特殊供应商**。整改:

- 供应商列表天生含「Claude Code」条目(normalize 自动注入存量配置),**默认
  未启用**,列表行带「(未启用)」标;内置条目不可删除(删了下次加载也会回来)。
- 回车打开**专用编辑表单**:启用总开关/显示名/claude binary/MCP 工具桥开关/
  流空闲看门狗——没有 base_url/协议/API Key/超时/额外请求体。通用表单的协议
  下拉撤掉 claude-code(协议降级为内部实现细节)。
- **启用开关=总开关**:同时控制订阅中转可选与 claude_code 委托工具注册
  (plugins.claude_code.enabled 字段退役);未启用不进任何模型选择器,激活池
  指向它给明确的"供应商未启用"双语报错(端点装配/from_choices/new 三道)。
- 模型列表预置 CLI 别名 fable/opus/sonnet/haiku(默认 sonnet);模型浏览器对
  它跳过 HTTP /models 拉取直接回预置表。**思考档接通**:每模型
  low/medium/high/xhigh/max 五档(supported_reasoning_variants 协议自供,不走
  models.dev),选择经 thinking-variants.json 持久化,请求时映射 `--effort`。

真机验证(第二轮):注入+禁用报错(激活池指向禁用条目→"供应商未启用")/
启用后中转正常/`--effort low` 实测入参/TUI PTY 探针
(testkit/claude-code/tui_probe.py)四段全过——未启用标、专用表单无 HTTP 字
段、表单内启用+保存、落盘后启用态与预置模型完好。

## 11. 工具面双四档(2026-08-20 第三轮讨论定稿)

用户实测发现经桥的 Miyu 工具在 claude 侧执行、本就不走 Miyu 渲染管线,"保
渲染"不成立;裁定原生工具转正。定稿:

- **两个独立四档作用域**(off/dev/normal/all,按会话模式裁决,专用表单可改):
  `native_tools`(claude 自带 Bash/Edit/Read…,**默认 all**)与
  `miyu_tools`(MCP 桥,**默认 off**)。可叠加:dev 会话可同时拥有两边。
- 原生工具开启时:去掉 `--tools ""`,改传 `--permission-mode`(共用
  plugins.claude_code.permission_mode,**默认改为 bypassPermissions**——无头
  模式没有交互审批,acceptEdits 下 Bash 会被拒;委托工具同步吃这个默认)。
- **工作目录一律会话工作区**(workspace::effective_workdir,与 run_command
  同源),删除固定空目录设计;辅助请求(scope≠chat)保持无工具。
- 会话模式经 Agent 构造期 `with_claude_code_dev_mode` 打到客户端。
- 已知代价:原生工具跑长静默命令会撞流空闲看门狗(默认 300s,表单可调)。

真机验证:默认档下 claude 用自带 Bash 读取随机口令文件并原样回复(不可伪造),
请求日志实测 `--permission-mode bypassPermissions` 且无 `--tools`/`--mcp-config`;
作用域四组合单测(all/dev×normal 会话/dev×dev 会话/normal×dev 会话)全绿;
TUI 探针含新字段复测全过。

## 12. 第四轮验收整改(2026-08-20,用户实测六问题)

1. **清空联动**:续传映射织入 Miyu 会话 id(不同会话显式隔离,字节级撞链也
   不共用);/reset、平台清空、/wipe、删除会话六个入口全部联动
   `llm::forget_claude_code_session`——丢映射 + 尽力删除
   `~/.claude/projects/*/<会话id>.jsonl` 转录。真机:reset 后转录零残留。
   顺手修:**会话标题生成的 LLM 调用 scope 误标 "chat"**(08-10 调研 P2 旧
   账)改为 "session-title",在中转下不再产生游离的持久 claude 会话。
2. **上下文限制**:窗口不钉值(第五轮用户裁定:回猜测默认 168k,要改自己在模型菜单设);
   Miyu 的压缩管自己的账本,claude 侧真实上下文由其 autocompact 自管,Miyu
   压缩→链断→重开会话间接同步。
3. **桥工具图片**:新增 web/bridge_progress——桥内层调用改走带 progress 的
   执行,图片落 image asset(挂到该会话正在跑的 turn)并以 tool.image 事件直
   发 EventHub,REPL/WebUI 走既有渲染路;`prepare_for_external_output` 明确
   答 false(旧空 progress 误答 true,print_image 曾把图打进 daemon stdout)。
   真机:bridge_print_image 资产落库 + REPL 渲染痕迹。
4. **WebUI 工具过桥**:桥的 ToolCatalog/ToolCall 与回合装配同源补注册
   artifact 四件 + share_file(attach_owner_turn_tools,normal 表)。
5. 表单标签去掉档位括号。
6. **会话隔离**:见 1,映射按 (provider, model, miyu 会话) 三元隔离。

另:双四档默认改 native=all + miyu=normal;供应商列表 Claude Code 置顶
(default_templates + normalize 搬移,四个位置式引用测试改按 id 定位)。

**已知未修**(桥语义,与中转正交):经桥调用 Miyu `task` 的子代理工具集为空
(用户实测);ask_question 未过桥。`task` 已进去重剔除表,中转路径不受影响。
**排查铁律**:隔离测试 home 的配置文件会钉住旧默认(permission_mode/
miyu_tools 两次踩坑)——"默认没生效"先查配置残值。

## 13. 第五轮(2026-08-20 下午)

- 窗口钉撤销(用户裁定:回猜测默认,要改自己设);新增 **--autocompact 同步**:
  模型菜单显式配了窗口(100k–1M)就透传给 claude 的自动压缩阈值。
- 剔除表补 glob/grep/todowrite(claude 原生有 Glob/Grep/TodoWrite,此前人格
  声称"宿主没有"是错的);表单启用标签改「启用(中转 Claude Code)」。
- **工具透明度**:assistant 帧的 tool_use 入参(⚙)与 user 帧的工具结果(↳)
  截断摘要进思考通道,不再只有 [tool: 名字]。
- **wake.rs 补 tool.image 分支**(唤醒/shellhook 流此前静默丢图);桥泵日志
  升级(persisted=info/dropped=warn)。
- **"改动没生效"根因=幽灵 daemon**:旧进程二进制已删除态仍占 8300,
  `daemon stop` 报"未运行"却活着,用户 12:00 的测试全打在旧代码上。处理=
  kill 幽灵后干净重启;真机终验:kitty 图形字节实录+结果摘要行+回复确认。
  一次性会话的资产随阅后即焚级联删除属设计语义。

## 14. 第七轮(2026-08-20 下午,用户实测二批)

- **present_artifact 假 ok**:桥泵此前只接 Image,Artifact progress 被丢——
  补 Artifact 臂(save_artifact_asset+tool.artifact 事件,与图片同构);真机
  验证 artifact_assets 落 bridge_present_artifact_1。
- **上下文表 1.2M 假数**:claude 结果帧 usage 是整轮累计(多次工具迭代求
  和);表读数改用流内最后一次 message_start/delta 的真实 per-call usage
  (ChatResult.last_request_usage 由中转直供,回合层不再用轮累计覆盖,
  RoundUsage 优先取它)——这正是"拿 claude 侧数据"的正确来源。
- **ask_question 过桥**(bridge_question):对 QuestionBroker 复刻回合内问答
  流(question.requested 带活动 run_id→前端既有 UI→答案回 oneshot→问答对
  落 running turn);claude 侧 MCP 客户端超时经 MCP_TOOL_TIMEOUT 放宽 30 分
  钟。真机:claude 提问→REPL 弹窗→Closed 回传→claude 正确反应。
- **后台任务跟进**:claude 自己的后台/通知活在单次进程里,轮末即杀跟不了
  进;job/alarm 从去重表**请回**(Miyu 的 daemon 常驻+完成唤醒才是这套架构
  的后台);glob/grep/todowrite 按用户裁定入表。miyu_tools 默认改 **all**
  (dev 也挂,ask_question 尤其)。
- share_file 的 txt/md/log/json kind 改 "text";runtime 时间戳带时区(%:z,
  字节稳定);WebUI 音频/视频/文件卡片美化(web/shared.js+styles.css,待浏
  览器验收)。
- **已知限制(立项未做)**:①步间 followup——非 CLI 硬限制,stream-json
  stdin 可中途注入,但需要"常驻进程+排队注入"的重构;②WebUI 切走再切回,
  中转的工具卡片消失——重绘数据源 turn.tool_flow 与模型回放耦合,直接塞会
  污染回放/打断续传,需要 display-only 持久化车道(如 turns 新列)。

## 15. 第八轮(2026-08-20 傍晚)

- **task 经中转的子代理"空工具集"真相**:中转按设计丢弃外层工具定义
  (工具循环不外交),SubagentRunner 的 Miyu 工具对内层 claude 不可见;此前
  ephemeral 判定又把 subagent 作用域的原生工具/桥一并关掉,子代理彻底徒手。
  修=subagent 作用域按同一双四档给原生工具+MCP 桥,子代理成为"嵌套 claude
  代理"由内层自闭环(会话不持久保持)。真机:嵌套子代理经桥调
  scientific_calculator,不可心算表达式逐位吻合。
- **task 从去重表请回**:与 claude 原生 Task 语义不同——Miyu 子代理在
  daemon 里作为后台任务运行、完成唤醒跟进;claude 的 Task 活在单次进程里。
  task(发射)/job(查询停止)成对,不改名。
- WebUI 删除最后一个可见会话双生新会话:前端兜底守卫只防并发不防先后
  (deleteSession 与 session.deleted 事件先后各兜一次);按被删会话 id 加一
  次性闩锁(app.js,随前端批待浏览器验收)。
- 理论递归提示:外层 claude→mcp task→子代理→中转→内层 claude→mcp task…
  与 claude_code 工具的递归同属 D19 放行范畴,由订阅限流自然约束。

## 16. 第九轮(2026-08-20 晚)

- **切走丢卡片正式修复(display-only 车道)**:ToolFlowRound 加 `remote`
   标记(JSON 列免迁移)——中转侧工具活动随回合收集(Agent 泵挂钩)、落进
  tool_flow 供 UI 重绘,回放/估算/压缩三处消费点统一跳过 remote 轮,问答
  对回放门改按"有无原生轮"判定。真机:remote 轮落库且次轮 --resume 保持
  (增量 188B,回放未被污染)。share_file 富预览在重建路径复原(app.js
  hook,随前端批)。
- **中转环境事实注入**(常量字节):每轮一进程、自带后台/通知活不过本轮;
  桥在场时补 mcp__miyu__task/job/alarm 三句。第八轮 task 只改了注释没删数
  组项,本轮真正移出剔除表。真机后台闭环:task(background)发射→daemon 跑
  →完成唤醒新轮→claude 汇报回码,全链通。
- REPL 摘要行 ↳ 主题补 claude 原生工具白名单(Bash 命令/Read 等路径/
  WebFetch 安全 URL/Task 描述/ToolSearch query);原生 Bash 的结果以命令输
  出块渲染(与 run_command 同路),不再只有一行 ok。
- 上下文表读数第 2 处对齐;流式思考本就已接(thinking_delta→思考通道)。
- MCP 对外接入达成度:`miyu mcp-serve` 是标准 stdio server,任何 MCP 客户
  端可挂(env: MIYU_SESSION 指会话;MIYU_HOME/XDG_RUNTIME_DIR 必须与
  daemon 环境一致地传或不传)。

## 17. 第十轮(2026-08-20 下午)

- **Bash 展示与 run_command 完全对齐**(用户验收指出颜色不同/输出缺失):
  渲染层引入命令家族判定 is_command_tool(run_command|Bash),Bash 进
  CommandLiveDisplay(同一个 $ 标题行/↳ 命令/│ 输出尾巴,颜色同源);流侧
  Bash 输出不再折叠空白(truncate_block 保换行),WebUI 事件映射同步给
  Bash 分配输出尾巴,前端命令卡家族(is-command/图标/主题)纳入 Bash。
  真机(隔离 daemon+PTY+pyte 还原):`$ 运行命令×1 ok / ↳ printf … /
  │ BASHL1-3` 三行输出块齐全。
- **图片上方空两行修复**(用户截图点名):print_image_file 开头的前导空行
  删除——所有调用方都紧跟 prepare_for_external_output,摘要冻结已留一个
  空行,叠加即两行。真机:冻结行→↳ 主题→恰好一个空行→图形字节。
- 前端批(音频卡/多选/删末会话闩锁/share 重建)用户浏览器验收通过,已提交
  (21e63a02)。

## 18. 第十一轮(2026-08-20 傍晚)——WebUI 切换保活(§14 已知限制清偿)

- **回合进行中切走再切回不再丢已渲染输出**。根因:切会话时
  disposeAllLiveRuns 销毁直播状态,切回只能靠事件环重放,而环只留 4096 条,
  长回复的 delta 洪流必然把它冲掉(resync 两轮后放弃→空壳)。
- 修法=**离屏保活**:直播状态(liveRuns)跨切换保留,气泡 DOM 游离但事件照
  常写入;live 记归属会话(sessionId),reattachLiveArticles 按会话过滤重
  挂(停止按钮/待答问题卡一并归位);视图副作用(滚动 contentAdded/时间线
  挂载/问题坞/用户消息补渲/state.turns 注入)按 liveViewed 屏蔽;
  conversationRunning 只数本视图;restoreLiveRuns 只对全新空壳重放(保活
  的再放一遍=正文翻倍);**checkpoint 落库的部分正文与保活气泡是同一份**,
  被未结束 live 认领的 running 回合,持久化渲染只画用户消息
  (liveClaimsTurn)。
- 真机(playwright+chrome headless,隔离 daemon,订阅 haiku,sleep 45 造
  在跑窗口,__liveDebug 探针):切走前 conn=true→在B conn=false 且 B 无串
  台→切回 conn=true 原样重挂;OPENING 计数切换前后不变(不丢不重);离屏
  期间新增的工具卡 1→2 出现在保活气泡里;完成后 DONE 出现、二次切回计数
  不变。测具坑:断言用"切换前后计数不变"的不变式——思考签里模型会复述
  指令原文,绝对值断言全是假警报;测试会话要每轮新建(老会话人格对指令式
  提示词逆反)。

## 19. 第十二轮(2026-08-20 晚)——平台门禁撤销 + 侧栏拖拽排序

- **平台门禁撤销(用户裁定翻转)**:claude-code 语义是普通供应商/模型,QQ 等
  平台会话命中照常中转。撤除整套 claude_code_platform_blocked 机制
  (字段/builder/with_platform_delivery/task.rs 调用点/门禁测试/wiki 文案)。
  真机:隔离 daemon + 伪 NapCat(浏览器 WebSocket 连 /ws,loopback 空 token,
  应答 API 帧),admin 私聊消息 → 订阅 haiku 真回复(池里仅 claude-code,
  回复本身即中转铁证)。
- **侧栏会话拖拽排序**:v28 迁移加 sessions.sort_key(存量按"最近活跃在前"
  固化,间隔 1024;ALTER 前探列保证幂等——迁移测试的崩溃残留重放场景会
  撞重复列);列表 ORDER BY sort_key ASC,updated_at DESC;
  **sessions_with_dev 合并 normal+dev 后原本按 updated_at 重排,会覆盖
  sort_key 序**(真机 PUT ok 但 GET 不变的根因)——sort_key 进
  SessionRecord,合并排序改按它;新会话 sort_key=MIN-1024 插最前;
  PUT /api/sessions/order(IpcCommand::ReorderSessions)全量重写序 +
  session.reordered 事件广播;前端 HTML5 DnD(组内拖拽,drop-before/after
  主题色指示线,乐观重排+全量提交,失败回滚 refreshSessions)。
  **语义变化:排序固化后,会话活跃不再自动置顶,顺序只随拖动与新建变化。**
  真机(playwright DragEvent 走真实绑定链):第 3 项拖至首位生效,服务端
  GET 顺序持久化一致。
- 顺手:用户真机配置里第四轮 normalize 回填的 200k 窗口残值清除
  (haiku/sonnet/fable;opus=512000 是用户手设,保留)。
