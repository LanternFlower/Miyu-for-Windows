# 04 · Agent 面向模型的语言统一为英文

## 1. 现状（已量化）

- `src/tools/descriptions/*.json`：61/61 个文件含中文，共约 54 KB。`ToolSpec::register` 会用这些 JSON **覆盖**代码里的 description/parameters，因此模型实际看到的是中文。
- `i18n::agent_text(en, zh)` 按系统 locale 返回中文或英文；大量 agent 侧提示（工具描述、错误回灌、子代理续跑、终局提示）依赖它。
- 代码中还有一批不走 `agent_text` 的中文硬编码（例如 `src/agent/turn_loop/mod.rs` 的 tool-limit warning、length 截断回灌、`subagent_runner.rs` 的 `finalization_prompt` 等）。
- UI 文案走 `i18n::text` 与 `localize`，**不受本方案影响**（用户说的是 AI 看到的提示，不是界面语言）。

## 2. 目标

1. 所有进入模型上下文/工具结果的工程性提示为英文。
2. 用户自己配置的 persona、dev-prompt、user identity、skill/script 描述**原样保留**。
3. UI 显示语言仍跟随 locale/配置。
4. 默认 Miyu 人格 `src/prompts/miyu.md` 是否英文化，见 D7（推荐：保留中文人格，但把其中工程指令拆成英文注入；若用户更在意中文思维链影响，则连人格一起英文化）。

## 3. 方案

### 3.1 分层

| 层 | 语言 | 改动 |
|---|---|---|
| UI（CLI/WebUI/REPL 展示） | 跟随 locale | 不动 |
| Agent-facing 工具契约 | English only | `descriptions/*.json` 全部翻译 `description` 与参数 description；`display_name` 保持中文供 UI |
| Agent-facing 动态提示/错误回灌 | English only | `agent_text()` 改为固定返回 en；或新增 `model_text()` 并全量替换 |
| 用户内容（persona、identity、kb、scripts） | 原样 | 不动 |

### 3.2 `agent_text` 处理

推荐最小语义改动：

```rust
pub fn agent_text(en: &'static str, _zh: &'static str) -> &'static str { en }
```

- 保留 `_zh` 参数避免一次性大规模调用点变更；后续 lint 可逐步删除中文参数。
- 新增测试：`agent_text` 在 `zh` 系统 locale 下仍返回英文；`text`（UI）仍按 locale。
- 硬编码中文改为英文常量，或迁移到 `agent_text`。

### 3.3 工具描述 JSON

- 61 个文件逐一手工翻译，不机翻后直接提交：参数名、enum 值、JSON Schema 结构不得改变。
- 重点保持“报错要说自己真正知道的”风格，并把 `AGENTS.md` 中已知兼容性提示写进英文描述（例如 `reference_images` 接受 stringified array、`questions` 必须是真数组等）。
- `groups.json` 的 summary 也是模型可见的 load_tools 目录摘要，一并英文化。
- `stub` 摘要自动取 description 第一行，因此英文后 stub 目录自动英文。

### 3.4 动态提示清单（实现时 grep 收口）

重点位置：

- `src/agent/turn_loop/mod.rs`：tool-limit warning、length 截断回灌、ask_question 错误期望形状、defer sibling tools。
- `src/tools/subagent_runner.rs`：`finalization_prompt`、resume 提示。
- `src/tools/load_tools.rs`：BASE_DESCRIPTION、stub 模式说明、execute 错误。
- `src/tools/skills.rs`、`src/tools/artifact.rs`、`src/tools/vision/mod.rs`、`src/tools/default_tools/command.rs` 等工具描述/错误。
- `src/agent/prompt.rs`：memory preamble、host environment 里的 LaTeX 说明。
- `src/agent/history.rs`、`src/agent/context.rs`：replay/summary 中面向模型的中文说明。
- `src/question.rs`：`assistant_exchange_text/user_exchange_text`（这两者是 UI 回显，可保留中文，但需要甄别）。

用脚本在 CI 中检查“模型可见面”而不是全仓库：
- 检查 `descriptions/*.json` 的 `description`/`parameters` 不含 CJK。
- 检查 `ChatMessage::system/plain/turn_context` 调用点传入的常量不含 CJK（白名单用户 prompt 文件与 KB 内容）。

### 3.5 与缓存/历史重置的关系

- 工具描述改变 = tools 数组冷启动一次，不触发 `reset_history`。
- 若翻译默认 `miyu.md`，`prompt_fingerprint` 变化会触发历史重置。**这是用户可见破坏**，必须作为 D7 的一部分确认，或使用 `reset_if_prompt_changed_with_compatible` 的白名单语义避免删历史。

## 4. 修改文件清单

- `src/i18n.rs`：`agent_text` 固定英文 + 测试。
- `src/tools/descriptions/*.json`（61）+ `groups.json`。
- `src/agent/`、`src/tools/`、`src/platforms/`、`src/llm/` 中所有 `agent_text` 调用与中文硬编码。
- `scripts/`：新增模型面 CJK 门禁（仅覆盖模型可见路径，不扫 UI）。

## 5. 验收

1. `MIYU_LANG=zh_CN` 启动，mock 抓取真实 provider 请求：tools description、system 注入、tool error 回灌均为英文；UI footer 仍是中文。
2. 61 个 description JSON 通过“模型面无 CJK”门禁；`display_name` 允许中文。
3. 用户自建 persona 中文内容逐字不变；`config/prompts/` 下用户文件不被脚本重写。
4. `bash scripts/refactor-check.sh` 全绿；两轮请求 cache log 无异常。
5. 用 dev-smoke 验证英文错误后模型重试行为不退化。
