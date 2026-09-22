# 平台 hook(as-built)

`crates/miyu-hosts/src/platforms/plugins/mod.rs::PlatformPlugin`:平台生命周期插件。与 scripts / MCP /
子系统不是一回事——它挂在场所流水线(入站 → 准入 → 触发 → 回合 → 投递)上,
启用集也不由 `PersonaManifest.plugins` 管。

## 描述符

`PluginDescriptor { id: &'static str, priority: i32, default_enabled: bool }`。
同一 hook 按 `priority` 排序;同优先级按注册顺序。

## hook 表(按流水线时序)

| hook | 阶段 | 说明 |
|---|---|---|
| `observe_ingress` | 传输层收到消息,准入之前 | 存档用,必须轻;失败只记日志 |
| `handle_command` | 准入后 | 平台命令(`/…`)拦截 |
| `preempt_inbound` / `turn_is_superseded` / `confirm_supersede` | 取 FIFO 租约前 | 让新消息抢占旧回合;不得改历史或 agent 状态 |
| `observe_inbound` | 准入后 | 看全部准入事件(含静默群消息、撤回) |
| `accept_followup` / `decide_trigger` | 触发判定 | 群里回不回 |
| `register_tools` | 组工具面 | 往平台作用域 registry 加工具 |
| `before_turn` | 回合开始前 | 包装 `PlatformTurnInput.content`(群聊记录块等);**`original` 快照不可改** |
| `turn_started` / `after_turn_aborted` | 回合生命周期 | |
| `before_send` / `after_send` | 投递 | `PreparedSend` 的 primary / after_success / fallback;图片 digest 与文字 bigram 幂等闸在投递层 |
| `after_session_reset` / `after_persona_reset` | 重置 | |
| `record_external_bot_message` | 其它 bot 的消息 | |

## 现状里要写明的边界

- 稳定 system 内容与动态回合注入分开;`memory_content` 不可改。
- hook 失败策略**逐个不同**:多数记 warn 继续,`before_turn` 若中途失败,已经就地改过的
  `content` 会留给后续流程——列为审计项(计划 §4.5),不在本轮改。
- 注册表在 daemon 启动时构造一次(`PlatformPluginRegistry::new`),回合经 `PlatformTurnContext` 持有同一份;
  每次调 hook 前按 `plugin_enabled(id, default_enabled)` 现判启用(配置驱动),所以开关改动对下一次 hook 调用即生效。

## 验收

`cargo test --lib platforms::plugins`、`platforms::tests::reply`、`platforms::onebot::tests::delivery`;
黑盒 `testkit/qq-*`。
