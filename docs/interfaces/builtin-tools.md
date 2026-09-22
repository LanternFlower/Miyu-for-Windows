# 内置工具(as-built)

Rust 内置工具 = `ToolSpec` 注册 + `src/tools/descriptions/<name>.json` 描述真相源。
42 份 JSON 由 `crates/miyu-engine/src/tools/tool_descriptions.rs` 逐个 `include_str!` 编进二进制,`groups.json`
定义 `load_tools` 分组。

## 描述 JSON 字段

| 字段 | 语义 |
|---|---|
| `name` `display_name` `description` `parameters` | 模型面(英文,`check-model-english.sh` 把关) |
| `always_loaded` | stub 模式下也全量给 |
| `load_policy` | `summary`(默认,只给首句桩)/ `group` / `hidden` |
| `groups` | `load_tools` 分组 |
| `timeout_seconds` | 缺省吃 registry 默认;`0` 豁免(run_command / subagent 自管) |
| `trust` | `"external"` 也给不可信入口;缺省只给属主 |

**JSON 里没有权限字段**:权限只由 Rust 侧 `.writes()` / `.presentation()` 决定
(缺省 `ReadOnly`)。描述改一个字 = 工具面字节变 = 冷启动一次缓存。

## 组装流水线(`crates/miyu-engine/src/tools/compose.rs`)

```
compose_core        core 件 + 按 persona/config 开关的内置插件(files/jobs/apply_patch/todo/goal/…/memory)
subagent 快照       此刻 registry 的克隆 + cross_hints → 子代理面
ledger              注册位置即权限边界
compose_providers   scripts(外部场所只收 Trust: external)→ MCP → skills
ask_question        只在 surface.interactive_questions
load_tools          常驻
apply_surface_policy External: retain_trust + generate_image 追加一句;最后 cross_hints
```

注册顺序沿旧表,定义按名排序;`shape_tests` 钉着三个场所的 tools 数组逐字节。

## 登记表(09-16)

- `config::BUILTIN_PLUGINS`(`crates/miyu-base/src/config/builtin_plugins.rs`):每个内置插件一行——id、种类
  (Core / Builtin / Provider)、中文名与提示、可否在引导里勾、机器开关 `installed(&AppConfig)`。
  `PLUGIN_IDS`、`TOGGLE_PLUGINS`、`plugin_label` 从它派生(`const fn`)。
- `tools::builtin_plugins::REGISTRARS`(`crates/miyu-engine/src/tools/builtin_plugins.rs`):同一 id → 注册函数,
  `after_subagent_snapshot` 标子代理面不带的。config 是底座不能反向依赖 tools,所以是两张表,
  `registrars_cover_every_builtin` 钉住对齐。
- `compose_core` 挂 core 件与按快照裁决的子系统工具,然后 `register_enabled(…, false)` 挂表里
  的插件;`compose` 在子代理快照之后 `register_enabled(…, true)`。

## 新增一件内置工具

1. `src/tools/<name>.rs` 写 `register(&mut ToolRegistry, …)`,用 `ToolSpec::new` +
   `.writes()`/`.presentation()`/`.with_display_name()`/`.with_timeout_seconds()`;
2. `descriptions/<name>.json` + `tool_descriptions.rs` 的 `include_str!`;
3. `config::BUILTIN_PLUGINS` 一行 + `tools::builtin_plugins::REGISTRARS` 一行(插件类);
   纯 core 件直接写进 `compose_core`;
4. 中文 `display_name`(英文界面退到 id);
5. 跑 `shape_tests`、`token_diet_baseline`,并按"改了工具面"手测两轮 cache-usage。

平台作用域工具(看图、群管、发送)经 `PlatformToolContext` 窄 trait 拿平台信息,
不引用 `PlatformTurnContext` 本体;工具层与平台层的其它交互走 [宿主端口](host-capabilities.md)。
