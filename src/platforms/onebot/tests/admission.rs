//! 准入判定、并发与限流。

use crate::platforms::onebot::*;
use super::shared::*;

#[test]
fn group_trigger_matrix() {
    let at_only = OneBotConfig::default();
    let mut parsed = InboundMessage {
        text: "/cmd 查询".into(),
        ..Default::default()
    };
    assert!(group_trigger_text(&at_only, &parsed, None, 10_000).is_none());
    parsed.at_self = true;
    assert_eq!(
        group_trigger_text(&at_only, &parsed, None, 10_000).as_deref(),
        Some("/cmd 查询")
    );

    let prefix = config_with(|config| {
        config.group_chats.trigger_keywords = vec!["/cmd".into()];
    });
    parsed.at_self = false;
    assert_eq!(
        group_trigger_text(&prefix, &parsed, None, 10_000).as_deref(),
        Some("查询")
    );
    parsed.text = "无前缀".into();
    assert!(group_trigger_text(&prefix, &parsed, None, 10_000).is_none());

    // An empty keyword list never fires (avoids always-on).
    let empty_prefix = OneBotConfig::default();
    assert!(group_trigger_text(&empty_prefix, &parsed, None, 10_000).is_none());

    let either = config_with(|config| {
        config.group_chats.trigger_keywords = vec!["喵".into(), "喵喵".into()];
    });
    parsed.text = "喵喵：早上好".into();
    assert_eq!(
        group_trigger_text(&either, &parsed, None, 10_000).as_deref(),
        Some("早上好")
    );

    parsed.text = "继续说".into();
    let replied_message = PlatformMessageInfo {
        message_id: "previous".into(),
        sender_id: "10000".into(),
        sender_display_name: "Miyu".into(),
        timestamp: 1,
        text: "previous reply".into(),
        reply_to_message_id: None,
        mentioned_user_ids: Vec::new(),
        mentioned_users: Vec::new(),
        media: Vec::new(),
        conversation_kind: Some(ConversationKind::Group),
        conversation_id: Some("9".to_string()),
    };
    assert_eq!(
        group_trigger_text(&at_only, &parsed, Some(&replied_message), 10_000).as_deref(),
        Some("继续说")
    );
}

#[tokio::test]
async fn busy_model_capacity_waits_silently_without_merging_the_turn() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    {
        let mut manager = state.manager.lock().unwrap();
        manager.config.platforms.qq.enabled = true;
        manager.config.platforms.qq.group_chats.allow_non_whitelist = true;
        manager
            .config
            .platforms
            .qq
            .group_chats
            .non_whitelist_rate_limit
            .max_messages = 0;
        manager.config.platforms.qq.group_chats.trigger_keywords = vec!["miyu".to_string()];
    }
    assert!(state
        .platforms
        .plugins
        .set(Ok(Arc::new(
            crate::platforms::plugins::PlatformPluginRegistry::default()
        )))
        .is_ok());
    let all_turn_permits = state
        .platforms
        .turn_permits
        .clone()
        .acquire_many_owned(crate::platforms::MAX_CONCURRENT_PLATFORM_TURNS as u32)
        .await
        .unwrap();
    let (handle, mut frames) = test_connection(None);
    let base = json!({
        "post_type": "message",
        "message_type": "group",
        "self_id": 10000,
        "user_id": 7,
        "group_id": 42,
        "message_id": 90,
        "group_name": "test group",
        "sender": { "nickname": "seven" },
    });

    let mut silent = base.clone();
    silent["message"] = json!([{ "type": "text", "data": { "text": "ordinary" } }]);
    handle_message(state.clone(), handle.clone(), silent, next_ingress_order()).await;
    assert!(frames.try_recv().is_err());

    let mut triggered = base;
    triggered["message"] = json!([{ "type": "text", "data": { "text": "miyu hello" } }]);
    let task = tokio::spawn(handle_message(
        state,
        handle,
        triggered,
        next_ingress_order(),
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), frames.recv())
            .await
            .is_err()
    );
    assert!(!task.is_finished());
    task.abort();
    let _ = task.await;
    drop(all_turn_permits);
}

