//! 删除历史的二次确认。
//!
//! 删除不可逆，所以要先发一个挑战码再由同一主体确认
//! （`DeleteConfirmations`）。挑战码有 TTL 也有条数上限——待确认的请求堆着不
//! 处理，本身就是一种泄漏。
//!
//! 确认必须来自**同一个主体**（`DeletePrincipal`）：群里 A 发起、B 确认，就等
//! 于没有确认。

use crate::platforms::plugins::message_history::tools::*;

pub(crate) const DELETE_CONFIRMATION_TTL: Duration = Duration::from_secs(5 * 60);

pub(crate) const MAX_DELETE_CONFIRMATIONS: usize = 128;

pub(crate) const MAX_CONFIRMATION_TOKEN_BYTES: usize = 128;

#[derive(Clone, Default)]
pub(crate) struct DeleteConfirmations {
    pub(crate) pending: Arc<Mutex<HashMap<String, PendingDelete>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeletePrincipal {
    pub(crate) platform: String,
    pub(crate) account_id: String,
    pub(crate) sender_id: String,
    pub(crate) conversation_scope: String,
}

impl DeletePrincipal {
    pub(crate) fn from_context(context: &PlatformTurnContext) -> Self {
        Self {
            platform: context.conversation.platform.clone(),
            account_id: context.conversation.account_id.clone(),
            sender_id: context.sender_id.clone(),
            conversation_scope: context.conversation.scope_key(),
        }
    }
}

pub(crate) struct PendingDelete {
    pub(crate) principal: DeletePrincipal,
    pub(crate) request: DeleteRequest,
    pub(crate) confirmation_phrase: String,
    pub(crate) issued_message_id: String,
    pub(crate) expires_at: Instant,
}

pub(crate) struct DeleteChallenge {
    pub(crate) confirmation_token: String,
    pub(crate) confirmation_phrase: String,
    pub(crate) scope: String,
    pub(crate) mode: String,
}

impl DeleteConfirmations {
    pub(crate) fn issue(
        &self,
        principal: DeletePrincipal,
        request: DeleteRequest,
        issued_message_id: String,
    ) -> DeleteChallenge {
        let token = random_confirmation_token();
        let scope = describe_scope(&request.scope);
        let mode = describe_delete_request(&request);
        let phrase = format!("确认删除 Miyu 历史 范围={scope} 模式={mode} {token}");
        let now = Instant::now();
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, entry| entry.expires_at > now && entry.principal != principal);
        if pending.len() >= MAX_DELETE_CONFIRMATIONS {
            if let Some(oldest) = pending
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(token, _)| token.clone())
            {
                pending.remove(&oldest);
            }
        }
        pending.insert(
            token.clone(),
            PendingDelete {
                principal,
                request,
                confirmation_phrase: phrase.clone(),
                issued_message_id,
                expires_at: now + DELETE_CONFIRMATION_TTL,
            },
        );
        DeleteChallenge {
            confirmation_token: token,
            confirmation_phrase: phrase,
            scope,
            mode,
        }
    }

    pub(crate) fn take_confirmed(
        &self,
        token: &str,
        principal: &DeletePrincipal,
        current_message_id: &str,
        current_message: &str,
    ) -> Result<DeleteRequest> {
        let token = token.trim();
        let now = Instant::now();
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, entry| entry.expires_at > now);
        let entry = pending
            .get(token)
            .context("the history deletion confirmation is missing or expired")?;
        if &entry.principal != principal {
            bail!("the history deletion confirmation belongs to another administrator");
        }
        if entry.issued_message_id == current_message_id {
            bail!("history deletion must be confirmed in a later administrator message");
        }
        if current_message.trim() != entry.confirmation_phrase {
            bail!(
                "the administrator must send the exact confirmation phrase in a new message: {}",
                entry.confirmation_phrase
            );
        }
        Ok(pending
            .remove(token)
            .expect("the checked confirmation still exists")
            .request)
    }
}

pub(crate) fn random_confirmation_token() -> String {
    let mut random = [0u8; 18];
    OsRng.fill_bytes(&mut random);
    format!("history-delete-{}", hex::encode(random))
}

pub(crate) fn register_delete(
    registry: &mut ToolRegistry,
    context: Arc<PlatformTurnContext>,
    store: HistoryStore,
    settings: Arc<QqMessageHistoryPluginSettings>,
    confirmations: DeleteConfirmations,
) {
    registry.register(
        ToolSpec::new(
            "delete_real_chat_history",
            "Permanently delete QQ real-chat history with server-enforced two-step confirmation. First use action=request; then the same administrator must send the exact returned confirmation phrase in a new message before action=confirm can succeed.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["request", "confirm"] },
                    "mode": { "type": "string", "enum": ["all", "keep_days"] },
                    "keep_days": { "type": "integer", "minimum": 1 },
                    "sender_id": { "type": "string", "description": "仅删除此发送者 QQ 的消息" },
                    "conversation_kind": { "type": "string", "enum": ["group", "private"] },
                    "conversation_id": { "type": "string", "description": "群号或私聊对方 QQ 号" },
                    "group_id": { "type": "string" },
                    "all_conversations": { "type": "boolean", "default": false },
                    "all_groups": { "type": "boolean", "default": false },
                    "start_time": { "type": "string" },
                    "end_time": { "type": "string" },
                    "confirmation_token": { "type": "string", "description": "For action=confirm, use the opaque token returned by action=request. The current administrator message must also exactly equal the returned confirmation phrase." }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            move |arguments| {
                let context = context.clone();
                let store = store.clone();
                let settings = settings.clone();
                let confirmations = confirmations.clone();
                async move {
                    delete(arguments, context, store, settings, confirmations).await
                }
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
    confirmations: DeleteConfirmations,
) -> Result<String> {
    if !effective_admin(&context) {
        bail!("only a configured Miyu platform administrator may delete history");
    }
    let principal = DeletePrincipal::from_context(&context);
    match required_string(&arguments, "action")?.as_str() {
        "request" => {
            let event = live_admin_message(&context)?;
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
            let challenge = confirmations.issue(principal, request, event.message_id.clone());
            Ok(json!({
                "ok": false,
                "requires_confirmation": true,
                "confirmation_token": challenge.confirmation_token,
                "confirmation_phrase": challenge.confirmation_phrase,
                "expires_in_seconds": DELETE_CONFIRMATION_TTL.as_secs(),
                "scope": challenge.scope,
                "mode": challenge.mode,
                "instruction": "请让当前管理员在下一条 QQ 消息中原样发送 confirmation_phrase；不要自行调用确认。"
            })
            .to_string())
        }
        "confirm" => {
            let token = required_string(&arguments, "confirmation_token")?;
            if token.len() > MAX_CONFIRMATION_TOKEN_BYTES {
                bail!("confirmation_token is too long");
            }
            let event = live_admin_message(&context)?;
            let request =
                confirmations.take_confirmed(&token, &principal, &event.message_id, &event.text)?;
            let report = store.delete_history(request).await?;
            Ok(json!({ "ok": true, "report": report }).to_string())
        }
        _ => bail!("action must be request or confirm"),
    }
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
