# 死代码清单与清理结果(2026-09-16)

来源:临时去掉 `src/lib.rs` 的 `#![allow(dead_code)]` 跑 `cargo check --lib`(不含测试),编译器报的 never used / never read。
用户批准后按下面的两阶段法清理(第一轮的候选清单见 git 历史):只被测试、其它 bin(`miyu-voice`)、feature、`include_str`、反射式名字用到的项会被误报,
每条删前都要按「删冗余先确认」走:搜全部调用方 → 加删前红测 → 删实现/描述 JSON/注册 → 全量测试 → 更新基线。

共 177 条,涉及 130 个文件。


## 清理方法(两阶段,全靠编译器裁决)

1. 把编译器在 lib 构建里报 never used 的每个函数 / 方法 / 常量都加上 `#[cfg(test)]`,`cargo check --all-targets` 必须仍然通过——
   通不过的是死链里没被单独报出来的调用方(`multiple methods are never used` 那几组),补标后再过;
2. 去掉全局 `allow(dead_code)` 跑一次 **测试构建**(`cargo check --lib --tests`):连测试都不用的 → 删;只有测试在用的 → 保留 `#[cfg(test)]`。
3. 字段 / 变体 / 结构体逐个看:协议与 serde 的 DTO 字段一律保留(`platform_types`、`ipc`、`conversation_db` 行类型、`config_api` 请求体、`embedding` 清单);
   内部结构体里从未读的字段删(`CatalogJob.started`、`CompactResult.transcript`、`TurnUpdateReceipt.session_id`、`PrivatePersona.username`);
   从未构造的删(`SurfaceTrust::Internal`、`web::dashboards::qq::ConversationQuery`);只在测试构造的标 `#[cfg(test)]`(`EditResult`、`ToolGroupDescription`);
   RAII 守卫字段不是死的,改下划线前缀或 `allow` 并写明(`live_turns::_host_tools_guard`、`scheduling::SessionTurnGuard`)。
4. `cargo fix` 清掉随之失效的 `use`;只被测试代码用到的 import 补 `#[cfg(test)]`。

结果:删除 88 项;104 项只有测试在用,保留为 `#[cfg(test)]`。

**第二批(同日,用户批准「连测试一起删」)**:把 104 项整体删掉跑测试构建,按编译错误定位每条测试的真实引用,再看函数体裁决——
29 项删除(16 项独立死逻辑连 11 条专用测试一起删;13 项薄包装把测试调用点改成直接调生产函数后删),
75 项是测试夹具(测试拿它们摆状态去测活代码,删了就是删覆盖)保留 `#[cfg(test)]`。明细见
`2026-09-16-core-normal-interfaces.md` 第七轮。下面「保留为 cfg(test)」一节里已删的 29 项:
`recent_daemon_log_lines` `format_daemon_log_line` `set_token_usage` `full` `rename_platform_provider_references`
`host_environment_block_with` `host_environment_block` `archive_book` `admission_for` `reset_if_prompt_changed`
`parse_patch` `ensure_editable_file_path` `edit_file` `edit_lines` `refresh_semantic_after_write` `dynamic_description`
`call_mcp_tool` `list_server_tools` `register_chat` `register_readonly` `xml_escape` `load_target_tool_xml`
`refresh_skills` `readable_subagent_log_line` `group_summary` `loadable_tools` `load_targets_xml` `script_summary_xml`
`clone_filtered`;之后又删 `persona_manifest.rs` 的 `script_enabled` `skill_enabled` `mcp_server_enabled` `allowed`(白名单裁决的重复实现)。

## 已删除