#[tokio::test]
async fn same_conversation_messages_can_be_observed_in_parallel() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    {
        let mut manager = state.manager.lock().unwrap();
        manager.config.platforms.qq.enabled = true;
        manager.config.platforms.qq.group_chats.allow_non_whitelist = true;
        manager
            .config
            .platforms
            .qq
            .group_chats
            .non_whitelist_rate_limit
            .max_messages = 0;
    }
    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let release_first = Arc::new(tokio::sync::Notify::new());
    assert!(state
        .platforms
        .plugins
        .set(Ok(Arc::new(
            crate::platforms::plugins::PlatformPluginRegistry::new(vec![Arc::new(
                BlockingObserverPlugin {
                    observed: observed_tx,
                    release_first: release_first.clone(),
                },
            )])
        )))
        .is_ok());
    let (handle, _frames) = test_connection(None);
    let event = |message_id: i64| {
        json!({
            "post_type": "message",
            "message_type": "group",
            "self_id": 10000,
            "user_id": 7,
            "group_id": 42,
            "group_name": "test group",
            "message_id": message_id,
            "message": [{ "type": "text", "data": { "text": "ordinary" } }],
            "sender": { "nickname": "seven" },
        })
    };

    let first = tokio::spawn(handle_message(
        state.clone(),
        handle.clone(),
        event(1),
        next_ingress_order(),
    ));
    assert_eq!(observed_rx.recv().await.as_deref(), Some("1"));

    let second = tokio::spawn(handle_message(
        state.clone(),
        handle,
        event(2),
        next_ingress_order(),
    ));
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(50), observed_rx.recv())
            .await
            .unwrap()
            .as_deref(),
        Some("2")
    );

    release_first.notify_one();
    first.await.unwrap();
    second.await.unwrap();
}

#[tokio::test]
async fn same_conversation_judgements_reuse_parallel_turn_admission() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    {
        let mut manager = state.manager.lock().unwrap();
        manager.config.platforms.qq.enabled = true;
        manager.config.platforms.qq.group_chats.allow_non_whitelist = true;
        manager.config.platforms.qq.session_limits = crate::config::PlatformSessionLimits {
            running: 2,
            queued: 2,
        };
        manager
            .config
            .platforms
            .qq
            .group_chats
            .non_whitelist_rate_limit
            .max_messages = 0;
    }
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    assert!(state
        .platforms
        .plugins
        .set(Ok(Arc::new(
            crate::platforms::plugins::PlatformPluginRegistry::new(vec![Arc::new(
                BlockingJudgePlugin {
                    entered: entered_tx,
                    barrier: barrier.clone(),
                },
            )])
        )))
        .is_ok());
    let (handle, _frames) = test_connection(None);
    let event = |message_id: i64, user_id: i64| {
        json!({
            "post_type": "message",
            "message_type": "group",
            "self_id": 10000,
            "user_id": user_id,
            "group_id": 42,
            "group_name": "test group",
            "message_id": message_id,
            "message": [{ "type": "text", "data": { "text": "ordinary" } }],
            "sender": { "nickname": user_id.to_string() },
        })
    };

    let first = tokio::spawn(handle_message(
        state.clone(),
        handle.clone(),
        event(1, 7),
        next_ingress_order(),
    ));
    let second = tokio::spawn(handle_message(
        state.clone(),
        handle,
        event(2, 8),
        next_ingress_order(),
    ));
    let entered = tokio::time::timeout(Duration::from_secs(1), async {
        let mut ids = vec![
            entered_rx.recv().await.unwrap(),
            entered_rx.recv().await.unwrap(),
        ];
        ids.sort();
        ids
    })
    .await
    .expect("both judgements should enter under the shared running=2 limit");
    assert_eq!(entered, ["1", "2"]);
    barrier.wait().await;
    first.await.unwrap();
    second.await.unwrap();
    assert!(state
        .platforms
        .session_turn_locks
        .lock()
        .unwrap()
        .is_empty());
}

