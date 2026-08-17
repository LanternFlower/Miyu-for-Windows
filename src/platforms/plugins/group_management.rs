use super::{require_ai_confirmation, PlatformPlugin, PlatformTurnInput, PluginDescriptor};
use crate::config::QqGroupManagementPluginSettings as Settings;
use crate::platforms::{
    BotGroupRole, ConversationKind, OutboundMessage, PlatformGroupMember, PlatformInboundEvent,
    PlatformInboundEventKind, PlatformTurnContext,
};
use crate::state::PlatformPluginScopeKey;
use crate::tools::{ToolRegistry, ToolSpec};
use anyhow::{bail, Context, Result};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const GROUP_MANAGEMENT_PLUGIN_ID: &str = "qq_group_management";
const ROLE_KEY: &str = "qq_group_management.bot_role";
const OFFENDERS_KEY: &str = "offender_history";
const KICKS_KEY: &str = "kick_history";
const EVENTS_KEY: &str = "management_events";
const MAX_TARGETS: usize = 32;
/// QQ caps a mute at 30 days; anything longer is rejected by the server, so
/// catch it here where the message can explain itself.
const MAX_BAN_SECONDS: u64 = 30 * 24 * 60 * 60;

fn settings(context: &PlatformTurnContext) -> Result<Settings> {
    context
        .config
        .platforms
        .qq
        .plugins
        .get(GROUP_MANAGEMENT_PLUGIN_ID)
        .map(Settings::from_instance)
        .transpose()
        .map(|settings| settings.unwrap_or_default())
}

