//! 删除历史：管理员单步执行。
//!
//! 08-21 用户裁定移除两步确认（挑战码 + 原样复述短语的流程整体退场）。
//! 身份门槛保留两道：配置内管理员（`effective_admin`）+ 请求必须来自一条
//! 活的平台消息且发送者/会话与回合一致（`live_admin_message`），防止伪造
//! 上下文触发删除。

use crate::platforms::plugins::message_history::tools::*;

pub(crate) fn register_delete(
    registry: &mut ToolRegistry,
    context: Arc<PlatformTurnContext>,
    store: HistoryStore,
    settings: Arc<QqMessageHistoryPluginSettings>,
) {
    registry.register(
        ToolSpec::new(
            "delete_real_chat_history",
            "Permanently delete QQ real-chat history. Administrator only. Executes immediately and reports what was deleted.",
            json!({
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "enum": ["all", "keep_days"] },
                    "keep_days": { "type": "integer", "minimum": 1 },
                    "sender_id": { "type": "string", "description": "仅删除此发送者 QQ 的消息" },
                    "conversation_kind": { "type": "string", "enum": ["group", "private"] },
                    "conversation_id": { "type": "string", "description": "群号或私聊对方 QQ 号" },
                    // group_id 别名已从 schema 撤下(08-21 token-diet),处理器仍兼容。
                    "all_conversations": { "type": "boolean", "default": false },
                    "all_groups": { "type": "boolean", "default": false },
                    "start_time": { "type": "string" },
                    "end_time": { "type": "string" }
                },
                "required": ["mode"],
                "additionalProperties": false
            }),
            move |arguments| {
                let context = context.clone();
                let store = store.clone();
                let settings = settings.clone();
                async move { delete(arguments, context, store, settings).await }
            },
        )
        .writes()
        .with_display_name("Delete real chat history"),
    );
}

pub(crate) async fn delete(
    arguments: Value,
    context: Arc<PlatformTurnContext>,
    store: HistoryStore,
    settings: Arc<QqMessageHistoryPluginSettings>,
) -> Result<String> {
    if !effective_admin(&context) {
        bail!("only a configured Miyu platform administrator may delete history");
    }
    live_admin_message(&context)?;
    let scope = history_scope(
        &arguments,
        &context,
        settings.allow_cross_conversation_search,
    )?;
    let mut request = match required_string(&arguments, "mode")?.as_str() {
        "all" => DeleteRequest::all(scope, now_unix()),
        "keep_days" => DeleteRequest::keep_days(
            scope,
            positive_u32(&arguments, "keep_days")?
                .context("keep_days is required for mode=keep_days")?,
            now_unix(),
        )?,
        _ => bail!("mode must be all or keep_days"),
    };
    request.sender_id = optional_id(&arguments, "sender_id")?;
    let (since, until) = parsed_time_range(&arguments)?;
    request.since = since;
    request.until = until;
    let scope_text = describe_scope(&request.scope);
    let mode_text = describe_delete_request(&request);
    let report = store.delete_history(request).await?;
    Ok(format!(
        "deleted {} message(s) (recalls {}, boundaries {}) — scope {scope_text}, mode {mode_text}",
        report.messages_deleted, report.recalls_deleted, report.boundaries_deleted
    ))
}

pub(crate) fn live_admin_message(
    context: &PlatformTurnContext,
) -> Result<&crate::platforms::PlatformInboundEvent> {
    let event = context
        .inbound_event()
        .context("history deletion requires a live platform message")?;
    if event.kind != PlatformInboundEventKind::Message
        || event.sender_id != context.sender_id
        || event.conversation != context.conversation
    {
        bail!("history deletion identity does not match the current platform message");
    }
    Ok(event)
}

pub(crate) fn describe_scope(scope: &HistoryScope) -> String {
    match scope {
        HistoryScope::Group(group) => format!(
            "{}:{}:group:{}",
            group.platform(),
            group.account_id(),
            group.group_id()
        ),
        HistoryScope::Private(conversation) => format!(
            "{}:{}:private:{}",
            conversation.platform(),
            conversation.account_id(),
            conversation.conversation_id()
        ),
        HistoryScope::AllGroups(account) => {
            format!("{}:{}:all_groups", account.platform(), account.account_id())
        }
        HistoryScope::Account(account) => {
            format!(
                "{}:{}:all_conversations",
                account.platform(),
                account.account_id()
            )
        }
    }
}

pub(crate) fn describe_delete_mode(mode: DeleteMode) -> String {
    match mode {
        DeleteMode::All => "all".to_string(),
        DeleteMode::KeepDays(days) => format!("keep_days:{days}"),
    }
}

pub(crate) fn describe_delete_request(request: &DeleteRequest) -> String {
    let mut description = describe_delete_mode(request.mode);
    if let Some(sender_id) = request.sender_id.as_deref() {
        description.push_str(&format!(":sender={sender_id}"));
    }
    if let Some(since) = request.since {
        description.push_str(&format!(":from={since}"));
    }
    if let Some(until) = request.until {
        description.push_str(&format!(":to={until}"));
    }
    description
}
