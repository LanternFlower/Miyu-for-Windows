//! 触发判定与延续窗口。

use crate::platforms::plugins::real_context::*;
use super::shared::*;

#[test]
fn explicit_direct_trigger_precedes_moderation_only_candidates() {
    assert_eq!(
        select_trigger(true, true, true, true, true),
        Some(TriggerKind::Direct)
    );
    assert_eq!(
        select_trigger(false, true, true, true, true),
        Some(TriggerKind::Moderation)
    );
}

#[test]
fn direct_trigger_judgement_respects_takeover_and_privileged_bypass() {
    let mut settings = RealContextPluginSettings::default();
    settings.takeover_direct_trigger_enable = false;
    assert!(!active_judgement_allowed(&settings, true, false, false));
    assert!(active_judgement_allowed(&settings, false, false, false));

    settings.takeover_direct_trigger_enable = true;
    assert!(active_judgement_allowed(&settings, true, false, false));
    assert!(!active_judgement_allowed(&settings, true, true, false));

    settings.privileged_direct_trigger_skip_active_judgement = false;
    assert!(active_judgement_allowed(&settings, true, true, false));
    assert!(!active_judgement_allowed(&settings, false, false, true));
    assert!(!active_judgement_allowed(&settings, true, true, true));
}

#[test]
fn skipped_social_judgement_preserves_moderation_only_trigger() {
    assert_eq!(
        select_trigger_for_policy(false, true, true, true, true, true),
        Some(TriggerKind::Moderation)
    );
    assert_eq!(
        select_trigger_for_policy(true, true, true, true, true, true),
        Some(TriggerKind::Direct)
    );
}

#[test]
fn continuation_window_is_inclusive_at_its_boundary() {
    let settings = RealContextPluginSettings::default();
    assert_eq!(settings.continuation_window_seconds, 15);
    let window = Duration::from_secs(settings.continuation_window_seconds);
    let started = Instant::now();
    let mut session = SessionRuntime::new(started);
    session.mark_continuation("30000", started, &settings);

    assert!(session.continuation_match("30000", started + window, true));
    assert!(!session.continuation_match(
        "30000",
        started + window + Duration::from_nanos(1),
        true,
    ));
}

#[test]
fn replying_inside_the_window_keeps_extending_it() {
    // The turn cap used to end a continuation after a few exchanges even
    // while the user kept talking; now only silence closes it.
    let settings = RealContextPluginSettings::default();
    let window = Duration::from_secs(settings.continuation_window_seconds);
    let mut now = Instant::now();
    let mut session = SessionRuntime::new(now);
    session.mark_continuation("30000", now, &settings);

    for _ in 0..10 {
        now += window - Duration::from_secs(1);
        assert!(
            session.continuation_match("30000", now, true),
            "the window should still be open"
        );
        // A reply landed inside the window: restart the clock.
        session.mark_continuation("30000", now, &settings);
    }

    // Silence past the window still closes it.
    assert!(!session.continuation_match("30000", now + window + Duration::from_secs(1), true));
}

#[test]
fn a_different_speaker_does_not_inherit_the_window() {
    let settings = RealContextPluginSettings::default();
    let started = Instant::now();
    let mut session = SessionRuntime::new(started);
    session.mark_continuation("30000", started, &settings);
    assert!(!session.continuation_match("40000", started, true));
}

#[tokio::test]
async fn direct_trigger_bypass_adds_and_cleans_up_the_waiting_reaction() {
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let (_temp, context) = test_context(Arc::new(ReactionAdapter {
        reactions: reactions.clone(),
    }));
    let plugin = RealContextPlugin::new();
    let event = inbound_event();
    // The bypass path under test requires takeover to stay off.
    let mut settings = RealContextPluginSettings::default();
    settings.takeover_direct_trigger_enable = false;
    let mut decision = TriggerDecision {
        should_reply: true,
        content: event.text.clone(),
        response_target: None,
    };

    plugin
        .decide_group_trigger(&context, &event, &mut decision, &settings)
        .await
        .unwrap();
    assert!(decision.should_reply);
    assert_eq!(
        reactions.lock().unwrap().as_slice(),
        &[(
            event.message_id.clone(),
            settings.active_reply_reaction_emoji_ids[0].to_string(),
            true,
        )]
    );

    plugin.after_turn_aborted(&context).await.unwrap();
    assert_eq!(reactions.lock().unwrap().last().unwrap().2, false);
}