| 文件 | 符号 |
|---|---|
| `src/agent/setup.rs` | `conversation_usage_tokens` |
| `src/cli/inline_picker.rs` | `truncate_display` |
| `src/cli/repl/layout.rs` | `repl_cursor_position` |
| `src/cli/repl/tail/mod.rs` | `session_empty` |
| `src/cli/repl/tail/screen/expand.rs` | `collapse_all` |
| `src/cli/repl/tail/screen/mod.rs` | `is_suspended` |
| `src/cli/repl/tail/screen/term.rs` | `content_rows` |
| `src/cli/repl/tail/screen/toast.rs` | `command_hint_open` |
| `src/cli/repl/tail/screen/toast.rs` | `toast_active` |
| `src/cli/repl/tail/screen/toast.rs` | `toast_rows` |
| `src/cli/select.rs` | `inline_single_select` |
| `src/config/defaults.rs` | `default_subagent_max_tool_steps` |
| `src/config/platform.rs` | `model_route_mut` |
| `src/config/platform.rs` | `prune_pool` |
| `src/config/platform.rs` | `remove_model_route` |
| `src/config/platform.rs` | `rename_model_references` |
| `src/config/platform.rs` | `rename_model_references` |
| `src/config/platform_ops.rs` | `rename_platform_model_references` |
| `src/config/pool_ref.rs` | `rename_model` |
| `src/config/provider.rs` | `supports_vision` |
| `src/config/provider_ops.rs` | `set_active_provider_model` |
| `src/config_tui/platforms/id_lists.rs` | `format_id_list` |
| `src/config_tui/providers.rs` | `select_model_pool` |
| `src/config_tui/real_context/mod.rs` | `real_context_media_mode_label` |
| `src/config_tui/real_context/mod.rs` | `real_context_media_mode_value` |
| `src/config_tui/tiers.rs` | `tier_display_name` |
| `src/i18n.rs` | `detect` |
| `src/ipc/launch.rs` | `web_access_urls` |
| `src/ledger/books.rs` | `format_balance` |
| `src/ledger/books.rs` | `rename_book` |
| `src/ledger/books.rs` | `today` |
| `src/ledger/categories.rs` | `archive_category` |
| `src/ledger/mod.rs` | `open` |
| `src/ledger/money.rs` | `format_with_currency` |
| `src/llm/mod.rs` | `add` |
| `src/memory/evicted.rs` | `config_provider` |
| `src/models_cache/lookup.rs` | `input_modalities_blocking` |
| `src/oobe/probe.rs` | `probe_now` |
| `src/paths/resource_migration.rs` | `ensure_destination_ancestors` |
| `src/persona_hint.rs` | `reminder_message` |
| `src/platforms/logging.rs` | `format_platform_tool_payload` |
| `src/platforms/logging.rs` | `truncate_platform_reply_log` |
| `src/platforms/plugins/group_management/args.rs` | `kick_schema` |
| `src/platforms/plugins/group_management/args.rs` | `reason_schema` |
| `src/platforms/plugins/message_history/tools/args.rs` | `explicit_or_current_group` |
| `src/platforms/plugins/real_context/affection/logging.rs` | `tag_change_suffix` |
| `src/platforms/plugins/real_context/history.rs` | `normalized_timestamp` |
| `src/platforms/plugins/real_context/runtime.rs` | `account_key` |
| `src/platforms/turn_context.rs` | `record_external_bot_message` |
| `src/question_tui/mod.rs` | `ask` |
| `src/render/command.rs` | `write_command_block` |
| `src/render/stream/timeline.rs` | `note_start` |
| `src/render/usage.rs` | `usage_total` |
| `src/runtime/run.rs` | `session_has_redo` |
| `src/runtime/stores.rs` | `admin` |
| `src/state/conversation_db/history.rs` | `reset_history` |
| `src/state/conversation_db/mod.rs` | `attachments_dir` |
| `src/state/conversation_db/sessions.rs` | `session_token_total` |
| `src/state/conversation_db/turns.rs` | `has_any_running_turns` |
| `src/state/history.rs` | `sponsor_record` |
| `src/state/mod.rs` | `with_usage_account` |
| `src/state/queue.rs` | `consume_queued_prompts_with_model` |
| `src/state/queue.rs` | `remove_queued_prompt` |
| `src/state/turns.rs` | `has_any_running_turns` |
| `src/state/usage_ops.rs` | `session_cumulative_tokens` |
| `src/state/usage_ops.rs` | `usage_details` |
| `src/tools/default_tools/mod.rs` | `path_kind` |
| `src/tools/http_response.rs` | `read_text_prefix` |
| `src/tools/jobs/mod.rs` | `status_display` |
| `src/tools/knowledge_base/files.rs` | `read_file_readonly` |
| `src/tools/knowledge_base/store.rs` | `slug` |
| `src/tools/registry/mod.rs` | `script_scope` |
| `src/tools/registry/mod.rs` | `set_script_allowlist` |
| `src/tools/subagent.rs` | `record` |
| `src/tools/subagent_runner.rs` | `clip_inline` |
| `src/tools/subagent_runner.rs` | `clone_inner` |
| `src/tools/subagent_runner.rs` | `run` |
| `src/web/actor/mod.rs` | `ensure_actor_agent` |
| `src/web/assets.rs` | `asset_response` |
| `src/web/assets.rs` | `binary_asset` |
| `src/web/assets.rs` | `text_asset` |
| `src/web/attachments.rs` | `attachment_delivery` |
| `src/web/goal_driver.rs` | `session_has_goal_round_run` |
| `src/web/persona.rs` | `canonical_prompt_documents` |
| `src/web/persona.rs` | `prompt_configuration_changed` |
| `src/web/persona.rs` | `prompt_documents_changed` |
| `src/web/sessions.rs` | `require_no_running_turn` |
| `src/web/voice_bridge.rs` | `daemon_state` |

