//! 好友申请与入群审核。

use crate::platforms::onebot::*;
use crate::paths::MiyuPaths;
use super::shared::*;

#[test]
fn friend_request_access_uses_admins_private_whitelist_and_dynamic_grants() {
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
            "3",
        ),
        (
            crate::platforms::access_control::AccessPermission::PrivateWhitelist,
            "4",
        ),
    ] {
        state
            .add_platform_access_grant(
                &crate::platforms::access_control::global_grant_key(permission, target_id),
                &actor,
            )
            .unwrap();
    }
    let mut config = OneBotConfig::default();
    config.admin_users.push(1);
    config.private_chats.whitelist.push(2);

    assert!(friend_request_allowed(&config, &state, 999, 1));
    assert!(friend_request_allowed(&config, &state, 999, 2));
    assert!(friend_request_allowed(&config, &state, 100, 3));
    assert!(friend_request_allowed(&config, &state, 100, 4));
    assert!(!friend_request_allowed(&config, &state, 100, 5));

    config
        .private_chats
        .friend_requests_require_private_whitelist = false;
    assert!(friend_request_allowed(&config, &state, 100, 5));
}

#[tokio::test]
async fn friend_request_handler_accepts_allowed_requests_and_leaves_others_pending() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    {
        let mut manager = state.manager.lock().unwrap();
        manager.config.platforms.qq.enabled = true;
        manager.config.platforms.qq.private_chats.whitelist.push(42);
    }
    let (handle, mut frames) = test_connection(None);

    let task = tokio::spawn(handle_friend_add_request(
        state.clone(),
        handle.clone(),
        friend_request_event(42, "flag-42"),
    ));
    let request: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(request["action"], "set_friend_add_request");
    assert_eq!(request["params"]["flag"], "flag-42");
    assert_eq!(request["params"]["approve"], true);
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": null,
            "echo": request["echo"],
        }),
    );
    task.await.unwrap();
    assert!(frames.try_recv().is_err());

    handle_friend_add_request(
        state.clone(),
        handle.clone(),
        friend_request_event(43, "flag-43"),
    )
    .await;
    assert!(frames.try_recv().is_err());

    state
        .manager
        .lock()
        .unwrap()
        .config
        .platforms
        .qq
        .private_chats
        .friend_requests_require_private_whitelist = false;
    let task = tokio::spawn(handle_friend_add_request(
        state,
        handle.clone(),
        friend_request_event(44, "flag-44"),
    ));
    let request: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(request["action"], "set_friend_add_request");
    assert_eq!(request["params"]["flag"], "flag-44");
    assert_eq!(request["params"]["approve"], true);
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": null,
            "echo": request["echo"],
        }),
    );
    task.await.unwrap();
    assert!(frames.try_recv().is_err());
}

#[test]
fn group_add_request_detection_and_parsing() {
    let event = group_add_request_event(130515298, 42, "flag-add");
    assert!(is_group_add_request(&event));
    assert!(!is_group_invite_request(&event));
    let request = parse_group_add_request(&event).unwrap();
    assert_eq!(request.group_id, 130515298);
    assert_eq!(request.user_id, 42);
    assert_eq!(request.flag, "flag-add");
    assert_eq!(request.comment, "申请加入");
    assert!(!group_join_request_is_filtered("flag-add"));
    assert!(!group_join_request_is_filtered("slreq:1:123:130515298:2:0"));
    assert!(group_join_request_is_filtered("slreq:1:123:130515298:2:1"));
    assert_eq!(
        group_add_request_action_flag("slreq:1:123:130515298:1:0"),
        "slreq:1:123:130515298:1:0"
    );
    assert_eq!(
        group_add_request_action_flag("slreq:1:123:130515298:2:0"),
        "slreq:1:123:130515298:1:0"
    );
    assert_eq!(
        group_add_request_action_flag("slreq:1:123:130515298:2:1"),
        "slreq:1:123:130515298:1:1"
    );
    assert_eq!(group_add_request_action_flag("flag-add"), "flag-add");

    let invite = json!({
        "post_type": "request",
        "request_type": "group",
        "sub_type": "invite",
        "self_id": 10000,
        "group_id": 130515298,
        "user_id": 42,
        "flag": "invite-flag",
    });
    assert!(!is_group_add_request(&invite));
    assert!(is_group_invite_request(&invite));

    assert!(parse_group_add_request(&json!({
        "post_type": "request",
        "request_type": "group",
        "sub_type": "add",
        "self_id": 0,
        "group_id": 130515298,
        "user_id": 42,
        "flag": "flag",
    }))
    .is_none());
    assert!(parse_group_add_request(&json!({
        "post_type": "request",
        "request_type": "group",
        "sub_type": "add",
        "self_id": 10000,
        "group_id": 130515298,
        "user_id": 42,
        "flag": " ",
    }))
    .is_none());
}

#[test]
fn group_join_decision_parser_handles_plain_and_fenced_json() {
    let (decision, reason) = parse_group_join_decision(
        "```json\n{\"decision\":\"approve\",\"reason\":\"符合通过条件\"}\n```",
    )
    .unwrap();
    assert_eq!(decision, GroupJoinDecision::Approve);
    assert_eq!(reason, "符合通过条件");

    let (decision, reason) = parse_group_join_decision(
        "前缀 {\"decision\":\"reject\",\"reason\":\"理由\\n换行\"} 后缀",
    )
    .unwrap();
    assert_eq!(decision, GroupJoinDecision::Reject);
    assert_eq!(reason, "理由换行");

    let (decision, reason) =
        parse_group_join_decision("{\"decision\":\"pending\",\"reason\":\"信息不足\"}")
            .unwrap();
    assert_eq!(decision, GroupJoinDecision::Pending);
    assert_eq!(reason, "信息不足");

    assert!(parse_group_join_decision("{\"decision\":\"maybe\"}").is_err());
    assert!(parse_group_join_decision("not json").is_err());

    let long_reason = "想".repeat(120);
    let (_, reason) = parse_group_join_decision(&format!(
        "{{\"decision\":\"reject\",\"reason\":\"{long_reason}\"}}"
    ))
    .unwrap();
    assert_eq!(reason.chars().count(), GROUP_JOIN_APPROVAL_MAX_REASON_CHARS);
}

