//! 工具参数的解析、时间范围与权限。
//!
//! `require_cross_conversation_access` 是这里的关键：查别的群的历史要额外权
//! 限，默认只能查当前会话——否则任何人都能借机器人窥探别的群。

use crate::platforms::plugins::message_history::tools::*;

pub(crate) const DEFAULT_ACTIVITY_RANKING_DAYS: i64 = 30;

pub(crate) const DEFAULT_ACTIVITY_RANKING_LIMIT: usize = 20;

pub(crate) const MAX_ACTIVITY_RANKING_LIMIT: usize = 200;

pub(crate) fn group_member_query(arguments: &Value) -> Result<String> {
    arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_string)
        .context(
            "query is required; usage: get_group_members_info({\"query\":\"QQ号、群名片或昵称\",\"limit\":10})",
        )
}

pub(crate) fn group_member_limit(arguments: &Value, max_results: usize) -> Result<usize> {
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .context(
            "limit must be a positive integer; usage: get_group_members_info({\"query\":\"张三\",\"limit\":10})",
        )?;
    if limit == 0 {
        bail!("limit must be a positive integer");
    }
    if limit > max_results {
        bail!("limit must not exceed the configured maximum of {max_results}");
    }
    Ok(limit)
}

pub(crate) fn group_member_match_rank(
    member: &PlatformGroupMember,
    query: &str,
    folded_query: &str,
) -> Option<u8> {
    if member.user_id == query {
        return Some(0);
    }
    if member.user_id.starts_with(query) {
        return Some(1);
    }
    if member.user_id.contains(query) {
        return Some(2);
    }

    let folded_card = member.card.to_lowercase();
    let folded_nickname = member.nickname.to_lowercase();
    if folded_card == folded_query || folded_nickname == folded_query {
        Some(0)
    } else if folded_card.starts_with(folded_query) || folded_nickname.starts_with(folded_query) {
        Some(1)
    } else if folded_card.contains(folded_query) || folded_nickname.contains(folded_query) {
        Some(2)
    } else {
        None
    }
}

pub(crate) fn group_member_json(member: &PlatformGroupMember) -> Value {
    json!({
        "user_id": member.user_id,
        "display_name": member.display_name(),
        "username": member.nickname,
        "nickname": member.nickname,
        "card": member.card,
        "role": member.role,
        "title": member.title,
        "avatar_url": crate::platforms::avatar::user_avatar_url(
            &member.user_id,
            crate::platforms::avatar::DEFAULT_AVATAR_SIZE,
        )
    })
}

pub(crate) fn history_scope(
    arguments: &Value,
    context: &PlatformTurnContext,
    allow_cross_group: bool,
) -> Result<HistoryScope> {
    let all_conversations = arguments
        .get("all_conversations")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let all_groups = arguments
        .get("all_groups")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if all_conversations || all_groups {
        require_cross_conversation_access(context, allow_cross_group)?;
        let account = crate::platforms::plugins::message_history::account_key(context)?;
        // all_groups 按字面语义只覆盖群聊:此前它与 all_conversations 完全
        // 等价(含全部私聊),模型按参数名删"群历史"会连私聊一起删。
        return Ok(if all_conversations {
            HistoryScope::Account(account)
        } else {
            HistoryScope::AllGroups(account)
        });
    }

    let conversation_id = optional_id(arguments, "conversation_id")?;
    let group_id = optional_id(arguments, "group_id")?;
    if conversation_id.is_some() && group_id.is_some() {
        bail!("use conversation_id or group_id, not both");
    }
    let explicit_id = conversation_id.or(group_id.clone());
    let kind = match optional_string(arguments, "conversation_kind")?.as_deref() {
        Some("group") => ConversationKind::Group,
        Some("private") => ConversationKind::Private,
        Some(_) => bail!("conversation_kind must be group or private"),
        None if group_id.is_some() => ConversationKind::Group,
        None => context.conversation.kind,
    };
    let current = crate::platforms::plugins::message_history::conversation_key(context)?;
    let Some(conversation_id) = explicit_id else {
        return Ok(match context.conversation.kind {
            ConversationKind::Group => HistoryScope::Group(current),
            ConversationKind::Private => HistoryScope::Private(current),
        });
    };
    let selected = ConversationKey::for_kind(
        context.conversation.platform.clone(),
        context.conversation.account_id.clone(),
        kind,
        conversation_id,
    )?;
    if selected != current {
        require_cross_conversation_access(context, allow_cross_group)?;
    }
    Ok(match kind {
        ConversationKind::Group => HistoryScope::Group(selected),
        ConversationKind::Private => HistoryScope::Private(selected),
    })
}

pub(crate) fn require_cross_conversation_access(
    context: &PlatformTurnContext,
    allow_cross_conversation: bool,
) -> Result<()> {
    if !allow_cross_conversation {
        bail!("cross-conversation history access is disabled");
    }
    if !effective_admin(context) {
        bail!("only a Miyu platform administrator may access another conversation's history");
    }
    Ok(())
}

pub(crate) fn effective_admin(context: &PlatformTurnContext) -> bool {
    context.conversation.platform == ONEBOT_PLATFORM
        && context.with_current_config(|config| {
            is_effective_admin(
                &config.platforms.qq,
                &context.state_store,
                &context.conversation.account_id,
                &context.sender_id,
            )
        })
}