#[test]
fn admission_matrix_uses_private_and_group_conversation_buckets() {
    let mut config = OneBotConfig::default();
    config.admin_users.push(1);
    config.private_chats.whitelist.push(2);
    config.group_chats.whitelist.push(10);

    let admin = admission_for(&config, Target::Group { group_id: 99 }, 100, 1);
    assert!(admin.allowed);
    assert!(admin.rate_key.is_none());
    assert!(admin.use_non_whitelist_text_models);

    let private_admin = admission_for(&config, Target::Private { user_id: 1 }, 100, 1);
    assert!(private_admin.allowed);
    assert!(!private_admin.use_non_whitelist_text_models);

    let private_whitelist = admission_for(&config, Target::Private { user_id: 2 }, 100, 2);
    assert!(private_whitelist.allowed);
    assert!(private_whitelist.rate_key.is_none());
    assert!(!private_whitelist.use_non_whitelist_text_models);

    let private_guest = admission_for(&config, Target::Private { user_id: 3 }, 100, 3);
    assert!(private_guest.allowed);
    assert_eq!(private_guest.rate_limit.max_messages, 2);
    assert_eq!(private_guest.rate_limit.window_seconds, 600);
    assert_eq!(private_guest.rate_key.as_deref(), Some("qq:100:private:3"));
    assert!(private_guest.use_non_whitelist_text_models);

    let group_whitelist = admission_for(&config, Target::Group { group_id: 10 }, 100, 2);
    assert!(group_whitelist.allowed);
    assert_eq!(group_whitelist.rate_limit.max_messages, 30);
    assert_eq!(group_whitelist.rate_limit.window_seconds, 60);
    assert!(group_whitelist.rate_key.is_none());
    assert!(!group_whitelist.use_non_whitelist_text_models);

    let group_guest = admission_for(&config, Target::Group { group_id: 11 }, 100, 3);
    assert!(group_guest.allowed);
    assert_eq!(group_guest.rate_limit.max_messages, 2);
    assert_eq!(group_guest.rate_limit.window_seconds, 600);
    assert_eq!(group_guest.rate_key.as_deref(), Some("qq:100:group:11"));
    assert!(group_guest.use_non_whitelist_text_models);

    let privileged_group_guest = admission_for(&config, Target::Group { group_id: 11 }, 100, 2);
    assert!(privileged_group_guest.allowed);
    assert!(privileged_group_guest.rate_key.is_none());
    assert!(privileged_group_guest.use_non_whitelist_text_models);

    config.private_chats.allow_non_whitelist = false;
    config.group_chats.allow_non_whitelist = false;
    assert!(!admission_for(&config, Target::Private { user_id: 3 }, 100, 3).allowed);
    assert!(!admission_for(&config, Target::Group { group_id: 11 }, 100, 3).allowed);
    let privileged_disallowed_group =
        admission_for(&config, Target::Group { group_id: 11 }, 100, 2);
    assert!(!privileged_disallowed_group.allowed);
    assert!(privileged_disallowed_group.rate_key.is_none());
    assert!(privileged_disallowed_group.use_non_whitelist_text_models);
}

#[test]
fn admission_materializes_the_effective_text_model_pool() {
    let mut base = crate::config::AppConfig::default();
    let provider_id = base.providers[0].id.clone();
    let pool = |model: &str| {
        vec![crate::config::ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: model.to_string(),
        }]
    };
    base.active_provider_models = Some(pool("global"));
    base.platforms.qq.text_models = Some(pool("platform"));
    base.platforms.qq.non_whitelist_text_models = Some(pool("non-whitelist"));
    base.platforms.qq.admin_users.push(1);
    base.platforms.qq.private_chats.whitelist.push(2);
    base.platforms.qq.group_chats.whitelist.push(10);

    for (target, user_id, expected) in [
        (Target::Private { user_id: 1 }, 1, "platform"),
        (Target::Private { user_id: 2 }, 2, "platform"),
        (Target::Private { user_id: 3 }, 3, "non-whitelist"),
        (Target::Group { group_id: 10 }, 3, "platform"),
        (Target::Group { group_id: 11 }, 1, "non-whitelist"),
    ] {
        let mut config = base.clone();
        let admission = admission_for(&config.platforms.qq, target, 100, user_id);
        apply_admission_text_model_pool(&mut config, target, &admission);
        assert_eq!(
            config.active_provider_models.as_ref().unwrap()[0].model,
            expected
        );
    }
}