#[derive(Clone, Debug, Serialize)]
struct BanRecord {
    record_id: String,
    group_id: String,
    user_id: String,
    user_name: String,
    duration: u64,
    started_at: i64,
    expires_at: i64,
    status: String,
    operator_id: String,
    reason: String,
    source: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct OffenderHistory {
    user_id: String,
    user_name: String,
    ban_count: u64,
    total_duration: u64,
    first_ban_at: i64,
    last_ban_at: i64,
    last_reason: String,
    reason_history: Vec<ReasonEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReasonEntry {
    reason: String,
    duration: u64,
    banned_at: i64,
    operator_id: String,
    record_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct KickRecord {
    record_id: String,
    group_id: String,
    user_id: String,
    user_name: String,
    kicked_at: i64,
    operator_id: String,
    reason: String,
    reject_add_request: bool,
    source: String,
}

/// 统一的群管理事件流：禁言/解禁/踢出/头衔追加到同一份持久化记录，
/// 查询与统计从这里读取。旧的 offender_history / kick_history 两个 key
/// 照常写入（WebUI 的 HTTP 端点依赖它们），查询时按 record_id 去重合并，
/// 事件流上线前的历史因此自动补齐。
#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManagementEvent {
    record_id: String,
    /// ban | unban | kick | kick_black | title_set | title_clear
    action: String,
    user_id: String,
    user_name: String,
    #[serde(default)]
    duration: u64,
    happened_at: i64,
    operator_id: String,
    reason: String,
    source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    detail: String,
}

pub(crate) struct GroupManagementPlugin;

impl GroupManagementPlugin {
    pub(crate) fn new() -> Self {
        Self
    }

    fn scope(context: &PlatformTurnContext) -> PlatformPluginScopeKey {
        PlatformPluginScopeKey {
            plugin_id: GROUP_MANAGEMENT_PLUGIN_ID.to_string(),
            platform: context.conversation.platform.clone(),
            account_id: context.conversation.account_id.clone(),
            conversation_kind: context.conversation.kind.as_str().to_string(),
            conversation_id: context.conversation.conversation_id.clone(),
        }
    }

    async fn prepare_role(context: &PlatformTurnContext) {
        let role = match context.bot_group_role().await {
            BotGroupRole::Owner => "owner",
            BotGroupRole::Admin => "admin",
            BotGroupRole::Member => "member",
            BotGroupRole::Unknown => "unknown",
        };
        context.set_plugin_value(ROLE_KEY, Value::String(role.to_string()));
    }

    fn register(
        self: &Arc<Self>,
        registry: &mut ToolRegistry,
        context: Arc<PlatformTurnContext>,
    ) -> Result<()> {
        let settings = settings(&context)?;
        let query_enabled = settings.enable_tool || settings.enable_kick_tool;
        if context.conversation.kind != ConversationKind::Group {
            // 群聊之外只给 Miyu 管理员留跨群查询入口（group_id 必填）
            if query_enabled && context.is_admin {
                self.register_history_query(registry, context);
            }
            return Ok(());
        }
        if settings.enable_tool {
            self.register_ban(registry, context.clone());
        }
        if settings.enable_kick_tool {
            self.register_kick(registry, context.clone());
        }
        if query_enabled {
            self.register_history_query(registry, context.clone());
        }
        if settings.enable_special_title_tool {
            self.register_title(registry, context);
        }
        Ok(())
    }

    fn register_ban(
        self: &Arc<Self>,
        registry: &mut ToolRegistry,
        context: Arc<PlatformTurnContext>,
    ) {
        let plugin = self.clone();
        registry.register(
            ToolSpec::new(
                "qq_group_manage_with_log",
                "Mute or unmute one or more members in the current QQ group and record the action. duration_seconds is in SECONDS (1 hour = 3600, 24 hours = 86400); 0 un-mutes.",
                json!({
                    "type": "object",
                    "properties": {
                        "user_id": { "type": "string", "description": "Optional QQ id or multiple QQ ids separated by spaces/commas. Falls back to mentions/reply." },
                        "duration_seconds": {
                            "type": ["integer", "null"],
                            "minimum": 0,
                            "maximum": MAX_BAN_SECONDS,
                            "description": "禁言秒数，不是分钟也不是小时：10 分钟=600，1 小时=3600，24 小时=86400，最长 30 天=2592000；0 表示解禁。"
                        },
                        "duration": {
                            "type": ["integer", "null"],
                            "minimum": 0,
                            "maximum": MAX_BAN_SECONDS,
                            "description": "Deprecated alias of duration_seconds (also seconds)."
                        },
                        "reason": { "type": "string" },
                        "confirmation_token": { "type": "string" }
                    },
                    "additionalProperties": false
                }),
                move |args| {
                    let plugin = plugin.clone();
                    let context = context.clone();
                    async move { plugin.ban(args, context).await }
                },
            )
            .writes()
            .with_display_name("QQ群禁言/解禁"),
        );
    }

    fn register_kick(
        self: &Arc<Self>,
        registry: &mut ToolRegistry,
        context: Arc<PlatformTurnContext>,
    ) {
        let plugin = self.clone();
        registry.register(
            ToolSpec::new(
                "qq_group_manage_kick_with_log",
                "Kick one or more members from the current QQ group and record the action. Set blacklist=true to also reject their future join requests. Pass every target in a single call; the result reports each target separately.",
                kick_schema(),
                move |args| {
                    let plugin = plugin.clone();
                    let context = context.clone();
                    async move { plugin.kick(args, context).await }
                },
            )
            .writes()
            .with_display_name("QQ群踢人"),
        );
    }

    fn register_title(
        self: &Arc<Self>,
        registry: &mut ToolRegistry,
        context: Arc<PlatformTurnContext>,
    ) {
        let plugin = self.clone();
        registry.register(
            ToolSpec::new(
                "qq_group_set_special_title_with_log",
                "Set or clear one member's QQ group special title and record the action.",
                json!({
                    "type": "object",
                    "properties": {
                        "special_title": { "type": "string" },
                        "user_id": { "type": "string" },
                        "duration": { "type": "integer", "default": -1, "description": "头衔有效期秒数；-1 表示永久。" },
                        "reason": { "type": "string" },
                        "confirmation_token": { "type": "string" }
                    },
                    "required": ["special_title"],
                    "additionalProperties": false
                }),
                move |args| {
                    let plugin = plugin.clone();
                    let context = context.clone();
                    async move { plugin.title(args, context).await }
                },
            )
            .writes()
            .with_display_name("设置QQ群头衔"),
        );
    }

    fn register_history_query(
        self: &Arc<Self>,
        registry: &mut ToolRegistry,
        context: Arc<PlatformTurnContext>,
    ) {
        registry.register(ToolSpec::new(
            "qq_group_manage_history_query",
            "Query QQ group management records (mute/kick/title). view=events lists individual actions newest-first; view=stats aggregates per member (ban_count, kick_count, total mute duration). Miyu admins may pass group_id to query another group; group_id is required outside that group's chat.",
            history_query_schema(),
            move |args| {
                let context = context.clone();
                async move { query_history(args, &context) }
            },
        ));
    }

    async fn ban(&self, args: Value, context: Arc<PlatformTurnContext>) -> Result<String> {
        let settings = settings(&context)?;
        // The parameter has always been seconds, but it used to be spelled
        // `duration` with no unit anywhere — models read it as minutes and a
        // "24 hour" mute came out as 24 minutes. The explicit name wins; the
        // old one still works.
        let duration = args
            .get("duration_seconds")
            .and_then(Value::as_u64)
            .or_else(|| args.get("duration").and_then(Value::as_u64))
            .unwrap_or(settings.default_duration_seconds);
        if duration > MAX_BAN_SECONDS {
            return json_result(
                false,
                &format!(
                    "禁言时长上限 {MAX_BAN_SECONDS} 秒（30 天），收到 {duration} 秒；注意该参数的单位是秒"
                ),
                Value::Null,
            );
        }
        let reason = bounded_reason(&args, &settings)?;
        let targets = resolve_targets(&args, &context)?;
        if let Some(prompt) = require_ai_confirmation(
            &context,
            "qq_group_manage_with_log",
            &json!({
                "arguments": args,
                "targets": targets,
                "duration": duration,
                "reason": reason,
            }),
        )
        .await?
        {
            return Ok(prompt);
        }
        let mut results = Vec::with_capacity(targets.len());
        for target in targets {
            results.push(
                self.ban_one(&context, &settings, &target, duration, &reason)
                    .await,
            );
        }
        Ok(aggregate_target_results(results).to_string())
    }

    async fn ban_one(
        &self,
        context: &PlatformTurnContext,
        settings: &Settings,
        user_id: &str,
        duration: u64,
        reason: &str,
    ) -> Value {
        let member = match validate_target(context, user_id, true).await {
            Ok(member) => member,
            Err(error) => return failure_for_target(error, user_id),
        };
        tracing::info!(
            action = if duration == 0 { "unmute" } else { "mute" },
            requester_id = %context.sender_id,
            target_id = user_id,
            duration,
            "recording QQ group management intent"
        );
        if let Err(error) = context.set_group_ban(user_id, duration).await {
            return failure_for_target(error, user_id);
        }
        let now = now_unix();
        let record_id = record_id();
        let status = if duration == 0 { "unmuted" } else { "active" };
        let record = BanRecord {
            record_id: record_id.clone(),
            group_id: context.conversation.conversation_id.clone(),
            user_id: user_id.to_string(),
            user_name: member.display_name().to_string(),
            duration,
            started_at: now,
            expires_at: now.saturating_add(duration as i64),
            status: status.to_string(),
            operator_id: context.sender_id.clone(),
            reason: reason.to_string(),
            source: "llm_tool".to_string(),
        };
        let mut audit_errors = Vec::new();
        if settings.enable_record {
            let event = ManagementEvent {
                record_id: record_id.clone(),
                action: if duration == 0 { "unban" } else { "ban" }.to_string(),
                user_id: user_id.to_string(),
                user_name: member.display_name().to_string(),
                duration,
                happened_at: now,
                operator_id: context.sender_id.clone(),
                reason: reason.to_string(),
                source: "llm_tool".to_string(),
                detail: String::new(),
            };
            if let Err(error) = append_event(context, &event, settings.max_records_per_group) {
                audit_errors.push(format!("event log: {error}"));
            }
        }
        if duration > 0 && settings.enable_offender_history {
            if let Err(error) = update_offender(context, settings, &record) {
                audit_errors.push(format!("offender history: {error}"));
            }
        }
        if let Err(error) = record_real_context(
            context,
            &record_id,
            if duration == 0 { "解禁" } else { "禁言" },
            &member,
            reason,
            Some(duration),
        )
        .await
        {
            audit_errors.push(format!("real context: {error}"));
        }
        let mut result = external_operation_result(json!({ "record": record }), audit_errors);
        result["user_id"] = json!(user_id);
        // Echo the duration in words so a unit mix-up is visible in the result
        // instead of only on the victim's client.
        result["duration_seconds"] = json!(duration);
        result["duration_text"] = json!(humanize_seconds(duration));
        result
    }

    async fn kick(&self, args: Value, context: Arc<PlatformTurnContext>) -> Result<String> {
        let settings = settings(&context)?;
        let blacklist = args
            .get("blacklist")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let reason = bounded_reason(&args, &settings)?;
        let targets = resolve_targets(&args, &context)?;
        if targets.is_empty() {
            return json_result(false, "没有解析出踢人目标", Value::Null);
        }
        if let Some(prompt) = require_ai_confirmation(
            &context,
            "qq_group_manage_kick_with_log",
            &json!({ "arguments": args, "targets": targets, "blacklist": blacklist }),
        )
        .await?
        {
            return Ok(prompt);
        }
        // Sequential on purpose: kicks are destructive and the bridge throttles
        // them anyway. Per-target results carry their own retry verdict, so one
        // failure no longer sinks the whole call.
        let mut results = Vec::with_capacity(targets.len());
        for target in &targets {
            results.push(
                self.kick_one(&context, &settings, target, blacklist, &reason)
                    .await,
            );
        }
        Ok(aggregate_target_results(results).to_string())
    }

    async fn kick_one(
        &self,
        context: &PlatformTurnContext,
        settings: &Settings,
        user_id: &str,
        blacklist: bool,
        reason: &str,
    ) -> Value {
        let member = match validate_target(context, user_id, true).await {
            Ok(member) => member,
            Err(error) => return failure_for_target(error, user_id),
        };
        tracing::info!(
            action = if blacklist { "kick_blacklist" } else { "kick" },
            requester_id = %context.sender_id,
            target_id = user_id,
            "recording QQ group management intent"
        );
        let mut bridge_error = None;
        if let Err(error) = context.set_group_kick(user_id, blacklist).await {
            // NapCat reports a successful kick as `status=failed, retcode=100,
            // detail=kick member failed: <protobuf>` — and that protobuf starts
            // `08 00`, field 1 varint 0, the platform's own success code. The
            // member really is gone; only the envelope says otherwise. Asking
            // the server who is in the group settles it without having to
            // pattern-match a bridge's error text, and a genuine failure (an
            // unresolvable UIN, missing permission) still leaves the member in
            // place and is still reported as a failure.
            match context.group_member_fresh(user_id).await {
                Ok(None) => {
                    tracing::warn!(
                        target: "miyu::qq",
                        user_id,
                        error = %error,
                        "{}",
                        crate::i18n::text(
                            "the kick bridge reported a failure but the member is gone; treating it as done",
                            "踢人接口报错但成员已不在群里，按成功处理",
                        )
                    );
                    bridge_error = Some(error.to_string());
                }
                _ => return failure_for_target(error, user_id),
            }
        }
        let record = KickRecord {
            record_id: record_id(),
            group_id: context.conversation.conversation_id.clone(),
            user_id: user_id.to_string(),
            user_name: member.display_name().to_string(),
            kicked_at: now_unix(),
            operator_id: context.sender_id.clone(),
            reason: reason.to_string(),
            reject_add_request: blacklist,
            source: "llm_tool".to_string(),
        };
        let mut audit_errors = Vec::new();
        if let Err(error) = append_kick(context, &record, settings.max_kick_history_per_group) {
            audit_errors.push(format!("kick history: {error}"));
        }
        // 与 ban 路径对齐:enable_record 管事件日志(kick 历史是入群审批
        // 等功能依赖的数据,不受此开关影响)。
        if settings.enable_record {
            let event = ManagementEvent {
                record_id: record.record_id.clone(),
                action: if blacklist { "kick_black" } else { "kick" }.to_string(),
                user_id: record.user_id.clone(),
                user_name: record.user_name.clone(),
                duration: 0,
                happened_at: record.kicked_at,
                operator_id: record.operator_id.clone(),
                reason: record.reason.clone(),
                source: record.source.clone(),
                detail: String::new(),
            };
            if let Err(error) = append_event(context, &event, settings.max_records_per_group) {
                audit_errors.push(format!("event log: {error}"));
            }
        }
        if let Err(error) = record_real_context(
            context,
            &record.record_id,
            if blacklist {
                "踢出并拉黑"
            } else {
                "踢出"
            },
            &member,
            reason,
            None,
        )
        .await
        {
            audit_errors.push(format!("real context: {error}"));
        }
        let mut data = json!({ "record": record });
        if let Some(bridge_error) = bridge_error {
            // Surfaced, not swallowed: the model should say the kick went
            // through, and an operator reading the transcript should still see
            // that the bridge disagreed.
            data["bridge_reported_error"] = Value::String(bridge_error);
            data["verified_by"] = Value::String("group_member_fresh".to_string());
        }
        external_operation_result(data, audit_errors)
    }

    async fn title(&self, args: Value, context: Arc<PlatformTurnContext>) -> Result<String> {
        let settings = settings(&context)?;
        let targets = resolve_targets(&args, &context)?;
        if targets.len() != 1 {
            return json_result(false, "群头衔操作必须且只能指定一个目标", Value::Null);
        }
        let title = args
            .get("special_title")
            .and_then(Value::as_str)
            .context("special_title is required")?
            .trim();
        if title.chars().count() > settings.max_special_title_length {
            return json_result(false, "群头衔超过配置长度限制", Value::Null);
        }
        let duration = args.get("duration").and_then(Value::as_i64).unwrap_or(-1);
        let duration = if duration < 0 {
            -1
        } else if settings.max_special_title_duration_seconds > 0 {
            duration.min(settings.max_special_title_duration_seconds)
        } else {
            duration
        };
        let reason = bounded_reason(&args, &settings)?;
        let member = match validate_target(&context, &targets[0], false).await {
            Ok(member) => member,
            Err(error) => return json_result(false, &error.to_string(), Value::Null),
        };
        if let Some(prompt) = require_ai_confirmation(
            &context,
            "qq_group_set_special_title_with_log",
            &json!({
                "arguments": args,
                "target": targets[0],
                "special_title": title,
                "duration": duration,
                "reason": reason,
            }),
        )
        .await?
        {
            return Ok(prompt);
        }
        tracing::info!(
            action = if title.is_empty() { "clear_title" } else { "set_title" },
            requester_id = %context.sender_id,
            target_id = %targets[0],
            "recording QQ group management intent"
        );
        if let Err(error) = context
            .set_group_special_title(&targets[0], title, duration)
            .await
        {
            return json_result(false, &error.to_string(), Value::Null);
        }
        let id = record_id();
        let mut audit_errors = Vec::new();
        let event = ManagementEvent {
            record_id: id.clone(),
            action: if title.is_empty() {
                "title_clear"
            } else {
                "title_set"
            }
            .to_string(),
            user_id: targets[0].clone(),
            user_name: member.display_name().to_string(),
            duration: 0,
            happened_at: now_unix(),
            operator_id: context.sender_id.clone(),
            reason: reason.clone(),
            source: "llm_tool".to_string(),
            detail: title.to_string(),
        };
        if let Err(error) = append_event(&context, &event, settings.max_records_per_group) {
            audit_errors.push(format!("event log: {error}"));
        }
        if let Err(error) = record_real_context(
            &context,
            &id,
            if title.is_empty() {
                "清除群头衔"
            } else {
                "设置群头衔"
            },
            &member,
            &reason,
            None,
        )
        .await
        {
            audit_errors.push(format!("real context: {error}"));
        }
        Ok(external_operation_result(
            json!({ "record_id": id, "user_id": targets[0], "special_title": title, "duration": duration }),
            audit_errors,
        )
        .to_string())
    }

}

impl PlatformPlugin for Arc<GroupManagementPlugin> {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: GROUP_MANAGEMENT_PLUGIN_ID,
            // Resolve the bot's current group role before recall and real-context
            // plugins build their per-turn capability prompts.
            priority: 210,
            default_enabled: true,
        }
    }

