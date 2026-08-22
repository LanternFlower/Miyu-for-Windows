//! 一条入站消息从事件到回合的分发。
//!
//! `handle_message_with_activity` 是这条路上最长的一段：判准入、取图、拼上下
//! 文、决定回给谁、建会话、跑回合。它长是因为**这些判断彼此耦合**——准入结果
//! 影响要不要取图，取图结果影响用哪个模型池。拆成小函数只会把耦合变成一堆需要
//! 来回传的参数。

use crate::platforms::onebot::*;

#[derive(Clone, Copy, Debug)]
pub(in crate::platforms::onebot) enum Target {
    Private { user_id: i64 },
    Group { group_id: i64 },
}

impl Target {
    pub(in crate::platforms::onebot) fn kind(self) -> &'static str {
        match self {
            Self::Private { .. } => "private",
            Self::Group { .. } => "group",
        }
    }

    pub(in crate::platforms::onebot) fn conversation_id(self) -> i64 {
        match self {
            Self::Private { user_id } => user_id,
            Self::Group { group_id } => group_id,
        }
    }
}

pub(in crate::platforms::onebot) fn message_event(
    target: Target,
    event: &Value,
    parsed: &InboundMessage,
) -> PlatformInboundEvent {
    message_event_at(target, event, parsed, Instant::now(), None)
}

pub(in crate::platforms::onebot) fn message_event_at(
    target: Target,
    event: &Value,
    parsed: &InboundMessage,
    received_at: Instant,
    message_position: Option<PlatformMessagePosition>,
) -> PlatformInboundEvent {
    let self_id = event.get("self_id").and_then(Value::as_i64).unwrap_or(0);
    PlatformInboundEvent {
        kind: PlatformInboundEventKind::Message,
        conversation: platform_conversation(target, self_id),
        conversation_display_name: None,
        message_id: event
            .get("message_id")
            .and_then(value_id_string)
            .unwrap_or_default(),
        sender_id: event
            .get("user_id")
            .and_then(value_id_string)
            .unwrap_or_default(),
        sender_display_name: event_sender_display_name(event),
        operator_id: None,
        timestamp: event.get("time").and_then(Value::as_i64).unwrap_or(0),
        received_at,
        message_position,
        ingress_order: None,
        text: parsed.text.clone(),
        reply_to_message_id: parsed.reply_to_message_id.clone(),
        replied_message: None,
        mentioned_user_ids: parsed.mentioned_user_ids.clone(),
        mentioned_users: Vec::new(),
        mentioned_bot: parsed.at_self,
        media: parsed.media.clone(),
        notice_sub_type: None,
        duration_seconds: None,
    }
}

pub(in crate::platforms::onebot) async fn handle_message(
    state: DaemonState,
    conn: ConnectionHandle,
    event: Value,
    ingress_order: i64,
) {
    let self_id = event.get("self_id").and_then(Value::as_i64).unwrap_or(0);
    let activity = observe_message_activity(&state, &event, self_id, Instant::now());
    handle_message_with_activity(state, conn, event, ingress_order, activity).await;
}

