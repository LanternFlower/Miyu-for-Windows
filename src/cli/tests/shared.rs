//! 几个测试文件共用的构造辅助。
use crate::cli::*;

pub(super) fn sample_pop_turn(status: TurnStatus) -> Turn {
    Turn {
        turn_id: "turn-1".to_string(),
        seq: 1,
        user_content: "first prompt line\nsecond prompt line".to_string(),
        display_content: "first prompt line\nsecond prompt line".to_string(),
        user_timestamp: "2026-07-19 10:42".to_string(),
        assistant_content: "first answer line\nsecond answer line".to_string(),
        assistant_reasoning: Some("private reasoning".to_string()),
        assistant_provider_id: None,
        assistant_model: None,
        assistant_timestamp: Some("2026-07-19 10:43".to_string()),
        status,
        tool_reports: vec!["hidden tool report".to_string()],
        tool_flow: Vec::new(),
        question_exchanges: Vec::new(),
        followups: Vec::new(),
        attachments: Vec::new(),
        hidden: false,
        is_summary: false,
        owner_pid: None,
        token_total: 0,
        token_prompt: 0,
        token_cache_read: 0,
        token_usage_estimated: false,
        revision: 0,
        journal_events: Vec::new(),
        context_messages: Vec::new(),
    }
}

pub(super) fn pop_test_paths(root: &std::path::Path) -> MiyuPaths {
    MiyuPaths {
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
        system_scripts_dir: root.join("system/scripts"),
    }
}
