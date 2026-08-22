//! clap 帮助文本的中文化。
//!
//! clap 本身不支持多语言，所以这里在构建好的 `Command` 上逐个子命令替换描述与
//! 模板。做法笨但可控：不用维护一份平行的命令定义，加新参数时忘了翻译也只是
//! 显示英文，不会不一致到出错。

use crate::cli::*;

pub(in crate::cli) fn localized_command() -> clap::Command {
    let mut command = Cli::command();
    command = command
        .about(t("Miyu AI assistant", "Miyu AI 助手"))
        .override_usage(t(
            "miyu [OPTIONS] [MESSAGE]... [COMMAND]",
            "miyu [选项] [消息]... [命令]",
        ));
    if is_zh() {
        command = command
            .subcommand_help_heading("命令")
            .arg_required_else_help(false)
            .next_help_heading("选项")
            .help_template("{about}\n\n用法: {usage}\n\n命令:\n{subcommands}\n参数:\n{positionals}\n选项:\n{options}\n{after-help}")
            .after_help("提示：不带参数进入 REPL；直接输入消息会发送一次对话。可在配置界面设置语言，MIYU_LANG 可临时覆盖。")
            .disable_help_subcommand(true);
    } else {
        command = command
            .after_help(
                "Tip: run without arguments to enter the REPL; pass MESSAGE to send one chat turn. Set the language in the configuration UI; MIYU_LANG is a temporary override.",
            )
            .disable_help_subcommand(true);
    }
    command = localize_top_args(command);
    command = localize_subcommands(command);
    command = apply_localized_help_flags(command, true);
    if is_zh() {
        command = apply_chinese_help_template(command);
    }
    // 终端无缝集成组在根帮助里以静态段单独成节(这些子命令已 hide,
    // 不进 {subcommands});最后设置以免被上面的通用中文模板覆盖。
    command = command.help_template(root_help_template());
    command
}

pub(in crate::cli) fn root_help_template() -> String {
    let shell_block = t(
        "  fish-init          Integrate with fish; then chat in natural language directly in the terminal
  bash-init          Integrate with bash
  zsh-init           Integrate with zsh
  powershell-init    Integrate with PowerShell
  remove-shell-hook  Safely remove installed Miyu shell hooks
  models             Switch the terminal session's model (-g edits the global pool)
  variant            Switch the terminal session model's thinking level
  history            Show conversation history
  reset              Clear the terminal-integration session context
  reset-memory       Erase this persona's long-term memory
  pop                Move conversation turns out of active context",
        "  fish-init          集成到 fish，集成后可在终端直接使用自然语言交流
  bash-init          集成到 bash
  zsh-init           集成到 zsh
  powershell-init    集成到 PowerShell
  remove-shell-hook  安全删除已安装的 Miyu shell hook
  models             修改终端集成会话的模型（-g 改全局模型池）
  variant            切换终端集成会话模型的思考档位
  history            显示会话历史
  reset              清除终端集成会话上下文
  reset-memory       清空长期记忆
  pop                将对话轮次移出当前上下文",
    );
    if is_zh() {
        format!(
            "{{about}}

用法: {{usage}}

命令:
{{subcommands}}

终端无缝集成相关：
{shell_block}

参数:
{{positionals}}
选项:
{{options}}
{{after-help}}"
        )
    } else {
        format!(
            "{{about}}

Usage: {{usage}}

Commands:
{{subcommands}}

Terminal integration:
{shell_block}

Arguments:
{{positionals}}
Options:
{{options}}
{{after-help}}"
        )
    }
}

pub(in crate::cli) fn apply_localized_help_flags(mut command: clap::Command, root: bool) -> clap::Command {
    command = command.disable_help_flag(true).arg(
        Arg::new("help")
            .short('h')
            .long("help")
            .help(t("Print help", "显示帮助"))
            .action(ArgAction::Help),
    );
    if root {
        command = command.disable_version_flag(true).arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .help(t("Print version", "显示版本"))
                .action(ArgAction::Version),
        );
    }
    let subcommands = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name().to_string())
        .collect::<Vec<_>>();
    for name in subcommands {
        command = command.mut_subcommand(&name, |subcommand| {
            apply_localized_help_flags(subcommand, false)
        });
    }
    command
}

