//! 回复处理测试共用的 fixture。

use crate::platforms::plugins::reply_processor::*;
use crate::config::AppConfig;
use crate::paths::MiyuPaths;
use crate::platforms::{ConversationKind, PlatformAdapter, PlatformConversation};
use crate::state::StateStore;
use futures_util::future::BoxFuture;
use std::path::PathBuf;

pub(super) fn test_context(is_admin: bool) -> (tempfile::TempDir, PlatformTurnContext) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let paths = MiyuPaths {
        root_dir: root.to_path_buf(),
        config_dir: root.join("config"),
        config_file: root.join("config/config.jsonc"),
        skills_dir: root.join("config/skills"),
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        state_dir: root.join("state"),
        pictures_dir: root.join("pictures"),
        fish_hook_file: root.join("fish/miyu.fish"),
        bash_hook_file: root.join("shell/bash-hook.sh"),
        zsh_hook_file: root.join("shell/zsh-hook.zsh"),
        scripts_dir: root.join("config/scripts"),
        system_scripts_dir: PathBuf::new(),
    };
    let context = PlatformTurnContext::new(
        PlatformConversation {
            platform: "onebot".to_string(),
            account_id: "10000".to_string(),
            kind: ConversationKind::Group,
            conversation_id: "20000".to_string(),
        },
        "30000".to_string(),
        "tester".to_string(),
        is_admin,
        AppConfig::default(),
        paths.clone(),
        StateStore::new(&paths).unwrap(),
        Arc::new(NoopAdapter),
        Arc::new(crate::platforms::plugins::PlatformPluginRegistry::default()),
    );
    (temp, context)
}

pub(super) fn set_plugin_setting(context: &mut PlatformTurnContext, key: &str, value: Value) {
    context
        .config
        .platforms
        .qq
        .plugins
        .entry(PLUGIN_ID.to_string())
        .or_default()
        .settings
        .insert(key.to_string(), value);
}

pub(super) struct NoopAdapter;

impl PlatformAdapter for NoopAdapter {
    fn send<'a>(&'a self, _message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
        Box::pin(async { Ok(SendReceipt::default()) })
    }

    fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok("Miyu".to_string()) })
    }
}
