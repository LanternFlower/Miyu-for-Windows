//! 同发送者的新承诺不能使尚未入场的旧回合继续执行或清掉新承诺。

use super::shared::*;
use crate::platforms::plugins::real_context::*;

fn next_context(previous: &PlatformTurnContext) -> PlatformTurnContext {
    let mut event = previous.inbound_event().unwrap().clone();
    event.message_id = "message-2".to_string();
    event.text = "correction".to_string();
    event.ingress_order = Some(2);
    PlatformTurnContext::new(
        previous.conversation.clone(),
        previous.sender_id.clone(),
        previous.sender_display_name.clone(),
        previous.is_admin,
        previous.config.clone(),
        previous.paths.clone(),
        previous.state_store.clone(),
        previous.adapter.clone(),
        previous.plugins.clone(),
    )
    .with_inbound_event(event)
}

fn direct_settings() -> RealContextPluginSettings {
    RealContextPluginSettings {
        takeover_direct_trigger_enable: false,
        active_reply_reaction_enable: false,
        ..RealContextPluginSettings::default()
    }
}

async fn admit_direct(
    plugin: &RealContextPlugin,
    context: &PlatformTurnContext,
    settings: &RealContextPluginSettings,
) {
    let event = context.inbound_event().unwrap();
    let mut decision = TriggerDecision {
        should_reply: true,
        content: event.text.clone(),
        response_target: None,
    };
    plugin
        .decide_group_trigger(context, event, &mut decision, settings)
        .await
        .unwrap();
    assert!(decision.should_reply);
}

fn pending_generation(plugin: &RealContextPlugin, context: &PlatformTurnContext) -> Option<u64> {
    plugin
        .runtime
        .lock()
        .unwrap()
        .sessions
        .get(&runtime_session_key(context))
        .and_then(|session| session.pending.get(&context.sender_id))
        .map(|pending| pending.generation)
}

#[tokio::test]
async fn correction_invalidates_the_old_context_before_a_run_exists() {
    let (_temp, first) = availability_context(BotSendAvailability::Available);
    let correction = next_context(&first);
    let plugin = RealContextPlugin::new();
    let settings = direct_settings();
    admit_direct(&plugin, &first, &settings).await;
    let first_generation = pending_generation(&plugin, &first).unwrap();
    admit_direct(&plugin, &correction, &settings).await;
    assert_ne!(
        pending_generation(&plugin, &correction),
        Some(first_generation)
    );
    assert!(
        plugin.turn_is_superseded(&first),
        "an older queued or image-loading context must not start another generation"
    );
    assert!(!plugin.turn_is_superseded(&correction));
}

#[tokio::test]
async fn aborted_old_context_preserves_the_replacement_pending() {
    let (_temp, first) = availability_context(BotSendAvailability::Available);
    let correction = next_context(&first);
    let plugin = RealContextPlugin::new();
    let settings = direct_settings();
    admit_direct(&plugin, &first, &settings).await;
    admit_direct(&plugin, &correction, &settings).await;
    let replacement = pending_generation(&plugin, &correction).unwrap();

    plugin.after_turn_aborted(&first).await.unwrap();

    assert_eq!(pending_generation(&plugin, &correction), Some(replacement));
    assert!(!plugin.turn_is_superseded(&correction));
}

#[tokio::test]
async fn independent_reply_outside_the_window_keeps_both_contexts_valid() {
    let (_temp, first) = availability_context(BotSendAvailability::Available);
    let next = next_context(&first);
    let plugin = RealContextPlugin::new();
    let settings = direct_settings();
    admit_direct(&plugin, &first, &settings).await;
    plugin
        .runtime
        .lock()
        .unwrap()
        .sessions
        .get_mut(&runtime_session_key(&first))
        .unwrap()
        .pending
        .get_mut(&first.sender_id)
        .unwrap()
        .started =
        Instant::now() - Duration::from_secs(settings.active_reply_supersede_window_seconds + 1);
    admit_direct(&plugin, &next, &settings).await;

    assert!(!plugin.turn_is_superseded(&first));
    assert!(!plugin.turn_is_superseded(&next));

    let generation = pending_generation(&plugin, &next).unwrap();
    plugin.after_turn_aborted(&first).await.unwrap();
    assert_eq!(pending_generation(&plugin, &next), Some(generation));
}

#[tokio::test]
async fn a_superseded_context_stays_invalid_after_the_new_reply_lands() {
    let (_temp, first) = availability_context(BotSendAvailability::Available);
    let correction = next_context(&first);
    let plugin = RealContextPlugin::new();
    let settings = direct_settings();
    admit_direct(&plugin, &first, &settings).await;
    admit_direct(&plugin, &correction, &settings).await;

    plugin
        .finish_reply(
            &correction,
            &OutboundMessage::text(OutboundOrigin::FinalReply, "new answer"),
            &settings,
        )
        .await;

    assert!(pending_generation(&plugin, &correction).is_none());
    assert!(plugin.turn_is_superseded(&first));
    assert!(!plugin.turn_is_superseded(&correction));
}

#[tokio::test]
async fn an_older_delivery_does_not_consume_the_new_pending() {
    let (_temp, first) = availability_context(BotSendAvailability::Available);
    let correction = next_context(&first);
    let plugin = RealContextPlugin::new();
    let settings = direct_settings();
    admit_direct(&plugin, &first, &settings).await;
    admit_direct(&plugin, &correction, &settings).await;
    let replacement = pending_generation(&plugin, &correction).unwrap();

    plugin
        .finish_reply(
            &first,
            &OutboundMessage::text(OutboundOrigin::FinalReply, "older answer"),
            &settings,
        )
        .await;

    assert_eq!(pending_generation(&plugin, &correction), Some(replacement));
}

#[tokio::test]
async fn active_generation_handoff_preserves_the_running_context() {
    let (_temp, first) = availability_context(BotSendAvailability::Available);
    let correction = next_context(&first);
    let plugin = RealContextPlugin::new();
    let settings = direct_settings();
    admit_direct(&plugin, &first, &settings).await;
    let generation = pending_generation(&plugin, &first);

    assert!(plugin
        .preempt_inbound(&correction, correction.inbound_event().unwrap())
        .unwrap());
    plugin
        .confirm_supersede(&correction, correction.inbound_event().unwrap())
        .await;

    assert_eq!(pending_generation(&plugin, &first), generation);
    assert!(!plugin.turn_is_superseded(&first));
}