    fn register_tools(
        &self,
        registry: &mut ToolRegistry,
        context: Arc<PlatformTurnContext>,
    ) -> Result<()> {
        self.register(registry, context)
    }

    fn before_turn<'a>(
        &'a self,
        context: &'a PlatformTurnContext,
        input: &'a mut PlatformTurnInput,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if context.conversation.kind == ConversationKind::Group {
                GroupManagementPlugin::prepare_role(context).await;
                input.system_context.push("<qq-group-management>执行群管理动作前必须调用对应工具；只有工具返回 success=true 后才能声称动作已经完成。普通成员触发敏感动作时，工具可能要求在本轮原样再次调用同一工具进行确认。</qq-group-management>".to_string());
            }
            Ok(())
        })
    }

    fn observe_inbound<'a>(
        &'a self,
        context: &'a PlatformTurnContext,
        event: &'a PlatformInboundEvent,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if event.operator_id.as_deref() == Some(context.conversation.account_id.as_str()) {
                return Ok(());
            }
            let settings = settings(context)?;
            match event.kind {
                PlatformInboundEventKind::GroupBan if settings.enable_record => {
                    let duration = event.duration_seconds.unwrap_or(0);
                    if duration == 0 && !settings.sync_external_unmute_notice {
                        return Ok(());
                    }
                    let now = event.timestamp.max(now_unix());
                    let record = ManagementEvent {
                        record_id: record_id(),
                        action: if duration == 0 { "unban" } else { "ban" }.to_string(),
                        user_id: event.sender_id.clone(),
                        user_name: event.sender_display_name.clone(),
                        duration,
                        happened_at: now,
                        operator_id: event.operator_id.clone().unwrap_or_default(),
                        reason: String::new(),
                        source: "onebot_notice".to_string(),
                        detail: String::new(),
                    };
                    append_event(context, &record, settings.max_records_per_group)?;
                    let member = notice_member(context, event);
                    record_real_context(
                        context,
                        &record.record_id,
                        if duration == 0 {
                            "外部解禁"
                        } else {
                            "外部禁言"
                        },
                        &member,
                        "",
                        Some(duration),
                    )
                    .await?;
                }
                PlatformInboundEventKind::GroupDecrease => {
                    // Whoever left is gone regardless of how: drop them from
                    // the per-turn roster cache so a later kick/mute in this
                    // same turn cannot validate against a stale entry.
                    context.forget_group_member(&event.sender_id);
                    if event.notice_sub_type.as_deref() != Some("kick") {
                        return Ok(());
                    }
                    let record = KickRecord {
                        record_id: record_id(),
                        group_id: context.conversation.conversation_id.clone(),
                        user_id: event.sender_id.clone(),
                        user_name: event.sender_display_name.clone(),
                        kicked_at: event.timestamp.max(now_unix()),
                        operator_id: event.operator_id.clone().unwrap_or_default(),
                        reason: String::new(),
                        reject_add_request: false,
                        source: "onebot_notice".to_string(),
                    };
                    append_kick(context, &record, settings.max_kick_history_per_group)?;
                    // 与 GroupBan 通知分支对齐:事件日志受 enable_record 管。
                    if settings.enable_record {
                        append_event(
                            context,
                            &ManagementEvent {
                                record_id: record.record_id.clone(),
                                action: "kick".to_string(),
                                user_id: record.user_id.clone(),
                                user_name: record.user_name.clone(),
                                duration: 0,
                                happened_at: record.kicked_at,
                                operator_id: record.operator_id.clone(),
                                reason: String::new(),
                                source: "onebot_notice".to_string(),
                                detail: String::new(),
                            },
                            settings.max_records_per_group,
                        )?;
                    }
                    let member = notice_member(context, event);
                    record_real_context(context, &record.record_id, "外部踢出", &member, "", None)
                        .await?;
                }
                _ => {}
            }
            Ok(())
        })
    }

    fn before_send<'a>(
        &'a self,
        _context: &'a PlatformTurnContext,
        message: OutboundMessage,
    ) -> BoxFuture<'a, Result<super::PreparedSend>> {
        Box::pin(async move { Ok(super::PreparedSend::unchanged(message)) })
    }
}

