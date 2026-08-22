# 12 · 通讯平台生图限流（每会话每请求一张）

## 1. 现状（已定位）

- `generate_image` 一次调用只生成一张，但同一 turn 内可以连续调用多次；没有 turn 级计数。
- 工具注册发生在 `restricted_platform_registry` / `normal_tools`，注册表在配置缓存中跨 turn 复用，不能把计数器直接放进共享 `ToolRegistry`，否则会话之间互相污染。
- 管理员与私聊白名单有现成判定材料：
  - `PlatformTurnContext::is_admin`
  - `access_control::AccessPermission::PrivateWhitelist`（静态 + 动态 grant）
  - 但 `PlatformToolContext` trait 当前没有“私聊白名单”方法。

## 2. 方案

### 2.1 turn 级任务本地限流器

在 `src/tools/workspace.rs` 增加：

```rust
tokio::task_local! {
    static IMAGE_GEN_LIMIT: Option<Arc<ImageGenLimit>>;
}
struct ImageGenLimit {
    remaining: AtomicUsize,   // usize::MAX 表示豁免无限
}
```

- `with_image_gen_limit(remaining, future)` 包裹整个 turn 任务。
- `try_allow_image()`：非平台 turn（无 task-local）→ 放行；有 task-local 且 `remaining > 0` → CAS 减一；否则返回英文错误：

```
image generation limit reached: only one image per user request in messaging-platform conversations. Wait for the next user message before generating another.
```

- `src/tools/image_generation.rs::generate_image` 在解析参数后、发 API 前调用 `try_allow_image()`；本地 REPL/WebUI 无 task-local，保持无限。

### 2.2 豁免判定

`run_turn_task` 在 `profile.platform` 存在时：

```text
exempt = context.is_admin()
       || (conversation.kind == private && private_whitelist_authorized(...))
```

给 `PlatformToolContext` 增加 `is_private_whitelist()`（或更通用的 `image_generation_unlimited()`），实现放在 `src/platforms/tool_context.rs`：
- private conversation 才可能为 true；
- 静态 `config.private_chats.whitelist.contains(sender)` 或动态 `has_dynamic_access(... PrivateWhitelist ...)`。
- **不依赖** `allow_non_admin_host_tools`：该开关控制宿主工具，与生图豁免解耦（用户需求明确私聊白名单豁免）。

### 2.3 计数范围（D12）

- 推荐语义：**一个用户请求（一个 user turn）一张**。同一 turn 内的 queued follow-up 是否重置，按用户决策：
  - 推荐每个 queued prompt 重置一次（更符合“每次请求”）。
  - 实现上在 `consume_queued_prompts` 的 segment 边界重建 task-local 计数；因为 task-local 包裹的是整 turn future，需要在 consume 点更新 `Arc` 内部计数器，而不是重新 scope。
- 后台/子代理不涉及 `generate_image`（task 排除表已有），无需处理。

### 2.4 工具描述

- 平台与本地共用同一 description，避免 tools 数组因表面不同而漂移。
- description 只写通用语义；限额由代码承担，不写进 prompt 求模型自觉（`AGENTS.md` 1.3）。
- 可以在平台注册后 `amend_description` 追加一句静态英文说明“one image per user request in platform chats”，该追加必须字节稳定且所有平台会话一致。

## 3. 修改文件清单

- `src/tools/workspace.rs`：task-local 限流器。
- `src/tools/image_generation.rs`：`try_allow_image()` 埋点与错误。
- `src/platform_types.rs`、`src/platforms/tool_context.rs`：白名单判定方法。
- `src/web/turns/task.rs`：平台 turn 计算 exempt/remaining 并包裹 task-local。
- `src/web/actor/mod.rs`：确保 job-wake 等合成平台 turn 也走同一包装。
- `src/platforms/turn_run.rs`：平台 turn 启动路径若不经 `run_turn_task` 的 profile 也要覆盖（逐调用点 grep 后收口）。
- 测试：`src/tools/tests/image_limit.rs`、`src/platforms/tests/tools.rs`。

## 4. 验收

1. 群聊非管理员：同 turn 第一次 `generate_image` 成功，第二次返回 limit 错误，不发 API。
2. 下一个用户消息（新 turn）计数重置，可再生成一张。
3. 管理员与私聊白名单豁免；普通私聊用户（非白名单）受限。
4. 终端/WebUI 本地会话不触发限流。
5. 两个并发平台 turn 互不干扰（各自 task-local）。
6. 限流错误可证伪：临时去掉埋点，回归测试报红。
