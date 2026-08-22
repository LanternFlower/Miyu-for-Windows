# 13 · QQ 定时消息插件（qq_scheduled_messages）

> 状态：已实现。本文档记录设计定案与实现结构。

## 1. 需求与定案

- 配置会话 + 每天多个时间点 + **固定文本**，到点自动发送；不经过 AI、不耗 token。
- 错过的时点（停机/卡顿超窗）**跳过不补发**；同一时点去重防双发。
- 每个 tick 现读配置：启停插件、改任务表**无需重启**。
- AI 生成模式（把配置文字当提示词跑一轮）为未来可选扩展，一期不做。

## 2. 配置（`platforms.qq.plugins.qq_scheduled_messages`）

```jsonc
"qq_scheduled_messages": {
  "enabled": true,
  "settings": {
    "tasks": [
      {
        "conversation": "group:123456",          // 或 "private:10001"
        "times": ["08:30", "21:00"],             // 每天多个时间点，本地时区
        "message": "早安！",
        "days": ["mon", "tue", "wed", "thu", "fri"], // 可选，缺省=每天
        "account": 10001                          // 可选，多账号时指定；缺省=第一个已连接账号
      }
    ]
  }
}
```

校验器 `validate_qq_scheduled_messages_plugin_config`（`src/config/platform_plugins/mod.rs`）
在配置加载阶段拦格式错误；上限 64 任务、每任务 48 时点、消息 ≤4096 字符。

## 3. 实现结构

| 文件 | 职责 |
|---|---|
| `src/platforms/plugins/scheduled_messages/schedule.rs` | 纯时间计算：`HH:MM`/星期解析、`due_fires` 到期判定（含跨午夜窗口），无 IO，全单测 |
| `src/platforms/plugins/scheduled_messages/mod.rs` | 配置解析、tick 循环（20s tick / 70s 到期窗口）、fire-key 去重（按天清理） |
| `src/platforms/onebot/proactive.rs` | `send_direct_text`：不经回合直接构造 `OneBotAdapter` 发纯文本 |
| `src/web/server.rs` | daemon 启动时 `spawn_scheduled_message_worker` |

设计要点：

- **先记账再发送**：fire key 先进去重表再发，发送失败只告警不重试，避免每个 tick 撞一次失败。
- **不补发**：到期窗口 = (now−70s, now]，窗口外过期时点直接跳过。
- 插件在 `PlatformPluginRegistry::built_in()` 注册的是占位实现（无入站钩子），仅为让 `enabled` 开关与其它插件同一配置面。
- 定时发送的消息不进会话历史、AI 不感知（一期语义；若需要 AI 感知可后续接 `record_external_bot_message`）。

## 4. 验收

1. 配置一个 1-2 分钟后的时点，到点消息出现在目标会话；日志有 `scheduled message sent`。
2. 同一时点不双发（观察多 tick 覆盖同一分钟）。
3. 改配置（加时点/改文字/禁用）后不重启，下一 tick 生效。
4. 停机跨过时点后再启动，不补发。
5. `days` 掩码生效；`cargo test --lib scheduled_messages` 8 项全绿。