async fn validate_target(
    context: &PlatformTurnContext,
    user_id: &str,
    protect_managers: bool,
) -> Result<PlatformGroupMember> {
    if user_id == context.conversation.account_id {
        bail!("不能对 Miyu 自身执行该操作");
    }
    // Fresh lookup on purpose: this gate exists to stop kicks/mutes aimed at
    // members who already left, and a cached roster cannot answer that.
    let member = context
        .group_member_fresh(user_id)
        .await?
        .context("目标不在当前群中")?;
    if protect_managers && matches!(member.role.as_str(), "owner" | "admin") {
        bail!("不能对群主或管理员执行该操作");
    }
    Ok(member)
}

fn resolve_targets(args: &Value, context: &PlatformTurnContext) -> Result<Vec<String>> {
    let mut values = Vec::new();
    if let Some(list) = args.get("user_ids").and_then(Value::as_array) {
        values.extend(
            list.iter()
                .filter_map(Value::as_str)
                .flat_map(split_ids)
                .collect::<Vec<_>>(),
        );
    }
    if let Some(explicit) = args
        .get("user_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        values.extend(split_ids(explicit));
    } else if values.is_empty() {
        // Neither form given: fall back to who was mentioned, then to whoever
        // wrote the replied-to message.
        if let Some(event) = context.inbound_event() {
            values.extend(event.mentioned_user_ids.iter().cloned());
            if values.is_empty() {
                if let Some(replied) = event.replied_message.as_ref() {
                    values.push(replied.sender_id.clone());
                }
            }
        }
    }
    let mut seen = HashSet::new();
    values.retain(|id| {
        valid_id(id) && id != &context.conversation.account_id && seen.insert(id.clone())
    });
    values.truncate(MAX_TARGETS);
    if values.is_empty() {
        bail!("未找到有效的目标 QQ 号");
    }
    Ok(values)
}

