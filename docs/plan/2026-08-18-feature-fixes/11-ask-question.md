# 11 · ask_question 兼容性与 always_loaded

## 1. 根因

- `ask_question` 是唯一完全绕过 `ToolRegistry::call` 的工具（在 `agent/turn_loop/mod.rs` 中按名字特判），因此注册表层的 `coerce_declared_shapes` 到不了它。
- `descriptions/ask_question.json` 中 `always_loaded: false`。
- 默认 `tools.loading_mode = "stub"` 时，模型看到的 `ask_question` 只有：
  - 60 字符摘要（中文描述首行），
  - `{"type":"object"}` 宽松参数壳。
  真实 schema（`questions` 数组、`header/question/options/label/description`、required 等）必须通过 `load_tools` 额外获取。模型经常在只看到 stub 时直接调用，于是出现 `question`/`questions` 形状错误、选项序列化成字符串等问题。
- `QuestionRequest::parse` 已经做了两层补救（`questions` 字符串数组还原、`options` 字符串数组还原），说明线上确实在吃这类参数。

## 2. 方案

### 2.1 ask_question 在交互面常驻完整契约

- `descriptions/ask_question.json` 改为 `"always_loaded": true`。
- 只影响“会注册 ask_question 的交互面”：本地 REPL/WebUI（`run_turn_task` 中 `platform_context.is_none()` 才注册）。平台 restricted registry 和子代理不注册，因此额外 schema 成本不会扩散到群聊/后台。
- stub 模式下 `always_loaded` 工具本身就会发完整 definition，无需改 registry。

### 2.2 保持并加强兼容解析

`QuestionRequest::parse` 继续：
- 把字符串化的 `questions`/`options` 数组还原；
- 对常见错误形状返回**带期望形状**的英文错误（现有错误已经带期望形状，随 04 项英文化）。

实现时先加日志/审计统计“parse 失败形状”，不要盲目接受单数 `question` 字段；等真实数据再决定是否宽容。

### 2.3 错误回灌与重试

- 第一次 parse 失败时，tool error 继续回灌给模型；错误文案保持“期望整数/数组，收到 X”风格。
- 同一 turn 内错误重试超过 2 次仍失败，停止执行 ask_question 并返回 `unavailable_tool_output`，避免无限打转。

### 2.4 回归测试

- 构造 stub mode registry（注册 ask_question），断言 `stub_definitions()` 中 ask_question 的 schema 是完整 `questions` 数组 schema，而不是 `{"type":"object"}`。
- hybrid/full 模式首轮 definitions 含完整 ask_question。
- 平台/子代理 registry 不含 ask_question。
- `QuestionRequest::parse` 增加 fixture：合法、`questions` 字符串数组、嵌套 `options` 字符串数组、缺字段、错误类型。

## 3. 修改文件清单

- `src/tools/descriptions/ask_question.json`：`always_loaded: true` + 英文描述（随 04）。
- `src/agent/turn_loop/mod.rs`：重试上限与错误文案。
- `src/question.rs`：如需新增形状兼容，收口在 `parse`。
- `src/tools/tests` 或 `src/agent/tests/queue_journal.rs`：契约与解析测试。

## 4. 验收

1. REPL/WebUI 交互会话首轮请求即可看到完整 ask_question schema。
2. 模型传 `questions` 为字符串数组仍可解析并弹问题面板。
3. 平台会话与子代理 tools 中无 ask_question。
4. 契约变化后 tools 数组在会话内字节稳定；`bash scripts/refactor-check.sh` 全绿。