pub(in crate::cli) fn apply_chinese_help_template(mut command: clap::Command) -> clap::Command {
    let has_subcommands = command.get_subcommands().next().is_some();
    command = if has_subcommands {
        command.help_template(
            "{about}\n\n用法: {usage}\n\n命令:\n{subcommands}\n参数:\n{positionals}\n选项:\n{options}\n{after-help}",
        )
    } else {
        command.help_template(
            "{about}\n\n用法: {usage}\n\n参数:\n{positionals}\n选项:\n{options}\n{after-help}",
        )
    };
    let subcommands = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name().to_string())
        .collect::<Vec<_>>();
    for name in subcommands {
        command = command.mut_subcommand(&name, apply_chinese_help_template);
    }
    command
}

pub(in crate::cli) fn localize_top_args(command: clap::Command) -> clap::Command {
    command
        .mut_arg("debug", |arg| {
            arg.help(t(
                "Write detailed diagnostics to the Miyu log directory",
                "将详细诊断信息写入 Miyu 日志目录",
            ))
        })
        .mut_arg("stdout", |arg| {
            arg.help(t(
                "Plain output mode (no colors, no TUI); pipe-friendly for stdout redirection",
                "纯文本输出模式（无颜色、无 TUI）；适合管道重定向",
            ))
        })
        .mut_arg("continue_session", |arg| {
            arg.help(t(
                "Send the message into the terminal-integration session instead of a throwaway one-shot chat",
                "把消息发进终端集成会话，而不是用完即弃的一次性对话",
            ))
        })
        .mut_arg("message", |arg| {
            arg.help(t(
                "Message to send; omitted to enter REPL",
                "要发送的消息；省略则进入 REPL",
            ))
        })
}

