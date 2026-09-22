//! 三条中转线(agy / claude-code / codex)的去重名单都得是真工具。
//!
//! 名单是纯字符串常量,工具改名不会让它报错——`read_file` / `apply_patch` 就是这么
//! 在 08-21 三域合并里变成死条目的。断言要拿**注册表**去核,而注册表是工具层的东西;
//! 测试跟着被断言的那一侧走,所以住这儿(09-16 从 `llm::openai_compatible::*` 搬来:
//! 中转线层不该反过来 use 工具层)。名单常量由 `llm` 按线别名再导出。

mod antigravity {
    use miyu_core::llm::ANTIGRAVITY_BRIDGE_DUPLICATE_TOOLS as BRIDGE_DUPLICATE_TOOLS;

    /// 去重名单里的每个名字都必须真的是一件已注册的 Miyu 工具(改名会让
    /// 纯字符串名单静默失效,claude 线的 read_file/apply_patch 教训)。
    #[test]
    fn every_deduplicated_name_is_a_real_tool() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let paths = miyu_base::paths::MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("config/fish/conf.d/miyu.fish"),
            bash_hook_file: root.join("config/shell/bash-hook.sh"),
            zsh_hook_file: root.join("config/shell/zsh-hook.zsh"),
            scripts_dir: root.join("config/scripts"),
            system_scripts_dir: root.join("data/scripts"),
        };
        let mut config = miyu_base::config::AppConfig::default();
        config.plugins.web.enabled = true;
        config.skills.allow_command_execution = true;
        let registry = crate::tools::builtin_registry(&config, &paths);
        for name in BRIDGE_DUPLICATE_TOOLS {
            assert!(
                registry.contains(name),
                "去重名单里的 {name} 不是任何已注册工具——多半是工具改名后忘了跟着改"
            );
        }
    }
}

mod claude_code {
    use miyu_core::llm::CLAUDE_CODE_BRIDGE_DUPLICATE_TOOLS as BRIDGE_DUPLICATE_TOOLS;

    /// 去重名单里的每个名字都必须真的是一件已注册工具。名单是纯字符串,
    /// 改名不会让它报错——`read_file` / `apply_patch` 就是这么在 08-21 三域
    /// 合并里变成死条目、白挂了两件重复工具上桥的(09-01 转录取证)。
    #[test]
    fn every_deduplicated_name_is_a_real_tool() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let paths = miyu_base::paths::MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("config/fish/conf.d/miyu.fish"),
            bash_hook_file: root.join("config/shell/bash-hook.sh"),
            zsh_hook_file: root.join("config/shell/zsh-hook.zsh"),
            scripts_dir: root.join("config/scripts"),
            system_scripts_dir: root.join("data/scripts"),
        };
        let mut config = miyu_base::config::AppConfig::default();
        config.plugins.web.enabled = true;
        config.skills.allow_command_execution = true;
        let registry = crate::tools::builtin_registry(&config, &paths);
        for name in BRIDGE_DUPLICATE_TOOLS {
            assert!(
                registry.contains(name),
                "去重名单里的 {name} 不是任何已注册工具——多半是工具改名后忘了跟着改"
            );
        }
    }
}

mod codex {
    use miyu_core::llm::CODEX_BRIDGE_DUPLICATE_TOOLS as BRIDGE_DUPLICATE_TOOLS;

    #[test]
    fn every_deduplicated_name_is_a_real_tool() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let paths = miyu_base::paths::MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("config/fish/conf.d/miyu.fish"),
            bash_hook_file: root.join("config/shell/bash-hook.sh"),
            zsh_hook_file: root.join("config/shell/zsh-hook.zsh"),
            scripts_dir: root.join("config/scripts"),
            system_scripts_dir: root.join("data/scripts"),
        };
        let mut config = miyu_base::config::AppConfig::default();
        config.plugins.web.enabled = true;
        config.skills.allow_command_execution = true;
        let registry = crate::tools::builtin_registry(&config, &paths);
        for name in BRIDGE_DUPLICATE_TOOLS {
            assert!(
                registry.contains(name),
                "去重名单里的 {name} 不是任何已注册工具"
            );
        }
    }
}