fn split_ids(value: &str) -> Vec<String> {
    value
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| (5..=12).contains(&part.len()))
        .map(str::to_string)
        .collect()
}

/// Renders a mute duration the way a person would say it, so the model can
/// sanity-check its own arithmetic against what it intended.
fn humanize_seconds(seconds: u64) -> String {
    if seconds == 0 {
        return "解禁".to_string();
    }
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (hours, rest) = (rest / 3_600, rest % 3_600);
    let (minutes, secs) = (rest / 60, rest % 60);
    let mut parts = Vec::new();
    for (value, unit) in [
        (days, "天"),
        (hours, "小时"),
        (minutes, "分钟"),
        (secs, "秒"),
    ] {
        if value > 0 {
            parts.push(format!("{value}{unit}"));
        }
    }
    parts.join("")
}

fn valid_id(value: &str) -> bool {
    (5..=12).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn notice_member(
    context: &PlatformTurnContext,
    event: &PlatformInboundEvent,
) -> PlatformGroupMember {
    PlatformGroupMember {
        group_id: context.conversation.conversation_id.clone(),
        user_id: event.sender_id.clone(),
        nickname: event.sender_display_name.clone(),
        card: String::new(),
        role: "member".to_string(),
        title: String::new(),
        joined_at: 0,
        last_active_at: 0,
    }
}

fn bounded_reason(args: &Value, settings: &Settings) -> Result<String> {
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if reason.chars().count() > settings.max_reason_length {
        bail!("reason exceeds configured maximum length");
    }
    Ok(reason.to_string())
}

fn optional_id(args: &Value) -> Option<String> {
    args.get("user_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| valid_id(id))
        .map(str::to_string)
}

fn limit(args: &Value) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize
}

fn update_offender(
    context: &PlatformTurnContext,
    settings: &Settings,
    record: &BanRecord,
) -> Result<()> {
    context.state_store.plugin_update_json(
        &GroupManagementPlugin::scope(context),
        OFFENDERS_KEY,
        |current: Option<HashMap<String, OffenderHistory>>| {
            let mut map = current.unwrap_or_default();
            let entry = map
                .entry(record.user_id.clone())
                .or_insert_with(|| OffenderHistory {
                    user_id: record.user_id.clone(),
                    user_name: record.user_name.clone(),
                    first_ban_at: record.started_at,
                    ..OffenderHistory::default()
                });
            entry.user_name.clone_from(&record.user_name);
            entry.ban_count = entry.ban_count.saturating_add(1);
            entry.total_duration = entry.total_duration.saturating_add(record.duration);
            entry.last_ban_at = record.started_at;
            entry.last_reason.clone_from(&record.reason);
            entry.reason_history.push(ReasonEntry {
                reason: record.reason.clone(),
                duration: record.duration,
                banned_at: record.started_at,
                operator_id: record.operator_id.clone(),
                record_id: record.record_id.clone(),
            });
            if map.len() > settings.max_offender_history_per_group {
                if let Some(remove) = map
                    .iter()
                    .min_by_key(|(_, item)| (item.ban_count, item.last_ban_at))
                    .map(|(id, _)| id.clone())
                {
                    map.remove(&remove);
                }
            }
            Ok(Some(map))
        },
    )?;
    Ok(())
}

fn append_kick(context: &PlatformTurnContext, record: &KickRecord, max: usize) -> Result<()> {
    context.state_store.plugin_update_json(
        &GroupManagementPlugin::scope(context),
        KICKS_KEY,
        |current: Option<Vec<KickRecord>>| {
            let mut records = current.unwrap_or_default();
            records.push(record.clone());
            trim_vec(&mut records, max);
            Ok(Some(records))
        },
    )?;
    Ok(())
}

fn append_event(context: &PlatformTurnContext, event: &ManagementEvent, max: usize) -> Result<()> {
    context.state_store.plugin_update_json(
        &GroupManagementPlugin::scope(context),
        EVENTS_KEY,
        |current: Option<Vec<ManagementEvent>>| {
            let mut events = current.unwrap_or_default();
            events.push(event.clone());
            trim_vec(&mut events, max);
            Ok(Some(events))
        },
    )?;
    Ok(())
}

/// 解析查询目标群：群聊内默认当前群；带 group_id 且非当前群时要求
/// Miyu 管理员；群聊之外（私聊/CLI）group_id 必填且仅限管理员。
fn resolve_query_scope(
    args: &Value,
    context: &PlatformTurnContext,
) -> Result<PlatformPluginScopeKey> {
    let requested = args
        .get("group_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let current = GroupManagementPlugin::scope(context);
    let Some(id) = requested else {
        if context.conversation.kind == ConversationKind::Group {
            return Ok(current);
        }
        bail!("群聊之外查询必须提供 group_id");
    };
    if !valid_id(id) {
        bail!("group_id 必须是数字群号");
    }
    if context.conversation.kind == ConversationKind::Group
        && context.conversation.conversation_id == id
    {
        return Ok(current);
    }
    if !context.is_admin {
        bail!("跨群查询仅限 Miyu 管理员");
    }
    Ok(PlatformPluginScopeKey {
        conversation_kind: "group".to_string(),
        conversation_id: id.to_string(),
        ..current
    })
}

/// 汇总三个来源为一条按时间升序的事件流：新事件流为主，旧的
/// kick_history 与 offender_history.reason_history 按 record_id 去重补入，
/// 因此事件流上线之前的历史也查得到。
fn load_all_events(
    context: &PlatformTurnContext,
    scope: &PlatformPluginScopeKey,
) -> Result<Vec<ManagementEvent>> {
    let mut events = context
        .state_store
        .plugin_get_json::<Vec<ManagementEvent>>(scope, EVENTS_KEY)?
        .unwrap_or_default();
    let mut seen = events
        .iter()
        .map(|event| event.record_id.clone())
        .collect::<HashSet<_>>();
    let kicks = context
        .state_store
        .plugin_get_json::<Vec<KickRecord>>(scope, KICKS_KEY)?
        .unwrap_or_default();
    for kick in kicks {
        if seen.insert(kick.record_id.clone()) {
            events.push(ManagementEvent {
                record_id: kick.record_id,
                action: if kick.reject_add_request {
                    "kick_black"
                } else {
                    "kick"
                }
                .to_string(),
                user_id: kick.user_id,
                user_name: kick.user_name,
                duration: 0,
                happened_at: kick.kicked_at,
                operator_id: kick.operator_id,
                reason: kick.reason,
                source: kick.source,
                detail: String::new(),
            });
        }
    }
    let offenders = context
        .state_store
        .plugin_get_json::<HashMap<String, OffenderHistory>>(scope, OFFENDERS_KEY)?
        .unwrap_or_default();
    for offender in offenders.into_values() {
        for entry in &offender.reason_history {
            if seen.insert(entry.record_id.clone()) {
                events.push(ManagementEvent {
                    record_id: entry.record_id.clone(),
                    action: "ban".to_string(),
                    user_id: offender.user_id.clone(),
                    user_name: offender.user_name.clone(),
                    duration: entry.duration,
                    happened_at: entry.banned_at,
                    operator_id: entry.operator_id.clone(),
                    reason: entry.reason.clone(),
                    source: "offender_history".to_string(),
                    detail: String::new(),
                });
            }
        }
    }
    events.sort_by_key(|event| event.happened_at);
    Ok(events)
}

fn action_matches(filter: &str, action: &str) -> bool {
    match filter {
        "all" => true,
        "ban" => matches!(action, "ban" | "unban"),
        "kick" => matches!(action, "kick" | "kick_black"),
        "title" => matches!(action, "title_set" | "title_clear"),
        _ => false,
    }
}

/// 每条禁言事件的当前状态：后续有解禁则 unmuted，后续被再次禁言覆盖则
/// overridden，否则按到期时间判 active/expired。输入必须按时间升序。
fn ban_statuses(events: &[ManagementEvent], now: i64) -> HashMap<String, String> {
    let mut statuses = HashMap::new();
    for (index, event) in events.iter().enumerate() {
        if event.action != "ban" {
            continue;
        }
        let mut status = if event.happened_at.saturating_add(event.duration as i64) <= now {
            "expired"
        } else {
            "active"
        };
        for later in &events[index + 1..] {
            if later.user_id != event.user_id {
                continue;
            }
            match later.action.as_str() {
                "unban" => {
                    status = "unmuted";
                    break;
                }
                "ban" => {
                    status = "overridden";
                    break;
                }
                _ => {}
            }
        }
        statuses.insert(event.record_id.clone(), status.to_string());
    }
    statuses
}

fn query_history(args: Value, context: &PlatformTurnContext) -> Result<String> {
    let scope = match resolve_query_scope(&args, context) {
        Ok(scope) => scope,
        Err(error) => return json_result(false, &error.to_string(), Value::Null),
    };
    let action = args.get("action").and_then(Value::as_str).unwrap_or("all");
    if !matches!(action, "ban" | "kick" | "title" | "all") {
        return json_result(false, "action 必须是 ban/kick/title/all", Value::Null);
    }
    let events = load_all_events(context, &scope)?;
    match args.get("view").and_then(Value::as_str).unwrap_or("events") {
        "stats" => query_history_stats(&args, action, &events, &scope.conversation_id),
        "events" => query_history_events(&args, action, events, &scope.conversation_id),
        _ => json_result(false, "view 必须是 events 或 stats", Value::Null),
    }
}

fn query_history_events(
    args: &Value,
    action: &str,
    events: Vec<ManagementEvent>,
    group_id: &str,
) -> Result<String> {
    let user_id = optional_id(args);
    let keyword = lowercase_keyword(args);
    let ascending = args.get("sort_order").and_then(Value::as_str) == Some("asc");
    let statuses = ban_statuses(&events, now_unix());
    let mut records = events
        .into_iter()
        .filter(|event| action_matches(action, &event.action))
        .filter(|event| user_id.as_deref().is_none_or(|id| event.user_id == id))
        .filter(|event| {
            keyword.is_empty()
                || event.user_name.to_lowercase().contains(&keyword)
                || event.reason.to_lowercase().contains(&keyword)
                || event.detail.to_lowercase().contains(&keyword)
        })
        .map(|event| {
            let status = statuses.get(&event.record_id).cloned();
            let mut value = serde_json::to_value(&event).unwrap_or_default();
            if let Some(status) = status {
                value["status"] = json!(status);
            }
            value
        })
        .collect::<Vec<_>>();
    if !ascending {
        records.reverse();
    }
    records.truncate(limit(args));
    json_result(
        true,
        "查询成功",
        json!({ "group_id": group_id, "count": records.len(), "records": records }),
    )
}

#[derive(Default, Serialize)]
struct MemberStats {
    user_id: String,
    user_name: String,
    ban_count: u64,
    total_ban_duration: u64,
    kick_count: u64,
    title_count: u64,
    last_action_at: i64,
    last_reason: String,
}

fn aggregate_member_stats(action: &str, events: &[ManagementEvent]) -> Vec<MemberStats> {
    let mut map: HashMap<String, MemberStats> = HashMap::new();
    for event in events
        .iter()
        .filter(|event| action_matches(action, &event.action))
    {
        let entry = map.entry(event.user_id.clone()).or_default();
        entry.user_id.clone_from(&event.user_id);
        if !event.user_name.is_empty() {
            entry.user_name.clone_from(&event.user_name);
        }
        match event.action.as_str() {
            "ban" => {
                entry.ban_count += 1;
                entry.total_ban_duration = entry.total_ban_duration.saturating_add(event.duration);
            }
            "kick" | "kick_black" => entry.kick_count += 1,
            "title_set" | "title_clear" => entry.title_count += 1,
            _ => {}
        }
        if event.happened_at >= entry.last_action_at {
            entry.last_action_at = event.happened_at;
            if !event.reason.is_empty() {
                entry.last_reason.clone_from(&event.reason);
            }
        }
    }
    map.into_values().collect()
}

fn query_history_stats(
    args: &Value,
    action: &str,
    events: &[ManagementEvent],
    group_id: &str,
) -> Result<String> {
    let user_id = optional_id(args);
    let keyword = lowercase_keyword(args);
    let minimum = args
        .get("min_ban_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let sort_by = args
        .get("sort_by")
        .and_then(Value::as_str)
        .unwrap_or("ban_count");
    let ascending = args.get("sort_order").and_then(Value::as_str) == Some("asc");
    let mut items = aggregate_member_stats(action, events)
        .into_iter()
        .filter(|item| item.ban_count >= minimum)
        .filter(|item| user_id.as_deref().is_none_or(|id| item.user_id == id))
        .filter(|item| {
            keyword.is_empty()
                || item.user_name.to_lowercase().contains(&keyword)
                || item.last_reason.to_lowercase().contains(&keyword)
        })
        .collect::<Vec<_>>();
    items.sort_by_key(|item| match sort_by {
        "kick_count" => item.kick_count as i64,
        "total_duration" => item.total_ban_duration as i64,
        "time" | "last_action_at" => item.last_action_at,
        _ => item.ban_count as i64,
    });
    if !ascending {
        items.reverse();
    }
    items.truncate(limit(args));
    json_result(
        true,
        "查询成功",
        json!({ "group_id": group_id, "count": items.len(), "records": items }),
    )
}

fn lowercase_keyword(args: &Value) -> String {
    args.get("keyword")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase()
}

async fn record_real_context(
    context: &PlatformTurnContext,
    id: &str,
    action: &str,
    member: &PlatformGroupMember,
    reason: &str,
    duration: Option<u64>,
) -> Result<()> {
    let mut text = format!(
        "[System:群管理行为]\n操作：{action}\n执行者：Miyu（{}）\n对象：{}（{}）",
        context.conversation.account_id,
        member.display_name(),
        member.user_id
    );
    if let Some(duration) = duration {
        text.push_str(&format!("\n时长：{duration} 秒"));
    }
    if !reason.is_empty() {
        text.push_str(&format!("\n原因：{reason}"));
    }
    text.push_str(&format!("\n记录 ID：{id}"));
    context
        .plugins
        .try_record_external_bot_message(context, &format!("qq-management-{id}"), &text)
        .await
}

/// Kick takes a real array so batching is discoverable from the schema rather
/// than buried in prose — the scalar form stays for single targets and for
/// falling back to mentions/reply.
fn kick_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "user_ids": {
                "type": "array",
                "minItems": 1,
                "items": { "type": "string", "pattern": "^[1-9][0-9]{4,11}$" },
                "description": "QQ ids to kick. Prefer this over repeating the tool once per member."
            },
            "user_id": { "type": "string", "description": "Single QQ id, or several separated by spaces/commas. Falls back to mentions/reply when omitted." },
            "blacklist": { "type": "boolean", "default": false, "description": "true 时踢出并拒绝其后续加群请求（踢黑）。" },
            "reason": { "type": "string" },
            "confirmation_token": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn reason_schema() -> Value {
    json!({ "type": "object", "properties": { "user_id": { "type": "string" }, "reason": { "type": "string" }, "confirmation_token": { "type": "string" } }, "additionalProperties": false })
}

fn history_query_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["ban", "kick", "title", "all"], "default": "all", "description": "筛选操作类型；ban 含禁言与解禁，kick 含踢黑。" },
            "view": { "type": "string", "enum": ["events", "stats"], "default": "events", "description": "events=逐条记录（禁言带 active/expired/unmuted/overridden 状态）；stats=按成员聚合（ban_count、kick_count、total_ban_duration 等）。" },
            "user_id": { "type": "string" },
            "keyword": { "type": "string" },
            "min_ban_count": { "type": "integer", "minimum": 1, "description": "仅 stats 视图：过滤禁言次数下限。" },
            "sort_by": { "type": "string", "enum": ["time", "ban_count", "kick_count", "total_duration", "last_action_at"], "description": "events 视图按时间排；stats 视图默认按 ban_count。" },
            "sort_order": { "type": "string", "enum": ["asc", "desc"], "default": "desc" },
            "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
            "group_id": { "type": "string", "description": "跨群查询的目标群号；仅 Miyu 管理员可用，群聊之外调用时必填。" }
        },
        "additionalProperties": false
    })
}