pub(in crate::cli) fn localize_subcommands(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        (
            "ask",
            "Send one message to the assistant as a one-shot chat",
            "向助手发送一条消息，一次性对话",
        ),
        (
            "normal",
            "Enter the normal-mode REPL (full persona abilities)",
            "进入普通模式 REPL（人格全能力）",
        ),
        (
            "dev",
            "Enter the dev-mode REPL (minimal coding form, no persona)",
            "进入开发模式 REPL（极简编码形态，无人格）",
        ),
        (
            "tool-call",
            "Tool bridge: call this session's AI tools from the command line",
            "工具桥：以本会话身份调用 AI 工具",
        ),
        (
            "init",
            "Create default config and state files",
            "创建默认配置和状态文件",
        ),
        (
            "paths",
            "Show app config, data, and cache paths",
            "显示应用配置、数据和缓存路径",
        ),
        ("config", "Configure via the TUI", "使用 TUI 进行配置"),
        ("reload", "Reload configuration", "重新加载配置"),
        (
            "models",
            "Switch the terminal-integration session's model",
            "修改终端集成会话的模型",
        ),
        ("list-models", "List available models", "列出可用模型"),
        (
            "variant",
            "Switch the terminal session model's thinking level",
            "切换终端集成会话模型的思考档位",
        ),
        (
            "fish-init",
            "Integrate with fish so you can chat in natural language directly in the terminal",
            "集成到 fish，集成后可在终端直接使用自然语言交流。",
        ),
        ("bash-init", "Integrate with bash", "集成到 bash"),
        ("zsh-init", "Integrate with zsh", "集成到 zsh"),
        (
            "powershell-init",
            "Integrate with PowerShell so you can chat in natural language directly in the terminal",
            "集成到 PowerShell，集成后可在终端直接使用自然语言交流。",
        ),
        (
            "remove-shell-hook",
            "Safely remove installed Miyu shell hooks",
            "安全删除已安装的 Miyu shell hook",
        ),
        ("history", "Show conversation history", "显示会话历史"),
        (
            "pop",
            "Move conversation turns out of active context",
            "将对话轮次移出当前上下文",
        ),
        ("kb", "Manage the knowledge base", "管理知识库"),
        (
            "update-default-kb",
            "Update Miyu default knowledge base",
            "更新 Miyu 默认知识库",
        ),
        ("memory", "Manage assistant memory", "管理记忆"),
        ("skills", "Manage assistant skills", "管理助手 skills"),
        (
            "reset",
            "Clear the terminal-integration session context",
            "清除终端集成会话上下文",
        ),
        (
            "reset-memory",
            "Erase this persona's long-term memory",
            "清空长期记忆",
        ),
        (
            "wipe",
            "Erase all conversation history, memory, group contexts and their artifacts",
            "抹掉所有会话历史、记忆、群聊上下文和其产物",
        ),
                                                ("web", "Open the local Miyu WebUI", "访问本地 Miyu WebUI"),
        (
            "daemon",
            "Manage the unified Miyu background service",
            "管理 Miyu 统一后台服务",
        ),
        (
            "export",
            "Export configuration into a portable archive",
            "导出配置，把当前配置打包成可移植归档",
        ),
        ("import", "Import configuration", "导入配置"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    // 终端无缝集成组:从 {subcommands} 里藏掉,根帮助模板里以静态段
    // 单独成节(clap 不支持子命令分组);`miyu <cmd> -h` 不受影响。
    for name in [
        "fish-init",
        "bash-init",
        "zsh-init",
        "powershell-init",
        "remove-shell-hook",
        "models",
        "variant",
        "history",
        "reset",
        "reset-memory",
        "pop",
    ] {
        command = command.mut_subcommand(name, |subcommand| subcommand.hide(true));
    }
    for (index, name) in [
        "init",
        "config",
        "normal",
        "dev",
        "daemon",
        "web",
        "tool-call",
        "ask",
        "list-models",
        "export",
        "import",
        "kb",
        "memory",
        "skills",
        "update-default-kb",
        "wipe",
        "paths",
        "reload",
    ]
    .into_iter()
    .enumerate()
    {
        command = command
            .mut_subcommand(name, move |subcommand| subcommand.display_order(index));
    }
    command = command
        .mut_subcommand("ask", localize_ask_command)
        .mut_subcommand("models", localize_models_command)
        .mut_subcommand("variant", localize_variant_command)
        .mut_subcommand("history", localize_history_command)
        .mut_subcommand("pop", localize_pop_command)
        .mut_subcommand("kb", localize_kb_command)
        .mut_subcommand("memory", localize_memory_command)
        .mut_subcommand("skills", localize_skills_command)
        .mut_subcommand("config", localize_config_command)
        .mut_subcommand("web", localize_web_command)
        .mut_subcommand("daemon", localize_daemon_command)
        .mut_subcommand("export", localize_export_command)
        .mut_subcommand("import", localize_import_command);
    command
}

pub(in crate::cli) fn localize_export_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("output", |arg| {
            arg.help(t(
                "Archive path to write; omit to name it after this host and time",
                "要写入的归档路径；省略则按主机名与时间自动命名",
            ))
        })
        .mut_arg("all", |arg| {
            arg.help(t(
                "Include everything portable, index and platform history included",
                "包含全部可移植数据，含向量索引与平台历史",
            ))
        })
        .mut_arg("index", |arg| {
            arg.help(t(
                "Include the knowledge-base vector index (large; rebuildable with `miyu kb embed`)",
                "包含知识库向量索引（很大；可用 miyu kb embed 重建）",
            ))
        })
        .mut_arg("platforms", |arg| {
            arg.help(t(
                "Include chat-platform history",
                "包含通讯平台的聊天历史",
            ))
        })
        .mut_arg("no_secrets", |arg| {
            arg.help(t(
                "Blank out API keys and tokens (you must refill them after importing)",
                "清空 API key 与访问令牌（导入后需要自行补填）",
            ))
        })
        .mut_arg("dry_run", |arg| {
            arg.help(t(
                "Print what would be packed without writing an archive",
                "只打印将要打包的内容，不实际写归档",
            ))
        })
        .mut_arg("force", |arg| {
            arg.help(t("Overwrite an existing archive", "覆盖已存在的归档文件"))
        })
}