## 保留为 `#[cfg(test)]`(只有测试在用)

| 文件 | 符号 |
|---|---|
| `src/cli/daemon_log.rs` | `format_daemon_log_line` |
| `src/cli/daemon_log.rs` | `recent_daemon_log_lines` |
| `src/cli/footer.rs` | `set_token_usage` |
| `src/cli/repl/input_layout.rs` | `full` |
| `src/cli/repl/tail/screen/ansi.rs` | `raw` |
| `src/config/persona_manifest.rs` | `allowed` |
| `src/config/persona_manifest.rs` | `mcp_server_enabled` |
| `src/config/persona_manifest.rs` | `script_enabled` |
| `src/config/persona_manifest.rs` | `skill_enabled` |
| `src/config/platform_ops.rs` | `rename_platform_provider_references` |
| `src/config/pool_ref.rs` | `explicit_models_mut` |
| `src/config/provider.rs` | `role_is_explicit` |
| `src/config/provider_ops.rs` | `active_context_window` |
| `src/config/provider_ops.rs` | `antigravity_enabled` |
| `src/config/provider_ops.rs` | `claude_code_enabled` |
| `src/config/provider_ops.rs` | `codex_enabled` |
| `src/embedding/manifest.rs` | `DEFAULT_LOCAL_MODEL` |
| `src/host_info.rs` | `host_environment_block` |
| `src/host_info.rs` | `host_environment_block_with` |
| `src/ledger/books.rs` | `archive_account` |
| `src/ledger/books.rs` | `archive_book` |
| `src/llm/openai_compatible/endpoints.rs` | `detached` |
| `src/memory/write.rs` | `flush_pending_events` |
| `src/memory/write.rs` | `remember_pending_event` |
| `src/paths/legacy_migration.rs` | `daemon_is_running_at` |
| `src/paths/legacy_migration.rs` | `migrate_entry` |
| `src/paths/resource_migration.rs` | `migrate_resource_layout` |
| `src/paths/resource_migration.rs` | `write_resource_journal` |
| `src/platform_types.rs` | `quoted` |
| `src/platforms/mod.rs` | `acquire_session_turn` |
| `src/platforms/onebot/admission.rs` | `admission_for` |
| `src/platforms/onebot/dispatch.rs` | `handle_message` |
| `src/platforms/onebot/dispatch.rs` | `message_event` |
| `src/platforms/plugins/message_history/store/mod.rs` | `db_path` |
| `src/platforms/plugins/message_history/store/types.rs` | `account_scope` |
| `src/platforms/plugins/message_history/store/types.rs` | `conversation_kind` |
| `src/platforms/plugins/renderer/layout.rs` | `plan_columns` |
| `src/platforms/turn_context.rs` | `response_target` |
| `src/platforms/turn_context.rs` | `take_final_reply_suppression` |
| `src/render/math/mod.rs` | `render_math` |
| `src/render/math/raster.rs` | `halfblock_art` |
| `src/runtime/state.rs` | `is_authenticated` |
| `src/state/accounts.rs` | `count_accounts` |
| `src/state/accounts.rs` | `ensure_bootstrap_admin` |
| `src/state/assets.rs` | `save_user_attachment` |
| `src/state/assets.rs` | `user_attachment_path` |
| `src/state/conversation_db/accounts.rs` | `count_accounts` |
| `src/state/conversation_db/attachments.rs` | `insert_user_attachment` |
| `src/state/conversation_db/history.rs` | `load_session_loaded_items_with_sources` |
| `src/state/conversation_db/mod.rs` | `open` |
| `src/state/conversation_db/platform.rs` | `bind_platform_session` |
| `src/state/conversation_db/platform.rs` | `plugin_delete_scope` |
| `src/state/conversation_db/platform.rs` | `unbind_platform_session` |
| `src/state/conversation_db/queue/consume.rs` | `consume_queued_prompts` |
| `src/state/conversation_db/sessions.rs` | `find_session_by_name` |
| `src/state/conversation_db/turns.rs` | `append_tool_report` |
| `src/state/history.rs` | `load_session_loaded_tools_with_sources` |
| `src/state/history.rs` | `reset_if_prompt_changed` |
| `src/state/queue.rs` | `enqueue_prompt_for_target` |
| `src/state/sessions.rs` | `add_platform_access_grant` |
| `src/state/sessions.rs` | `bind_platform_session` |
| `src/state/sessions.rs` | `find_session_by_name` |
| `src/state/sessions.rs` | `platform_access_grants` |
| `src/state/sessions.rs` | `plugin_delete_scope` |
| `src/state/sessions.rs` | `remove_platform_access_grant` |
| `src/state/sessions.rs` | `unbind_platform_session` |
| `src/state/turns.rs` | `append_persisted_context` |
| `src/state/usage.rs` | `record_usage` |
| `src/state/usage.rs` | `record_usage_at` |
| `src/state/usage.rs` | `usage_details` |
| `src/tools/apply_patch.rs` | `parse_patch` |
| `src/tools/artifact.rs` | `ensure_private_dir` |
| `src/tools/artifact.rs` | `validate_file_name` |
| `src/tools/default_tools/files.rs` | `edit_file` |
| `src/tools/default_tools/mod.rs` | `ensure_editable_file_path` |
| `src/tools/knowledge_base/dashboard.rs` | `dashboard_root` |
| `src/tools/knowledge_base/files.rs` | `edit_lines` |
| `src/tools/knowledge_base/index.rs` | `refresh_semantic_after_write` |
| `src/tools/load_tools.rs` | `dynamic_description` |
| `src/tools/mcp.rs` | `call_mcp_tool` |
| `src/tools/mcp.rs` | `list_server_tools` |
| `src/tools/memes/mod.rs` | `register_chat` |
| `src/tools/memory.rs` | `register_readonly` |
| `src/tools/mod.rs` | `builtin_registry` |
| `src/tools/mod.rs` | `dev_registry` |
| `src/tools/registry/lazy.rs` | `empty_parameters` |
| `src/tools/registry/lazy.rs` | `load_target_tool_xml` |
| `src/tools/registry/lazy.rs` | `xml_escape` |
| `src/tools/registry/mod.rs` | `clone_filtered` |
| `src/tools/registry/mod.rs` | `lazy_definitions` |
| `src/tools/registry/mod.rs` | `load_targets_xml` |
| `src/tools/registry/mod.rs` | `loadable_tools` |
| `src/tools/registry/mod.rs` | `permission` |
| `src/tools/registry/mod.rs` | `script_summary_xml` |
| `src/tools/registry/spec.rs` | `with_timeout_seconds` |
| `src/tools/sandbox/mod.rs` | `confine_std` |
| `src/tools/skills.rs` | `refresh_skills` |
| `src/tools/subagent.rs` | `readable_subagent_log_line` |
| `src/tools/tool_descriptions.rs` | `TOOL_GROUPS` |
| `src/tools/tool_descriptions.rs` | `TOOL_GROUPS_RAW` |
| `src/tools/tool_descriptions.rs` | `group_summary` |
| `src/tools/tool_descriptions.rs` | `groups` |
| `src/web/attachments.rs` | `inspect_user_attachment` |
| `src/web/mod.rs` | `for_test_with_actor` |

## 保留并注明的

- `real_context/judge.rs::JudgeRequest.force_moderation_check`:有人写、没人读——像是判官侧漏了实现,记为疑似 bug,不删。
- `NoticeLevel::Warning`、`TtyWriteOp::Write`、`MathMode::Cell`、`MathArt.cols`:协议变体或测试在读,保留。
- 全局 `#![allow(dead_code)]` **已摘**:改成 `#![cfg_attr(test, allow(dead_code))]`,生产构建里死代码从此是警告;上面「保留并注明」的 18 个字段/变体各自带理由标了 `#[allow(dead_code)]`。

## 结构性候选(编译器看不见,未动)

- `AgentMode`:340 处引用;`AgentMode::reminder()` 恒 `None`、`with_mode_reminder` 空操作、`runtime_context` 的 `mode` 参数是 `let _ = mode;`——退役另开专项(提示词字节)。
- `agent → render` 两处反向使用(`is_command_tool`、`SPINNER_INTERVAL`)。
