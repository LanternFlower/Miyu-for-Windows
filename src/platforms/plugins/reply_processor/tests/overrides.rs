//! 会话级覆盖与管理员命令。

use crate::platforms::plugins::reply_processor::*;
use super::shared::*;

#[test]
fn session_override_round_trip_shape_is_stable() {
    let overrides = SessionOverrides {
        enabled: Some(true),
        threshold: Some(500),
        mode: Some(ReplyMode::Forward),
    };
    let json = serde_json::to_value(&overrides).unwrap();
    assert_eq!(json["mode"], "forward");
    assert_eq!(
        serde_json::from_value::<SessionOverrides>(json)
            .unwrap()
            .threshold,
        Some(500)
    );
}

#[test]
fn admin_commands_update_and_restore_only_the_current_scope() {
    let (_temp, context) = test_context(true);
    ReplyProcessorPlugin::handle_admin_command(&context, "阈值 500").unwrap();
    let settings = ReplyProcessorPlugin::effective_settings(&context).unwrap();
    assert!(settings.enabled);
    assert_eq!(settings.threshold, 500);
    assert!(settings.custom);

    ReplyProcessorPlugin::handle_admin_command(&context, "模式 转发").unwrap();
    assert_eq!(
        ReplyProcessorPlugin::effective_settings(&context)
            .unwrap()
            .mode,
        ReplyMode::Forward
    );
    ReplyProcessorPlugin::handle_admin_command(&context, "阈值 关").unwrap();
    assert!(
        !ReplyProcessorPlugin::effective_settings(&context)
            .unwrap()
            .enabled
    );

    ReplyProcessorPlugin::handle_admin_command(&context, "恢复默认").unwrap();
    let defaults = ReplyProcessorPlugin::effective_settings(&context).unwrap();
    assert!(defaults.enabled);
    assert_eq!(defaults.threshold, 300);
    assert_eq!(defaults.mode, ReplyMode::Image);
    assert!(!defaults.custom);
}

#[test]
fn non_admin_command_does_not_create_an_override() {
    let (_temp, context) = test_context(false);
    ReplyProcessorPlugin::handle_admin_command(&context, "阈值 1").unwrap();
    assert!(ReplyProcessorPlugin::overrides(&context).unwrap().is_none());
}