#[test]
fn dynamic_access_grants_feed_the_same_admission_matrix_for_every_bot() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let state = StateStore::new(&paths).unwrap();
    let actor = crate::state::PlatformAccessActor {
        platform: "onebot".to_string(),
        account_id: "100".to_string(),
        user_id: "42".to_string(),
        conversation_kind: "private".to_string(),
        conversation_id: "42".to_string(),
        message_id: "message-1".to_string(),
    };
    for (permission, target_id) in [
        (
            crate::platforms::access_control::AccessPermission::Administrator,
            "1",
        ),
        (
            crate::platforms::access_control::AccessPermission::PrivateWhitelist,
            "2",
        ),
        (
            crate::platforms::access_control::AccessPermission::GroupWhitelist,
            "10",
        ),
    ] {
        state
            .add_platform_access_grant(
                &crate::platforms::access_control::global_grant_key(
                    permission,
                    target_id.to_string(),
                ),
                &actor,
            )
            .unwrap();
    }
    let mut config = OneBotConfig::default();
    config.private_chats.allow_non_whitelist = false;
    config.group_chats.allow_non_whitelist = false;

    let admin =
        admission_for_with_state(&config, &state, Target::Group { group_id: 99 }, 999, 1);
    assert!(admin.allowed);
    assert!(admin.rate_key.is_none());
    assert!(admin.use_non_whitelist_text_models);

    let private_admin =
        admission_for_with_state(&config, &state, Target::Private { user_id: 1 }, 999, 1);
    assert!(private_admin.allowed);
    assert!(!private_admin.use_non_whitelist_text_models);

    let private_whitelist =
        admission_for_with_state(&config, &state, Target::Private { user_id: 2 }, 999, 2);
    assert!(private_whitelist.allowed);
    assert!(private_whitelist.rate_key.is_none());
    assert!(!private_whitelist.use_non_whitelist_text_models);

    let group_whitelist =
        admission_for_with_state(&config, &state, Target::Group { group_id: 10 }, 999, 3);
    assert!(group_whitelist.allowed);
    assert_eq!(
        group_whitelist.rate_limit,
        config.group_chats.whitelist_rate_limit
    );
    assert_eq!(group_whitelist.rate_key.as_deref(), Some("qq:999:group:10"));
    assert!(!group_whitelist.use_non_whitelist_text_models);
}

#[tokio::test]
async fn tool_followup_reservation_requires_the_same_conversation_and_sender() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 0);
    let config = state.manager.lock().unwrap().config.clone();
    let target = Target::Group { group_id: 99 };
    let event = json!({
        "self_id": 10000,
        "user_id": 42,
        "message_type": "group",
        "group_id": 99,
        "sender": { "nickname": "Alice" }
    });
    let (connection, _frames) = test_connection(None);
    let context = Arc::new(
        platform_turn_context(&state, connection, target, &event, config, None).unwrap(),
    );
    let followup = PlatformFollowupRun::new(context);
    followup.ingress().tool_started("call_1");
    let session_id: Arc<str> = "qq-session".into();
    let (cancel, _cancel_rx) = watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        "run_1".to_string(),
        crate::runtime::RunInfo {
            session_id: session_id.clone(),
            mode: crate::agent::AgentMode::Normal,
            audience: crate::config::PromptAudience::External,
            cancel,
            turn_id: Some("turn_1".to_string()),
            queue_target: None,
            supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
            platform_followup: Some(followup.clone()),
            operation: crate::runtime::RunOperation::Create,
            job_wake: false,
            turn_origin: crate::tools::workspace::TurnOrigin::Human,
            job_wake_label: None,
        },
    );

    assert!(
        reserve_tool_followup(&state, &session_id, &followup.conversation, "other-sender")
            .is_none()
    );
    let mut other_conversation = followup.conversation.clone();
    other_conversation.conversation_id = "100".to_string();
    assert!(reserve_tool_followup(&state, &session_id, &other_conversation, "42").is_none());
    assert!(reserve_tool_followup(&state, &session_id, &followup.conversation, "42").is_some());

    std::thread::sleep(Duration::from_millis(1));
    let newer = PlatformFollowupRun::new(followup.context.clone());
    newer.ingress().tool_started("call_2");
    let (newer_cancel, _newer_cancel_rx) = watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        "run_2".to_string(),
        crate::runtime::RunInfo {
            session_id: session_id.clone(),
            mode: crate::agent::AgentMode::Normal,
            audience: crate::config::PromptAudience::External,
            cancel: newer_cancel,
            turn_id: Some("turn_2".to_string()),
            queue_target: None,
            supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
            platform_followup: Some(newer.clone()),
            operation: crate::runtime::RunOperation::Create,
            job_wake: false,
            turn_origin: crate::tools::workspace::TurnOrigin::Human,
            job_wake_label: None,
        },
    );
    assert_eq!(
        platform_update_target(&state, &session_id, &followup.conversation, "42")
            .unwrap()
            .0,
        "run_2"
    );

    followup.ingress().tool_finished("call_1");
    newer.ingress().tool_finished("call_2");
    assert!(reserve_tool_followup(&state, &session_id, &followup.conversation, "42").is_none());
}

#[test]
fn rate_limit_notices_are_silent_in_private_chats_only() {
    assert!(!sends_rate_limit_notice(Target::Private { user_id: 7 }));
    assert!(sends_rate_limit_notice(Target::Group { group_id: 42 }));
}

#[test]
fn ingress_order_is_strictly_monotonic() {
    let first = next_ingress_order();
    let second = next_ingress_order();
    assert!(second > first);
}