pub(in crate::platforms::onebot) async fn handle_message_with_activity(
    state: DaemonState,
    conn: ConnectionHandle,
    event: Value,
    ingress_order: i64,
    activity: Option<InboundMessageActivity>,
) {
    // 先做**不需要配置**的判断：自己发的、缺 id 的、非私聊/群聊的事件直接丢。
    //
    // 这几项原来排在深拷贝之后，于是每条被丢弃的事件也要付一次整份 AppConfig
    // 的拷贝（实测 27.8 µs / 12KB 配置；23.7KB 的真实配置约 55 µs）。群里
    // 机器人自己的消息、各类通知事件都会走到这儿，白付。
    let user_id = event.get("user_id").and_then(Value::as_i64).unwrap_or(0);
    let self_id = event.get("self_id").and_then(Value::as_i64).unwrap_or(0);
    if user_id == 0 || user_id == self_id {
        return;
    }
    let message_type = event
        .get("message_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let target = match message_type {
        "private" => Target::Private { user_id },
        "group" => {
            let group_id = event.get("group_id").and_then(Value::as_i64).unwrap_or(0);
            if group_id == 0 {
                return;
            }
            Target::Group { group_id }
        }
        _ => return,
    };
    // 平台关掉时同样不必拷：在同一个锁作用域里先看一眼 bool
    let mut app_config = {
        let manager = state.manager.lock().unwrap();
        if !manager.config.platforms.qq.enabled {
            return;
        }
        manager.config.clone()
    };
    let config = app_config.platforms.qq.clone();
    let admission = admission_for_with_state(&config, &state.state_store, target, self_id, user_id);
    if !admission.allowed {
        return;
    }
    apply_admission_text_model_pool(&mut app_config, target, &admission);

    let mut parsed = parse_message(event.get("message"), event.get("raw_message"), self_id);
    if let Some(reason) = parsed.rejected_reason {
        tracing::warn!(
            target: "miyu::qq",
            self_id,
            sender_id = user_id,
            conversation_kind = target.kind(),
            conversation_id = target.conversation_id(),
            %reason,
            "{}",
            t("OneBot message rejected before plugin processing", "OneBot 消息在插件处理前被拒绝")
        );
        return;
    }
    let parsed_command = commands::parse(&app_config.platforms, parsed.text.trim());
    let mut inbound_event = message_event_at(
        target,
        &event,
        &parsed,
        activity
            .as_ref()
            .map(|activity| activity.received_at)
            .unwrap_or_else(Instant::now),
        activity.as_ref().map(|activity| activity.position),
    );
    inbound_event.ingress_order = Some(ingress_order);
    if parsed_command.is_none() && matches!(target, Target::Group { .. }) && config.show_group_name
    {
        inbound_event.conversation_display_name =
            resolve_group_name(&conn, self_id, target.conversation_id(), &event).await;
    }
    if parsed_command.is_none() && !parsed.mentioned_user_ids.is_empty() {
        inbound_event.mentioned_users =
            resolve_mentioned_users(&conn, self_id, target, &parsed.mentioned_user_ids).await;
    }
    let quoted_message_id = parsed_command
        .is_none()
        .then(|| {
            parsed.reply_to_message_id.as_deref().filter(|id| {
                event.get("message_id").and_then(value_id_string).as_deref() != Some(*id)
            })
        })
        .flatten();
    parsed.quoted_message_data = if let Some(quoted_message_id) = quoted_message_id {
        match get_message_data(&conn, quoted_message_id, QUOTED_MESSAGE_LOOKUP_TIMEOUT).await {
            Ok(data) => {
                let info = parse_message_info(&data, self_id)
                    .filter(|info| info.message_id == quoted_message_id)
                    .filter(|info| message_info_matches_target(info, target));
                if info.is_none() {
                    tracing::warn!(
                        target: "miyu::qq",
                        quoted_message_id,
                        "{}",
                        t("OneBot quoted-message metadata was missing or mismatched", "OneBot 引用消息元数据缺失或不匹配")
                    );
                }
                if info.is_some() {
                    inbound_event.replied_message = info;
                    Some(data)
                } else {
                    // Prevent the image merge stage from repeating an
                    // unscoped lookup for a cross-conversation message id.
                    parsed.reply_to_message_id = None;
                    None
                }
            }
            Err(error) => {
                tracing::warn!(
                    target: "miyu::qq",
                    error = %error,
                    quoted_message_id,
                    "{}",
                    t("OneBot quoted-message metadata lookup failed", "OneBot 引用消息元数据查询失败")
                );
                None
            }
        }
    } else {
        None
    };
    let context = match platform_turn_context_with_activity(
        &state,
        conn.clone(),
        target,
        &event,
        app_config,
        Some(inbound_event.clone()),
        activity.map(|activity| activity.handle),
    ) {
        Ok(context) => Arc::new(context),
        Err(error) => {
            tracing::warn!(target: "miyu::qq", error = %error, "{}", t("OneBot platform runtime initialization failed", "OneBot 平台运行时初始化失败"));
            return;
        }
    };

    // Classify group traffic before charging rate limits. Busy groups often
    // produce many messages that do not wake Miyu and must not starve actual
    // mentions or prefix commands.
    // Built-in commands own only their registered names. Other prefixed input
    // remains ordinary chat after plugins have had a chance to claim it.
    let plugin_command_response = if parsed_command.is_some() {
        None
    } else {
        context.handle_command(parsed.text.trim()).await
    };
    let builtin_command = if plugin_command_response.is_none() {
        parsed_command
    } else {
        None
    };

    // Plugins may supersede same-sender work before this message enters the
    // shared judgement/turn admission queue.
    let session_id = if plugin_command_response.is_none() && builtin_command.is_none() {
        match resolve_onebot_session(&state, &context, target, &event) {
            Ok(session_id) => Some(session_id),
            Err(error) => {
                tracing::warn!(target: "miyu::qq", error = %error, "{}", t("resolving the QQ session failed", "解析 QQ 会话失败"));
                if matches!(target, Target::Private { .. }) {
                    let _ = context
                        .send_bypass_plugins(OutboundMessage::text(
                            OutboundOrigin::Command,
                            t(
                                "Something went wrong while opening this conversation.",
                                "打开当前会话时出错了。",
                            ),
                        ))
                        .await;
                }
                return;
            }
        }
    } else {
        None
    };
    let core_trigger_content = (plugin_command_response.is_none() && builtin_command.is_none())
        .then(|| match target {
            Target::Private { .. } => Some(parsed.text.clone()),
            Target::Group { .. } => group_trigger_text(
                &config,
                &parsed,
                inbound_event.replied_message.as_ref(),
                self_id,
            ),
        })
        .flatten();
    if let Some(session_id) = session_id.as_deref() {
        // Group chats only accept follow-ups while a tool is executing (the
        // reservation guarantees same-round consumption); outside that window
        // group messages go through supersede/new-turn admission because other
        // people may be talking to each other. Private chats behave like the
        // REPL/WebUI instead: any message while a turn is active becomes a
        // follow-up to that turn, with the ingress reservation held when one
        // is available.
        let followup_target = if matches!(target, Target::Group { .. }) {
            reserve_tool_followup(
                &state,
                session_id,
                &context.conversation,
                &context.sender_id,
            )
            .map(|(run_id, turn_id, followup, reservation)| {
                (run_id, turn_id, followup, Some(reservation))
            })
        } else {
            platform_update_target(
                &state,
                session_id,
                &context.conversation,
                &context.sender_id,
            )
            .map(|(run_id, turn_id, followup)| {
                let reservation = followup.try_reserve();
                (run_id, turn_id, followup, reservation)
            })
        };
        if let Some((run_id, turn_id, followup, reservation)) = followup_target {
            let _ingress_reservation = reservation;
            let _enqueue_order = followup.lock_enqueue().await;
            let rate_decision = admission
                .rate_key
                .as_deref()
                .map_or(RateDecision::Allow, |key| {
                    state
                        .platforms
                        .rate
                        .lock()
                        .unwrap()
                        .check(key, admission.rate_limit)
                });
            if rate_decision != RateDecision::Allow {
                if rate_decision == RateDecision::DropWithNotice {
                    let _ = context
                        .send_bypass_plugins(OutboundMessage::text(
                            OutboundOrigin::Command,
                            t(
                                "Too many messages — please slow down a little.",
                                "消息太频繁了，请稍候再发。",
                            ),
                        ))
                        .await;
                }
                return;
            }
            match enqueue_tool_followup(
                &state,
                &conn,
                target,
                &event,
                parsed,
                &inbound_event,
                &context,
                &followup,
                session_id,
                &run_id,
                &turn_id,
                TurnUpdateMode::Followup,
            )
            .await
            {
                Ok(()) => tracing::info!(
                    target: "miyu::qq",
                    session_id,
                    sender_id = user_id,
                    message_id = %inbound_event.message_id,
                    "{}",
                    t("OneBot message queued as a follow-up to the active turn", "OneBot 消息已加入当前回合的后续队列")
                ),
                Err(error) => tracing::warn!(
                    target: "miyu::qq",
                    session_id,
                    sender_id = user_id,
                    error = %error,
                    "{}",
                    t("OneBot follow-up could not be queued", "OneBot 后续消息无法入队")
                ),
            }
            return;
        }
    }
    if let Some(session_id) = session_id.as_deref() {
        if context.preempt_inbound(&inbound_event) {
            if let Some((run_id, turn_id, followup)) = platform_update_target(
                &state,
                session_id,
                &context.conversation,
                &context.sender_id,
            ) {
                let _enqueue_order = followup.lock_enqueue().await;
                let result = enqueue_tool_followup(
                    &state,
                    &conn,
                    target,
                    &event,
                    parsed,
                    &inbound_event,
                    &context,
                    &followup,
                    session_id,
                    &run_id,
                    &turn_id,
                    TurnUpdateMode::Supersede,
                )
                .await;
                match result {
                    Ok(()) => {
                        // 覆盖成功:表情从旧消息转移到新消息,补救窗口从
                        // 新消息重新起算(链式覆盖)。
                        context.confirm_supersede(&inbound_event).await;
                        tracing::info!(
                            target: "miyu::qq",
                            session_id,
                            sender_id = user_id,
                            message_id = %inbound_event.message_id,
                            "{}",
                            t("OneBot message superseded the active generation", "OneBot 消息已取代当前生成")
                        )
                    }
                    Err(error) => tracing::warn!(
                        target: "miyu::qq",
                        session_id,
                        sender_id = user_id,
                        error = %error,
                        "{}",
                        t("OneBot active generation could not be superseded", "无法取代 OneBot 当前生成")
                    ),
                }
                return;
            }
            let manager = state.manager.lock().unwrap();
            for run in manager
                .active_runs
                .values()
                .filter(|run| &*run.session_id == session_id)
                .filter(|run| {
                    run.platform_followup.as_ref().is_some_and(|followup| {
                        followup.conversation == context.conversation
                            && followup.sender_id == context.sender_id
                    })
                })
            {
                run.request_cancel();
            }
        }
    }
    let session_limits = config.session_limits(
        match target {
            Target::Private { .. } => PlatformConversationKind::Private,
            Target::Group { .. } => PlatformConversationKind::Group,
        },
        &target.conversation_id().to_string(),
    );
    let session_turn_ticket = session_id.as_deref().map(|session_id| {
        state
            .platforms
            .session_turn_ticket(session_id, session_limits)
    });
    let session_turn = match session_turn_ticket {
        Some(ticket) => match ticket.acquire().await {
            Ok(lease) => Some(lease),
            // Dropped in silence. Announcing a full queue told the group
            // nothing it could act on — the backlog clears on its own — and
            // the apology itself cost a message at the exact moment the
            // conversation was already saturated. The log keeps it visible to
            // whoever runs the bot.
            Err(crate::platforms::SessionTurnAcquireError::Full) => {
                tracing::debug!(
                    target: "miyu::qq",
                    session_id = ?session_id,
                    sender_id = user_id,
                    message_id = %inbound_event.message_id,
                    "{}",
                    t(
                        "OneBot message discarded: the conversation queue is full",
                        "OneBot 消息已丢弃：当前会话等待队列已满"
                    )
                );
                return;
            }
            Err(crate::platforms::SessionTurnAcquireError::Closed) => return,
        },
        None => None,
    };
    if session_turn
        .as_ref()
        .is_some_and(|session_turn| !session_turn.is_valid())
    {
        context.after_turn_aborted().await;
        return;
    }
    let message_id = inbound_event.message_id.clone();
    if plugin_command_response.is_none() && builtin_command.is_none() {
        let trigger_content = core_trigger_content;
        let mut trigger = TriggerDecision {
            should_reply: trigger_content.is_some(),
            content: trigger_content.unwrap_or_else(|| parsed.text.clone()),
            // Reply targeting is owned by the real-context plugin. Keeping
            // the transport core neutral makes its quote/mention switches
            // authoritative and avoids an invisible default quote.
            response_target: None,
        };
        let rate_available = admission.rate_key.as_deref().is_none_or(|key| {
            state
                .platforms
                .rate
                .lock()
                .unwrap()
                .available(key, admission.rate_limit)
        });
        context.set_reply_rate_available(rate_available);
        context.observe_inbound(&inbound_event).await;
        context.decide_trigger(&inbound_event, &mut trigger).await;
        if !trigger.should_reply {
            return;
        }
        parsed.text = trigger.content;
        context.set_response_target(trigger.response_target);
    }

    tracing::info!(
        target: "miyu::qq",
        self_id,
        sender_id = user_id,
        conversation_kind = target.kind(),
        conversation_id = target.conversation_id(),
        %message_id,
        text_chars = parsed.text.chars().count(),
        images = parsed
            .images
            .len()
            .saturating_add(parsed.unresolved_image_files.len()),
        files = parsed.files.len(),
        command = plugin_command_response.is_some() || builtin_command.is_some(),
        "{}",
        t("OneBot message accepted", "OneBot 消息已接受")
    );

    // Built-in control commands bypass chat rate limits and preempt the
    // target session's active and queued work after authorization.
    //
    // 走完整的 send():命令输出和模型回复一样要过回复处理插件——/models
    // 这种长清单在 QQ 里刷屏,该转图就得转图。限流与目标预留都不受影响:
    // 前者在此之前就判完了,后者只对 FinalReply/Tool 生效。
    if let Some(command) = builtin_command {
        if let Some(response) =
            execute_builtin_command(&state, &context, target, &event, command).await
        {
            if let Err(error) = context.send(response).await {
                tracing::warn!(target: "miyu::qq", error = %error, "{}", t("OneBot built-in command response failed", "OneBot 内置命令响应失败"));
            } else {
                tracing::info!(target: "miyu::qq", self_id, sender_id = user_id, "{}", t("OneBot built-in command response sent", "OneBot 内置命令响应已发送"));
            }
        }
        return;
    }

    let decision = admission
        .rate_key
        .as_deref()
        .map_or(RateDecision::Allow, |key| {
            state
                .platforms
                .rate
                .lock()
                .unwrap()
                .check(key, admission.rate_limit)
        });
    match decision {
        RateDecision::Allow => {}
        RateDecision::DropSilently => {
            tracing::info!(
                target: "miyu::qq",
                self_id,
                sender_id = user_id,
                conversation_kind = target.kind(),
                conversation_id = target.conversation_id(),
                "{}",
                t("OneBot message rate-limited", "OneBot 消息已被限流")
            );
            context.after_turn_aborted().await;
            return;
        }
        RateDecision::DropWithNotice => {
            let notice_sent = sends_rate_limit_notice(target);
            tracing::info!(
                target: "miyu::qq",
                self_id,
                sender_id = user_id,
                conversation_kind = target.kind(),
                conversation_id = target.conversation_id(),
                notice_sent,
                "{}",
                t("OneBot message rate-limited", "OneBot 消息已被限流")
            );
            if notice_sent {
                let _ = context
                    .send_bypass_plugins(OutboundMessage::text(
                        OutboundOrigin::Command,
                        t(
                            "Too many messages — please slow down a little.",
                            "消息太频繁了，请稍候再发。",
                        ),
                    ))
                    .await;
            }
            context.after_turn_aborted().await;
            return;
        }
    }

    // Platform commands are independent of the LLM group wake trigger.
    // 与内置命令同理,插件命令的输出也要过回复处理插件。
    if let Some(response) = plugin_command_response {
        if let Err(error) = context.send(response).await {
            tracing::warn!(target: "miyu::qq", error = %error, "{}", t("OneBot plugin command response failed", "OneBot 插件命令响应失败"));
        } else {
            tracing::info!(target: "miyu::qq", self_id, sender_id = user_id, "{}", t("OneBot plugin command response sent", "OneBot 插件命令响应已发送"));
        }
        return;
    }
    let session_id = session_id.expect("non-command message has a resolved session");
    let session_turn = session_turn.expect("non-command message owns a session turn");
    let turn = build_and_run_turn(
        &state,
        &conn,
        target,
        &event,
        parsed,
        context.clone(),
        session_id,
    )
    .await;
    if !session_turn.is_valid() {
        context.after_turn_aborted().await;
        return;
    }
    match turn {
        Ok(Some(dispatch)) => match deliver_dispatch(&state, &context, dispatch).await {
            Err(error) => {
                tracing::warn!(target: "miyu::qq", error = %error, "{}", t("OneBot reply delivery failed", "OneBot 回复投递失败"));
                context.after_turn_aborted().await;
            }
            Ok(true) => {
                tracing::info!(
                    target: "miyu::qq",
                    self_id,
                    sender_id = user_id,
                    conversation_kind = target.kind(),
                    conversation_id = target.conversation_id(),
                    "{}",
                    t("OneBot reply delivered", "OneBot 回复已投递")
                );
            }
            Ok(false) => {}
        },
        Ok(None) => {
            if !context.turn_is_superseded() {
                context.after_turn_aborted().await;
            }
        }
        Err(error) => {
            tracing::warn!(target: "miyu::qq", error = %error, "{}", t("OneBot message handling failed", "OneBot 消息处理失败"));
            context.after_turn_aborted().await;
            if matches!(target, Target::Private { .. }) {
                let _ = context
                    .send_bypass_plugins(OutboundMessage::text(
                        OutboundOrigin::Command,
                        format!(
                            "{}{}",
                            t("Something went wrong: ", "出错了："),
                            safe_error_message(&error)
                        ),
                    ))
                    .await;
            }
        }
    }
}