#[tokio::test]
async fn correction_within_window_supersedes_committed_reply_and_moves_reactions() {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let (_temp, context) = test_context(Arc::new(ReactionAdapter {
        reactions: recorded.clone(),
    }));
    let plugin = RealContextPlugin::new();
    let settings = RealContextPluginSettings::default();
    let first = inbound_event();
    // 已承诺的回复(直触发或判断已通过),表情挂在旧消息上
    plugin.register_committed_pending(
        &runtime_session_key(&context),
        &first.sender_id,
        TriggerKind::Direct,
        vec![("message-1".to_string(), "289".to_string())],
        vec![active_reply_target(&first)],
    );
    // 补救窗口内同发送者的新消息:不再判断,直接顶替
    let mut correction = inbound_event();
    correction.message_id = "message-2".to_string();
    correction.text = "说错了,是另一件事".to_string();
    let mut decision = TriggerDecision {
        should_reply: false,
        content: correction.text.clone(),
        response_target: None,
    };
    plugin
        .decide_group_trigger(&context, &correction, &mut decision, &settings)
        .await
        .unwrap();
    assert!(decision.should_reply, "承诺沿用,补救消息应直接回复");
    let calls = recorded.lock().unwrap().clone();
    assert!(
        calls.contains(&("message-1".to_string(), "289".to_string(), false)),
        "旧消息的表情应被摘除: {calls:?}"
    );
    assert!(
        calls.contains(&("message-2".to_string(), "289".to_string(), true)),
        "新消息应贴上表情: {calls:?}"
    );
    // pending 已刷新:承诺保持、目标并入两条消息
    let runtime = plugin.runtime.lock().unwrap();
    let pending = runtime
        .sessions
        .get(&runtime_session_key(&context))
        .and_then(|session| session.pending.get(&first.sender_id))
        .expect("补救后 pending 应保留以支持链式覆盖");
    assert!(pending.committed);
    assert_eq!(pending.targets.len(), 2);
    assert_eq!(pending.reactions, vec![("message-2".to_string(), "289".to_string())]);
}

#[tokio::test]
async fn confirm_supersede_moves_reactions_and_restarts_the_window() {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let (_temp, context) = test_context(Arc::new(ReactionAdapter {
        reactions: recorded.clone(),
    }));
    let plugin = RealContextPlugin::new();
    let first = inbound_event();
    let (cancel, _receiver) = tokio::sync::watch::channel(false);
    let old_started = Instant::now() - Duration::from_secs(3);
    plugin
        .runtime
        .lock()
        .unwrap()
        .session_mut(&runtime_session_key(&context), Instant::now())
        .pending
        .insert(
            first.sender_id.clone(),
            PendingReply {
                generation: 7,
                started: old_started,
                trigger: TriggerKind::Direct,
                committed: true,
                reactions: vec![("message-1".to_string(), "289".to_string())],
                targets: vec![active_reply_target(&first)],
                cancel,
            },
        );
    let mut correction = inbound_event();
    correction.message_id = "message-2".to_string();
    plugin.confirm_supersede(&context, &correction).await;
    let calls = recorded.lock().unwrap().clone();
    assert!(calls.contains(&("message-1".to_string(), "289".to_string(), false)));
    assert!(calls.contains(&("message-2".to_string(), "289".to_string(), true)));
    let runtime = plugin.runtime.lock().unwrap();
    let pending = runtime
        .sessions
        .get(&runtime_session_key(&context))
        .and_then(|session| session.pending.get(&first.sender_id))
        .expect("覆盖后 pending 应保留");
    assert!(pending.started > old_started, "补救窗口应从新消息重新起算");
    assert_eq!(pending.targets.len(), 2);
    assert_eq!(pending.reactions, vec![("message-2".to_string(), "289".to_string())]);
}