pub(in crate::cli) fn localize_import_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("archive", |arg| {
            arg.help(t("Archive produced by `miyu export`", "miyu export 生成的归档"))
        })
        .mut_arg("force", |arg| {
            arg.help(t(
                "Overwrite existing data (the current installation is backed up first)",
                "覆盖已有数据（覆盖前会先备份当前安装）",
            ))
        })
}

pub(in crate::cli) fn localize_ask_command(command: clap::Command) -> clap::Command {
    command.mut_arg("message", |arg| {
        arg.help(t("Message to send", "要发送的消息"))
    })
}

pub(in crate::cli) fn localize_models_command(command: clap::Command) -> clap::Command {
    command.mut_arg("target", |arg| {
        arg.help(t(
            "List index, provider/model, or 'default' to follow the global pool",
            "模型列表序号、供应商/模型名，或 default 恢复跟随全局模型池",
        ))
    })
}

pub(in crate::cli) fn localize_variant_command(command: clap::Command) -> clap::Command {
    command.mut_arg("name", |arg| {
        arg.help(t(
            "Thinking level to select; omit to choose interactively",
            "要选择的思考档位；省略则进入交互选择",
        ))
    })
}

pub(in crate::cli) fn localize_history_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("limit", |arg| {
            arg.help(t("Number of history entries to show", "显示的历史条数"))
        })
        .mut_arg("raw", |arg| {
            arg.help(t("Print raw JSONL entries", "输出原始 JSONL 条目"))
        })
        .mut_arg("no_thinking", |arg| {
            arg.help(t("Hide stored reasoning", "隐藏已保存的思考内容"))
        })
}

pub(in crate::cli) fn localize_pop_command(command: clap::Command) -> clap::Command {
    command.mut_arg("count", |arg| {
        arg.help(t(
            "Number of oldest turns to pop; omit to select interactively",
            "要弹出的最旧轮次数；省略则进入交互多选",
        ))
    })
}

pub(in crate::cli) fn localize_config_command(command: clap::Command) -> clap::Command {
    command
        .mut_subcommand("validate", |subcommand| {
            subcommand.about(t("Validate configuration", "校验配置"))
        })
        .mut_subcommand("paths", |subcommand| {
            subcommand.about(t("Show configuration paths", "显示配置路径"))
        })
}

pub(in crate::cli) fn localize_web_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("port", |arg| arg.help(t("Local TCP port", "本地 TCP 端口")))
        .mut_arg("password", |arg| {
            arg.help(t(
                "Prompt securely for a required password",
                "安全输入所需的访问密码",
            ))
        })
        .mut_arg("password_file", |arg| {
            arg.help(t(
                "Read the WebUI password from a file",
                "从文件读取 WebUI 访问密码",
            ))
        })
}

pub(in crate::cli) fn localize_daemon_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        (
            "start",
            "Start all configured Miyu interfaces",
            "启动所有已配置的 Miyu 接口",
        ),
        (
            "stop",
            "Stop the Miyu background service",
            "停止 Miyu 后台服务",
        ),
        (
            "restart",
            "Restart the Miyu background service",
            "重启 Miyu 后台服务",
        ),
        (
            "status",
            "Show daemon and interface status",
            "显示 daemon 与接口状态",
        ),
        ("logs", "Follow daemon logs", "持续查看 daemon 日志"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    command
        .mut_arg("port", |arg| {
            arg.help(t("WebUI TCP port", "WebUI TCP 端口"))
        })
        .mut_subcommand("logs", |subcommand| {
            subcommand.mut_arg("lines", |arg| {
                arg.help(t(
                    "Print only the most recent N lines and exit",
                    "仅输出最近 N 行后退出",
                ))
            })
        })
}

