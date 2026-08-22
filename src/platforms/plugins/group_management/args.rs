//! 工具参数的校验与结果拼装。
//!
//! 目标 ID 来自模型，`validate_target` / `valid_id` 是执行不可逆动作（踢人、
//! 禁言）之前的最后一道。批量操作按目标逐个报结果
//! （`aggregate_target_results`）：三个里成功两个，不能报成整体成功或整体失败。

use crate::platforms::plugins::group_management::*;

pub(in crate::platforms::plugins::group_management) const MAX_TARGETS: usize = 32;

/// QQ caps a mute at 30 days; anything longer is rejected by the server, so
/// catch it here where the message can explain itself.
pub(in crate::platforms::plugins::group_management) const MAX_BAN_SECONDS: u64 = 30 * 24 * 60 * 60;

pub(in crate::platforms::plugins::group_management) async fn validate_target(
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

pub(in crate::platforms::plugins::group_management) fn resolve_targets(args: &Value, context: &PlatformTurnContext) -> Result<Vec<String>> {
    let mut values = Vec::new();
    // 经 string_list 收，容忍模型把数组写成「数组的 JSON 字符串」。踢人是
    // 有后果的操作，静默取不到目标比报错更糟。
    values.extend(
        crate::tools::string_list(args.get("user_ids"))
            .iter()
            .map(String::as_str)
            .flat_map(split_ids)
            .collect::<Vec<_>>(),
    );
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

pub(in crate::platforms::plugins::group_management) fn split_ids(value: &str) -> Vec<String> {
    value
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| (5..=12).contains(&part.len()))
        .map(str::to_string)
        .collect()
}

/// Renders a mute duration the way a person would say it, so the model can
/// sanity-check its own arithmetic against what it intended.
pub(in crate::platforms::plugins::group_management) fn humanize_seconds(seconds: u64) -> String {
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

pub(in crate::platforms::plugins::group_management) fn valid_id(value: &str) -> bool {
    (5..=12).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

pub(in crate::platforms::plugins::group_management) fn notice_member(
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

pub(in crate::platforms::plugins::group_management) fn bounded_reason(args: &Value, settings: &Settings) -> Result<String> {
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

pub(in crate::platforms::plugins::group_management) fn optional_id(args: &Value) -> Option<String> {
    args.get("user_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| valid_id(id))
        .map(str::to_string)
}

pub(in crate::platforms::plugins::group_management) fn limit(args: &Value) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize
}

/// Kick takes a real array so batching is discoverable from the schema rather
/// than buried in prose — the scalar form stays for single targets and for
/// falling back to mentions/reply.
pub(in crate::platforms::plugins::group_management) fn kick_schema() -> Value {
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

pub(in crate::platforms::plugins::group_management) fn reason_schema() -> Value {
    json!({ "type": "object", "properties": { "user_id": { "type": "string" }, "reason": { "type": "string" }, "confirmation_token": { "type": "string" } }, "additionalProperties": false })
}

pub(in crate::platforms::plugins::group_management) fn history_query_schema() -> Value {
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

pub(in crate::platforms::plugins::group_management) fn failure(error: anyhow::Error) -> Value {
    json!({
        "success": false,
        "operation_succeeded": false,
        "audit_succeeded": false,
        "do_not_retry": false,
        "message": error.to_string()
    })
}

pub(in crate::platforms::plugins::group_management) fn failure_for_target(error: anyhow::Error, user_id: &str) -> Value {
    let mut result = failure(error);
    result["user_id"] = json!(user_id);
    result
}

pub(in crate::platforms::plugins::group_management) fn external_operation_result(data: Value, audit_errors: Vec<String>) -> Value {
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
pub(in crate::platforms::plugins::group_management) fn aggregate_target_results(results: Vec<Value>) -> Value {
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

pub(in crate::platforms::plugins::group_management) fn json_result(success: bool, message: &str, data: Value) -> Result<String> {
    Ok(json!({ "success": success, "message": message, "data": data }).to_string())
}
