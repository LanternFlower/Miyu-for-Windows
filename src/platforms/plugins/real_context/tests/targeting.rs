//! 主动回复的目标选择与提示词拼装。

use crate::platforms::plugins::real_context::*;
use super::shared::*;

#[test]
fn disabled_targeting_returns_none_and_core_fallback_is_preserved_exactly() {
    let settings = RealContextPluginSettings {
        reply_target_enable: false,
        ..RealContextPluginSettings::default()
    };
    assert_eq!(response_target(&inbound_event(), &settings), None);

    let core = TriggerDecision {
        should_reply: true,
        content: "核心触发内容".to_string(),
        response_target: Some(ResponseTarget::quoted("core-message", "core-user")),
    };
    let mut changed = TriggerDecision {
        should_reply: false,
        content: "插件临时内容".to_string(),
        response_target: Some(ResponseTarget {
            message_id: "guessed-message".to_string(),
            user_id: "guessed-user".to_string(),
            quote: true,
            mention: true,
            explicit_mention_user_ids: Vec::new(),
        }),
    };

    restore_trigger_decision(&mut changed, &core);

    assert_eq!(changed.should_reply, core.should_reply);
    assert_eq!(changed.content, core.content);
    assert_eq!(changed.response_target, core.response_target);
}

#[test]
fn active_target_limits_keep_recent_text_and_supplements() {
    let event = inbound_event();
    let mut targets = (0..12)
        .map(|index| {
            let mut target = active_reply_target(&event);
            target.message_id = format!("text-{index}");
            target.content = format!("text {index}");
            target
        })
        .collect::<Vec<_>>();
    targets.extend((0..8).map(|index| {
        let mut target = active_reply_target(&event);
        target.message_id = format!("image-{index}");
        target.content.clear();
        target.supplemental = true;
        target
    }));

    normalize_active_targets(&mut targets, &event.sender_id);

    assert_eq!(
        targets.iter().filter(|target| !target.supplemental).count(),
        8
    );
    assert_eq!(
        targets.iter().filter(|target| target.supplemental).count(),
        5
    );
    assert!(!targets.iter().any(|target| target.message_id == "text-0"));
    assert!(targets.iter().any(|target| target.message_id == "text-11"));
    assert!(!targets.iter().any(|target| target.message_id == "image-0"));
    assert!(targets.iter().any(|target| target.message_id == "image-7"));
}

#[test]
fn active_target_prompt_is_bounded_and_keeps_the_current_message() {
    let (_temp, context) = availability_context(BotSendAvailability::Available);
    let mut current = inbound_event();
    current.message_id = "current".to_string();
    let mut targets = (0..MAX_ACTIVE_TARGET_MESSAGES)
        .map(|index| {
            let mut target = active_reply_target(&current);
            target.message_id = format!("old-{index}");
            target.content = "旧".repeat(20_000);
            target
        })
        .collect::<Vec<_>>();
    targets.push(active_reply_target(&current));
    set_active_targets(&context, &targets);

    let current_content = format!("CURRENT:{}", "新".repeat(20_000));
    let prompt = active_target_prompt(&context, &current, &current_content);

    assert!(prompt.len() <= MAX_ACTIVE_TARGET_PROMPT_BYTES);
    assert!(prompt.contains("CURRENT:"));
    assert!(prompt.contains("earlier merged messages omitted due to length limits"));
    // 截断保留的头部是带标记的当前消息,而不是裸正文。
    assert!(prompt.starts_with("[New messages received this turn]\nCURRENT:"));
}

#[test]
fn supersede_inherits_targets_only_for_the_same_sender() {
    let plugin = RealContextPlugin::new();
    let (_temp, context) = availability_context(BotSendAvailability::Available);
    let event = inbound_event();
    let (cancel, _receiver) = tokio::sync::watch::channel(false);
    let target = active_reply_target(&event);
    plugin
        .runtime
        .lock()
        .unwrap()
        .session_mut(&runtime_session_key(&context), Instant::now())
        .pending
        .insert(
            event.sender_id.clone(),
            PendingReply {
                generation: 1,
                started: Instant::now(),
                trigger: TriggerKind::Probability,
                committed: false,
                reactions: Vec::new(),
                targets: vec![target],
                cancel,
            },
        );

    assert!(plugin.preempt_inbound(&context, &event).unwrap());
    let inherited = active_targets_from_context(&context);
    assert_eq!(inherited.len(), 1);
    assert_eq!(inherited[0].sender_id, event.sender_id);

    active_judgement_skip::apply_active_judgement_skip_editor_changes(
        &context.state_store,
        &[],
        &[event.sender_id.parse().unwrap()],
    )
    .unwrap();
    assert!(!plugin.preempt_inbound(&context, &event).unwrap());

    let mut other = event.clone();
    other.sender_id = "other-user".to_string();
    assert!(!plugin.preempt_inbound(&context, &other).unwrap());
}