pub(in crate::cli) fn localize_kb_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        ("add", "Add a file or directory", "添加文件或目录"),
        ("list", "List indexed files", "列出已索引文件"),
        ("search", "Search knowledge base content", "搜索知识库内容"),
        ("find", "Find files by name", "按文件名查找文件"),
        ("read", "Read a knowledge base file", "读取知识库文件"),
        ("remove", "Remove a knowledge base file", "移除知识库文件"),
        (
            "reindex",
            "Rebuild keyword index on demand",
            "按需重建关键词索引",
        ),
        ("stats", "Show knowledge base statistics", "显示知识库统计"),
        ("embed", "Manage semantic embeddings", "管理语义嵌入"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    command
        .mut_subcommand("add", |subcommand| {
            subcommand
                .mut_arg("path", |arg| arg.help(t("Path to add", "要添加的路径")))
                .mut_arg("recursive", |arg| {
                    arg.help(t(
                        "Compatibility flag; directories are recursive by default",
                        "兼容参数；目录默认递归导入",
                    ))
                })
        })
        .mut_subcommand("search", |subcommand| {
            subcommand
                .mut_arg("query", |arg| arg.help(t("Search query", "搜索查询")))
                .mut_arg("limit", |arg| arg.help(t("Maximum results", "最大结果数")))
        })
        .mut_subcommand("find", |subcommand| {
            subcommand
                .mut_arg("query", |arg| arg.help(t("Filename query", "文件名查询")))
                .mut_arg("limit", |arg| arg.help(t("Maximum results", "最大结果数")))
        })
        .mut_subcommand("read", |subcommand| {
            subcommand
                .mut_arg("file", |arg| {
                    arg.help(t("Knowledge base file name", "知识库文件名"))
                })
                .mut_arg("start", |arg| arg.help(t("Starting line", "起始行")))
                .mut_arg("lines", |arg| arg.help(t("Number of lines", "读取行数")))
        })
        .mut_subcommand("remove", |subcommand| {
            subcommand.mut_arg("file", |arg| arg.help(t("File to remove", "要移除的文件")))
        })
}

pub(in crate::cli) fn localize_memory_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        ("stats", "Show memory statistics", "显示记忆统计"),
        ("reset", "Clear assistant memory", "清空助手记忆"),
        ("search", "Search memories", "搜索记忆"),
        ("remember", "Save a manual fact", "手动保存事实"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    command
        .mut_subcommand("reset", |subcommand| {
            subcommand.mut_arg("include_skills", |arg| {
                arg.help(t(
                    "Also remove generated skills",
                    "同时移除自动生成的 skills",
                ))
            })
        })
        .mut_subcommand("search", |subcommand| {
            subcommand
                .mut_arg("query", |arg| arg.help(t("Search query", "搜索查询")))
                .mut_arg("limit", |arg| arg.help(t("Maximum results", "最大结果数")))
                .mut_arg("forgotten", |arg| {
                    arg.help(t("Include forgotten memories", "包含已遗忘记忆"))
                })
        })
        .mut_subcommand("remember", |subcommand| {
            subcommand
                .mut_arg("content", |arg| arg.help(t("Fact content", "事实内容")))
                .mut_arg("source", |arg| arg.help(t("Source label", "来源标签")))
        })
}

pub(in crate::cli) fn localize_skills_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        ("list", "List skills", "列出 skills"),
        ("show", "Show a skill", "显示 skill"),
        ("enable", "Enable a skill", "启用 skill"),
        ("disable", "Disable a skill", "禁用 skill"),
        ("remove", "Remove a skill", "移除 skill"),
        ("stats", "Show skill statistics", "显示 skill 统计"),
        (
            "prune",
            "Remove disabled generated skills",
            "清理已禁用的自动 skills",
        ),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    for name in ["show", "enable", "disable", "remove"] {
        command = command.mut_subcommand(name, |subcommand| {
            subcommand.mut_arg("name", |arg| arg.help(t("Skill name", "skill 名称")))
        });
    }
    command
}