fn trim_vec<T>(values: &mut Vec<T>, max: usize) {
    let max = max.max(1);
    if values.len() > max {
        values.drain(..values.len() - max);
    }
}

fn record_id() -> String {
    format!("{:012x}", rand::random::<u64>() & 0xffffffffffff)
}
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn failure(error: anyhow::Error) -> Value {
    json!({
        "success": false,
        "operation_succeeded": false,
        "audit_succeeded": false,
        "do_not_retry": false,
        "message": error.to_string()
    })
}
fn failure_for_target(error: anyhow::Error, user_id: &str) -> Value {
    let mut result = failure(error);
    result["user_id"] = json!(user_id);
    result
}
fn external_operation_result(data: Value, audit_errors: Vec<String>) -> Value {
    let audit_succeeded = audit_errors.is_empty();
    json!({
        "success": true,
        "operation_succeeded": true,
        "audit_succeeded": audit_succeeded,
        "do_not_retry": true,
        "message": if audit_succeeded {
            "操作成功".to_string()
        } else {
            "外部操作已成功，但本地审计记录失败；请勿重试外部操作".to_string()
        },
        "audit_errors": audit_errors,
        "data": data,
    })
}

/// Partial-success envelope shared by the batched admin actions.
///
/// The per-target retry verdict is the important part: without it the model
/// cannot tell a hopeless failure from a transient one and hammers the same
/// target again — which is exactly what a batch kick against departed members
/// used to do.
fn aggregate_target_results(results: Vec<Value>) -> Value {
    let mut successful_target_ids = Vec::new();
    let mut failed_target_ids = Vec::new();
    let mut audit_failed_count = 0usize;
    for result in &results {
        let target_id = result
            .get("user_id")
            .and_then(Value::as_str)
            .or_else(|| {
                result
                    .pointer("/data/record/user_id")
                    .and_then(Value::as_str)
            })
            .unwrap_or_default()
            .to_string();
        if result["operation_succeeded"] == true {
            successful_target_ids.push(target_id);
            if result["audit_succeeded"] == false {
                audit_failed_count += 1;
            }
        } else {
            failed_target_ids.push(target_id);
        }
    }
    let success_count = successful_target_ids.len();
    let failed_count = failed_target_ids.len();
    let has_failures = failed_count > 0;
    let message = if audit_failed_count > 0 || has_failures {
        format!(
            "外部操作成功 {success_count} 个、失败 {failed_count} 个；成功目标不得重试，失败目标可单独重试"
        )
    } else {
        format!("成功 {success_count} 个，失败 {failed_count} 个")
    };
    json!({
        "success": success_count > 0,
        "operation_succeeded": success_count > 0,
        "audit_succeeded": audit_failed_count == 0,
        "do_not_retry": !has_failures,
        "do_not_retry_successful_targets": true,
        "retry_failed_targets_only": has_failures,
        "message": message,
        "success_count": success_count,
        "failed_count": failed_count,
        "audit_failed_count": audit_failed_count,
        "successful_target_ids": successful_target_ids,
        "failed_target_ids": failed_target_ids,
        "results": results,
    })
}