#[tokio::test]
async fn direct_trigger_registers_a_committed_pending_for_correction() {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let (_temp, context) = test_context(Arc::new(ReactionAdapter {
        reactions: recorded.clone(),
    }));
    let plugin = RealContextPlugin::new();
    let settings = RealContextPluginSettings {
        takeover_direct_trigger_enable: false,
        ..RealContextPluginSettings::default()
    };
    let event = inbound_event();
    let mut decision = TriggerDecision {
        should_reply: true,
        content: event.text.clone(),
        response_target: None,
    };
    plugin
        .decide_group_trigger(&context, &event, &mut decision, &settings)
        .await
        .unwrap();
    assert!(decision.should_reply);
    let runtime = plugin.runtime.lock().unwrap();
    let pending = runtime
        .sessions
        .get(&runtime_session_key(&context))
        .and_then(|session| session.pending.get(&event.sender_id))
        .expect("直触发应登记可被补救的 pending");
    assert!(pending.committed);
    assert_eq!(pending.reactions, vec![("message-1".to_string(), "289".to_string())]);
}

#[tokio::test]
async fn muted_bot_suppresses_direct_group_trigger_while_unknown_fails_open() {
    let plugin = RealContextPlugin::new();
    // The availability check this test is about lives on the path taken
    // when active judgement is *not* running. `takeover_direct_trigger_enable`
    // defaults to true, which sends a direct trigger through the full
    // judgement flow instead — so with plain defaults the branch below is
    // never reached and the assertions pass or fail for unrelated reasons.
    let settings = RealContextPluginSettings {
        takeover_direct_trigger_enable: false,
        ..RealContextPluginSettings::default()
    };
    let event = inbound_event();
    let (_temp, muted_context) = availability_context(BotSendAvailability::Muted);
    let mut muted = TriggerDecision {
        should_reply: true,
        content: event.text.clone(),
        response_target: None,
    };
    plugin
        .decide_group_trigger(&muted_context, &event, &mut muted, &settings)
        .await
        .unwrap();
    assert!(!muted.should_reply);

    let probabilistic_settings = RealContextPluginSettings {
        active_judge_probability: 1.0,
        ..RealContextPluginSettings::default()
    };
    let mut probabilistic = TriggerDecision {
        should_reply: false,
        content: event.text.clone(),
        response_target: None,
    };
    plugin
        .decide_group_trigger(
            &muted_context,
            &event,
            &mut probabilistic,
            &probabilistic_settings,
        )
        .await
        .unwrap();
    assert!(!probabilistic.should_reply);

    let (_temp, unknown_context) = availability_context(BotSendAvailability::Unknown);
    let mut unknown = TriggerDecision {
        should_reply: true,
        content: event.text.clone(),
        response_target: None,
    };
    plugin
        .decide_group_trigger(&unknown_context, &event, &mut unknown, &settings)
        .await
        .unwrap();
    assert!(unknown.should_reply);
}

#[tokio::test]
async fn supersede_signal_wakes_an_inflight_judgement() {
    let (sender, mut receiver) = tokio::sync::watch::channel(false);
    let waiter = tokio::spawn(async move {
        wait_for_supersede(&mut receiver).await;
    });
    sender.send_replace(true);
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn directly_triggered_image_is_a_primary_target() {
    let (_temp, context) = availability_context(BotSendAvailability::Available);
    let mut current = inbound_event();
    current.text.clear();
    current.media.push(crate::platforms::PlatformInboundMedia {
        kind: PlatformMediaKind::Image,
        id: Some("image-1".to_string()),
        name: None,
        url: None,
    });

    let prompt = active_target_prompt(&context, &current, "（对方发送了 1 张图片）");

    assert!(prompt.starts_with("[New messages received this turn]\n（对方发送了 1 张图片）"));
    assert!(!prompt.contains("无明确文字目标消息"));
    assert!(!prompt.contains("同一用户随后发送的补充材料"));
}