pub(crate) fn parsed_time_range(arguments: &Value) -> Result<(Option<i64>, Option<i64>)> {
    let since = optional_string(arguments, "start_time")?
        .as_deref()
        .map(|value| parse_time(value, false))
        .transpose()?;
    let until = optional_string(arguments, "end_time")?
        .as_deref()
        .map(|value| parse_time(value, true))
        .transpose()?;
    if since.zip(until).is_some_and(|(since, until)| since > until) {
        bail!("start_time must not be later than end_time");
    }
    Ok((since, until))
}

pub(crate) fn apply_time_filter(arguments: &Value, query: &mut SearchQuery) -> Result<()> {
    let (since, until) = parsed_time_range(arguments)?;
    if since.is_some() || until.is_some() {
        query.since = since;
        query.until = until;
    } else if let Some(days) = positive_u32(arguments, "days")? {
        query.since = Some(now_unix().saturating_sub(i64::from(days) * 86_400));
    }
    Ok(())
}

pub(crate) fn explicit_or_current_group(
    arguments: &Value,
    context: &PlatformTurnContext,
    allow_cross_group: bool,
) -> Result<GroupKey> {
    match history_scope(arguments, context, allow_cross_group)? {
        HistoryScope::Group(group) => Ok(group),
        HistoryScope::Private(_) => bail!("this operation requires a group conversation"),
        HistoryScope::AllGroups(_) | HistoryScope::Account(_) => {
            bail!("this operation requires one group conversation")
        }
    }
}

pub(crate) fn required_string(arguments: &Value, key: &str) -> Result<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .with_context(|| format!("{key} is required"))
}

pub(crate) fn optional_id(arguments: &Value, key: &str) -> Result<Option<String>> {
    let value = match arguments.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(value)) => value.trim().to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(_) => bail!("{key} must be a QQ numeric id"),
    };
    if value.is_empty() {
        return Ok(None);
    }
    if !value.bytes().all(|byte| byte.is_ascii_digit()) || value == "0" {
        bail!("{key} must be a positive QQ numeric id");
    }
    Ok(Some(value))
}

pub(crate) fn required_id(arguments: &Value, key: &str) -> Result<String> {
    optional_id(arguments, key)?.with_context(|| format!("{key} is required"))
}

pub(crate) fn optional_string(arguments: &Value, key: &str) -> Result<Option<String>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            Ok((!value.trim().is_empty()).then(|| value.trim().to_string()))
        }
        Some(_) => bail!("{key} must be a string"),
    }
}

pub(crate) fn optional_i64(arguments: &Value, key: &str) -> Result<Option<i64>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .with_context(|| format!("{key} must be an integer")),
    }
}

pub(crate) fn parse_time(value: &str, end_of_day: bool) -> Result<i64> {
    let value = value.trim();
    if let Ok(timestamp) = value.parse::<i64>() {
        return Ok(timestamp);
    }
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp.timestamp());
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(value) = NaiveDateTime::parse_from_str(value, format) {
            return local_timestamp(value, end_of_day)
                .with_context(|| format!("{value} is not a valid local time"));
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        let time = if end_of_day {
            date.and_hms_opt(23, 59, 59)
        } else {
            date.and_hms_opt(0, 0, 0)
        }
        .context("date is outside the supported range")?;
        return local_timestamp(time, end_of_day)
            .with_context(|| format!("{value} is not a valid local date"));
    }
    bail!(
        "invalid time {value:?}; use a Unix timestamp, RFC 3339, YYYY-MM-DD, or YYYY-MM-DD HH:MM[:SS]"
    )
}

pub(crate) fn local_timestamp(value: NaiveDateTime, prefer_latest: bool) -> Option<i64> {
    let local = Local.from_local_datetime(&value);
    let resolved = if prefer_latest {
        local.latest()
    } else {
        local.earliest()
    }?;
    Some(resolved.timestamp())
}

pub(crate) fn format_time(timestamp: i64) -> String {
    Local
        .timestamp_opt(timestamp, 0)
        .earliest()
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

pub(crate) fn positive_u32(arguments: &Value, key: &str) -> Result<Option<u32>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let raw = value
                .as_u64()
                .with_context(|| format!("{key} must be a positive integer"))?;
            let value = u32::try_from(raw).with_context(|| format!("{key} is too large"))?;
            if value == 0 {
                bail!("{key} must be positive");
            }
            Ok(Some(value))
        }
    }
}

pub(crate) fn history_limit_ceiling(settings: &QqMessageHistoryPluginSettings) -> usize {
    if settings.history_search_max_results == 0 {
        settings.history_safe_page_limit
    } else {
        settings
            .history_search_max_results
            .min(settings.history_safe_page_limit)
    }
    .clamp(1, 1_000)
}

pub(crate) fn limit(arguments: &Value, configured: usize, safety_limit: usize) -> usize {
    let ceiling = if configured == 0 {
        safety_limit
    } else {
        configured.min(safety_limit)
    }
    .clamp(1, 1_000);
    arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(ceiling)
        .clamp(1, ceiling)
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