fn json_result(success: bool, message: &str, data: Value) -> Result<String> {
    Ok(json!({ "success": success, "message": message, "data": data }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parser_accepts_only_qq_sized_numeric_ids() {
        assert_eq!(
            split_ids("12345, 12345 @678901 and invalid-12"),
            vec!["12345", "12345", "678901"]
        );
        assert!(valid_id("12345"));
        assert!(!valid_id("1234"));
        assert!(!valid_id("12a45"));
    }

    #[test]
    fn mute_duration_is_spelled_in_seconds_and_reads_back_in_words() {
        assert_eq!(humanize_seconds(0), "解禁");
        assert_eq!(humanize_seconds(600), "10分钟");
        assert_eq!(humanize_seconds(3_600), "1小时");
        // The exact case that shipped as 24 minutes: 24h must be 86400, and a
        // 1440 that a model meant as "minutes" must read back as 24 minutes so
        // the mistake is visible in the result.
        assert_eq!(humanize_seconds(86_400), "1天");
        assert_eq!(humanize_seconds(1_440), "24分钟");
        assert_eq!(humanize_seconds(MAX_BAN_SECONDS), "30天");
        assert_eq!(humanize_seconds(90), "1分钟30秒");
    }

    #[test]
    fn kick_targets_accept_an_array_and_still_fall_back_to_a_scalar() {
        // The array form is what the schema now advertises; the scalar and its
        // space/comma splitting stay for single targets and older habits.
        let ids = |value: &Value| -> Vec<String> {
            value
                .get("user_ids")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .flat_map(split_ids)
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(
            ids(&json!({ "user_ids": ["12345", "678901"] })),
            vec!["12345".to_string(), "678901".to_string()]
        );
        assert_eq!(split_ids("12345 678901"), vec!["12345", "678901"]);
    }

    #[test]
    fn batch_results_tell_the_model_which_targets_may_be_retried() {
        let aggregate = aggregate_target_results(vec![
            external_operation_result(json!({ "record": { "user_id": "12345" } }), Vec::new()),
            failure_for_target(anyhow::anyhow!("目标不在当前群中"), "678901"),
        ]);
        assert_eq!(aggregate["success_count"], 1);
        assert_eq!(aggregate["failed_count"], 1);
        // Mixed outcome: the successes must never be retried, the failure may
        // be retried on its own. Without this the model re-kicked the same
        // dead target over and over.
        assert_eq!(aggregate["do_not_retry"], false);
        assert_eq!(aggregate["do_not_retry_successful_targets"], true);
        assert_eq!(aggregate["retry_failed_targets_only"], true);
        assert_eq!(aggregate["failed_target_ids"], json!(["678901"]));

        let all_good = aggregate_target_results(vec![external_operation_result(
            json!({ "record": { "user_id": "12345" } }),
            Vec::new(),
        )]);
        assert_eq!(all_good["do_not_retry"], true);
    }

    fn event(action: &str, user: &str, at: i64, duration: u64, reason: &str) -> ManagementEvent {
        ManagementEvent {
            record_id: format!("{action}-{user}-{at}"),
            action: action.to_string(),
            user_id: user.to_string(),
            user_name: format!("用户{user}"),
            duration,
            happened_at: at,
            operator_id: "10000".to_string(),
            reason: reason.to_string(),
            source: "llm_tool".to_string(),
            detail: String::new(),
        }
    }

    #[test]
    fn action_filter_groups_related_event_kinds() {
        assert!(action_matches("ban", "unban"));
        assert!(action_matches("kick", "kick_black"));
        assert!(action_matches("title", "title_clear"));
        assert!(action_matches("all", "ban"));
        assert!(!action_matches("ban", "kick"));
        assert!(!action_matches("bogus", "ban"));
    }

    #[test]
    fn ban_status_reflects_later_unban_override_and_expiry() {
        let now = 1_000_000;
        let events = vec![
            event("ban", "11111", now - 100, 3_600, "刷屏"), // 后被解禁
            event("unban", "11111", now - 50, 0, ""),
            event("ban", "22222", now - 100, 600, "口嗨"), // 后被再次禁言覆盖
            event("ban", "22222", now - 50, 3_600, "加重"), // 仍在禁言期
            event("ban", "33333", now - 7_200, 600, "已过期"),
        ];
        let statuses = ban_statuses(&events, now);
        assert_eq!(statuses[&events[0].record_id], "unmuted");
        assert_eq!(statuses[&events[2].record_id], "overridden");
        assert_eq!(statuses[&events[3].record_id], "active");
        assert_eq!(statuses[&events[4].record_id], "expired");
    }

    #[test]
    fn member_stats_aggregate_counts_durations_and_last_reason() {
        let events = vec![
            event("ban", "11111", 100, 600, "刷屏"),
            event("ban", "11111", 200, 1_200, "再犯"),
            event("unban", "11111", 300, 0, ""), // 解禁不计次
            event("kick", "11111", 400, 0, "屡教不改"),
            event("kick_black", "22222", 500, 0, ""),
            event("title_set", "22222", 600, 0, ""),
        ];
        let mut stats = aggregate_member_stats("all", &events);
        stats.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].ban_count, 2);
        assert_eq!(stats[0].total_ban_duration, 1_800);
        assert_eq!(stats[0].kick_count, 1);
        assert_eq!(stats[0].last_reason, "屡教不改");
        assert_eq!(stats[0].last_action_at, 400);
        assert_eq!(stats[1].kick_count, 1);
        assert_eq!(stats[1].title_count, 1);
        assert_eq!(stats[1].ban_count, 0);

        // action 过滤只统计对应类别
        let ban_only = aggregate_member_stats("ban", &events);
        assert!(ban_only.iter().all(|item| item.kick_count == 0));
    }

    #[test]
    fn astrbot_defaults_are_preserved() {
        let settings = Settings::default();
        assert_eq!(settings.default_duration_seconds, 600);
        assert_eq!(settings.max_reason_length, 500);
        assert_eq!(settings.max_records_per_group, 500);
    }

    #[test]
    fn response_contract_uses_success_and_message() {
        let response: Value =
            serde_json::from_str(&json_result(true, "ok", json!({ "record_id": "abc" })).unwrap())
                .unwrap();
        assert_eq!(response["success"], true);
        assert_eq!(response["message"], "ok");
    }

    #[test]
    fn audit_failure_reports_partial_success_and_forbids_retry() {
        let response = external_operation_result(
            json!({ "record_id": "abc" }),
            vec!["injected audit failure".to_string()],
        );
        assert_eq!(response["success"], true);
        assert_eq!(response["operation_succeeded"], true);
        assert_eq!(response["audit_succeeded"], false);
        assert_eq!(response["do_not_retry"], true);
        assert!(response["message"].as_str().unwrap().contains("请勿重试"));
    }

    #[test]
    fn external_failure_remains_retryable() {
        let response = failure(anyhow::anyhow!("injected external failure"));
        assert_eq!(response["success"], false);
        assert_eq!(response["operation_succeeded"], false);
        assert_eq!(response["do_not_retry"], false);
    }
}
