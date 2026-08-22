//! 记忆测试共用的 fixture。

use crate::memory::*;
use crate::config::AppConfig;
use crate::paths::MiyuPaths;

pub(super) fn test_paths(temp: &tempfile::TempDir) -> MiyuPaths {
    MiyuPaths {
        root_dir: temp.path().to_path_buf(),
        config_dir: temp.path().join("config"),
        config_file: temp.path().join("config/config.jsonc"),
        skills_dir: temp.path().join("config/skills"),
        data_dir: temp.path().join("data"),
        cache_dir: temp.path().join("cache"),
        state_dir: temp.path().join("state"),
        pictures_dir: temp.path().join("pictures"),
        fish_hook_file: temp.path().join("fish/miyu.fish"),
        bash_hook_file: temp.path().join("shell/bash-hook.sh"),
        zsh_hook_file: temp.path().join("shell/zsh-hook.zsh"),
        scripts_dir: temp.path().join("config/scripts"),
        system_scripts_dir: PathBuf::new(),
    }
}

pub(super) fn diary_config(batch_size: usize) -> AppConfig {
    let mut config = AppConfig::default();
    config.plugins.memory.diary_batch_size = batch_size;
    config
}

pub(super) fn test_origin() -> MemoryOrigin {
    MemoryOrigin::local("test-session")
}

pub(super) fn platform_origin(user_id: &str, display_name: &str) -> MemoryOrigin {
    MemoryOrigin {
        kind: "platform".to_string(),
        platform: "onebot".to_string(),
        account_id: "10000".to_string(),
        conversation_kind: "private".to_string(),
        conversation_id: user_id.to_string(),
        sender_id: user_id.to_string(),
        sender_display_name: display_name.to_string(),
        session_id: format!("session-{user_id}"),
        message_id: format!("message-{user_id}"),
    }
}

pub(super) fn scoped_store(
    config: &AppConfig,
    paths: &MiyuPaths,
    origin: &MemoryOrigin,
    privileged: bool,
) -> MemoryStore {
    let ownership = origin.principal_ownership().unwrap();
    MemoryStore::new(config, paths).with_request_context(
        if privileged {
            MemoryAccess::Privileged
        } else {
            MemoryAccess::principal(ownership.owner_principal.clone())
        },
        Some(ownership.owner_principal),
        ownership.owner_display_name,
    )
}

pub(super) fn record_turn(store: &MemoryStore, user: &str, assistant: &str) -> bool {
    let (database_id, generation) = store.identity().unwrap();
    store
        .process_after_turn(user, assistant, &test_origin(), &database_id, generation)
        .unwrap()
}