#[tokio::test]
async fn group_add_request_handler_approves_rejects_and_pends() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 0);
    {
        let mut manager = state.manager.lock().unwrap();
        manager.config.platforms.qq.enabled = true;
        manager.config.platforms.qq.plugins.insert(
            QQ_GROUP_JOIN_APPROVAL_PLUGIN_ID.to_string(),
            crate::config::PlatformPluginInstanceConfig {
                enabled: None,
                settings: serde_json::json!({
                    "groups": [{
                        "group_id": 130515298,
                        "approve_condition": "通过条件：与 Arch Linux 相关"
                    }]
                })
                .as_object()
                .unwrap()
                .clone(),
            },
        );
    }
    let (handle, mut frames) = test_connection(None);

    let approve_review = |_config: AppConfig,
                          _paths: MiyuPaths,
                          _settings: QqGroupJoinApprovalPluginSettings,
                          _condition: String,
                          _request: GroupJoinRequest,
                          _state: StateStore| async move {
        Ok((GroupJoinDecision::Approve, String::new()))
    };
    let task = tokio::spawn(handle_group_add_request_with_llm(
        state.clone(),
        handle.clone(),
        group_add_request_event(130515298, 42, "flag-add-1"),
        approve_review,
    ));
    let request: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(request["action"], "set_group_add_request");
    assert_eq!(request["params"]["flag"], "flag-add-1");
    assert_eq!(request["params"]["sub_type"], "add");
    assert_eq!(request["params"]["approve"], true);
    assert!(request["params"].get("reason").is_none());
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": null,
            "echo": request["echo"],
        }),
    );
    task.await.unwrap();
    assert!(frames.try_recv().is_err());

    let reject_review = |_config: AppConfig,
                         _paths: MiyuPaths,
                         _settings: QqGroupJoinApprovalPluginSettings,
                         _condition: String,
                         _request: GroupJoinRequest,
                         _state: StateStore| async move {
        Ok((GroupJoinDecision::Reject, "理由不符".to_string()))
    };
    let task = tokio::spawn(handle_group_add_request_with_llm(
        state.clone(),
        handle.clone(),
        group_add_request_event(130515298, 43, "flag-add-2"),
        reject_review,
    ));
    let request: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(request["params"]["approve"], false);
    assert_eq!(request["params"]["reason"], "理由不符");
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": null,
            "echo": request["echo"],
        }),
    );
    task.await.unwrap();
    assert!(frames.try_recv().is_err());

    let pending_review = |_config: AppConfig,
                          _paths: MiyuPaths,
                          _settings: QqGroupJoinApprovalPluginSettings,
                          _condition: String,
                          _request: GroupJoinRequest,
                          _state: StateStore| async move {
        Ok((GroupJoinDecision::Pending, String::new()))
    };
    handle_group_add_request_with_llm(
        state,
        handle.clone(),
        group_add_request_event(130515298, 44, "flag-add-3"),
        pending_review,
    )
    .await;
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn group_add_request_handler_leaves_unknown_or_disabled_pending() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 0);
    state.manager.lock().unwrap().config.platforms.qq.enabled = true;
    let (handle, mut frames) = test_connection(None);
    let review = |_config: AppConfig,
                  _paths: MiyuPaths,
                  _settings: QqGroupJoinApprovalPluginSettings,
                  _condition: String,
                  _request: GroupJoinRequest,
                  _state: StateStore| async move {
        Ok((GroupJoinDecision::Approve, String::new()))
    };

    // No plugin settings at all.
    handle_group_add_request_with_llm(
        state.clone(),
        handle.clone(),
        group_add_request_event(130515298, 42, "flag-none"),
        review,
    )
    .await;
    assert!(frames.try_recv().is_err());

    // Disabled plugin.
    state
        .manager
        .lock()
        .unwrap()
        .config
        .platforms
        .qq
        .plugins
        .insert(
            QQ_GROUP_JOIN_APPROVAL_PLUGIN_ID.to_string(),
            crate::config::PlatformPluginInstanceConfig {
                enabled: Some(false),
                settings: serde_json::json!({
                    "groups": [{
                        "group_id": 130515298,
                        "approve_condition": "符合条件通过"
                    }]
                })
                .as_object()
                .unwrap()
                .clone(),
            },
        );
    handle_group_add_request_with_llm(
        state.clone(),
        handle.clone(),
        group_add_request_event(130515298, 42, "flag-disabled"),
        review,
    )
    .await;
    assert!(frames.try_recv().is_err());

    // AI review error is fail-closed.
    let failing =
        |_config: AppConfig,
         _paths: MiyuPaths,
         _settings: QqGroupJoinApprovalPluginSettings,
         _condition: String,
         _request: GroupJoinRequest,
         _state: StateStore| async move { anyhow::bail!("model unavailable") };
    state
        .manager
        .lock()
        .unwrap()
        .config
        .platforms
        .qq
        .plugins
        .get_mut(QQ_GROUP_JOIN_APPROVAL_PLUGIN_ID)
        .unwrap()
        .enabled = None;
    handle_group_add_request_with_llm(
        state,
        handle.clone(),
        group_add_request_event(130515298, 42, "flag-error"),
        failing,
    )
    .await;
    assert!(frames.try_recv().is_err());
}
