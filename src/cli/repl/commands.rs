//! REPL 的斜杠命令表。
//!
//! `REPL_COMMAND_TABLE` 是单一事实来源：补全、帮助、解析全从它派生，加命令只
//! 用改这一处。

use crate::cli::*;

pub(in crate::cli) fn print_mode_help() {
    if crate::i18n::is_zh() {
        println!("请选择模式。想让裸 miyu 命令直接进某个模式,可以在设置中修改(config.jsonc 的 default_mode)。\n");
        println!("  miyu normal   普通模式。可使用全部工具,适合日常使用。支持角色扮演、娱乐聊天、记忆、技能等全部能力。");
        println!("  miyu dev      开发模式。与普通模式明确区分,用于开发工作;移除与开发无关的角色扮演与娱乐工具,提示词极简可编辑,记忆独立。");
        println!("  miyu '<your_prompts>'   使用普通模式进行一次性对话");
    } else {
        println!("Pick a mode. To make bare `miyu` enter one directly, set default_mode in config.jsonc.\n");
        println!("  miyu normal   full-capability mode: persona, memory, every tool.");
        println!("  miyu dev      development mode: minimal editable prompt, coding tools only, separate memory.");
        println!("  miyu '<your_prompts>'   one-shot ask in normal mode");
    }
}

pub(in crate::cli) fn print_repl_help() {
    println!("{}", t("commands:", "命令:"));
    let width = REPL_COMMAND_TABLE
        .iter()
        .map(|spec| {
            spec.name.len()
                + if spec.arg_hint.is_empty() {
                    0
                } else {
                    spec.arg_hint.len() + 1
                }
        })
        .max()
        .unwrap_or(0);
    for spec in REPL_COMMAND_TABLE {
        let invocation = if spec.arg_hint.is_empty() {
            spec.name.to_string()
        } else {
            format!("{} {}", spec.name, spec.arg_hint)
        };
        println!("  {invocation:<width$}  {}", t(spec.help_en, spec.help_zh));
    }
    println!("{}", t("keys:", "快捷键:"));
    println!(
        "  Tab         {}",
        t(
            "cycle NORMAL/CHAT, or complete slash commands",
            "循环切换 普通/闲聊，或补全斜杠菜单"
        )
    );
    println!("  Enter       {}", t("send message", "发送消息"));
    println!("  Shift+Enter {}", t("insert newline", "插入换行"));
    println!(
        "  Ctrl+J      {}",
        t(
            "insert newline, same as Shift+Enter",
            "插入换行，与 Shift+Enter 相同"
        )
    );
    println!(
        "  Ctrl+V      {}",
        t(
            "paste image or text from clipboard",
            "从剪贴板粘贴图片或文本"
        )
    );
    println!("  Ctrl+L      {}", t("clear screen", "清屏"));
    println!(
        "  Up/Down     {}",
        t("browse input history", "切换输入历史")
    );
    println!(
        "  Esc Esc     {}",
        t("interrupt running reply", "中断当前回复")
    );
    println!(
        "  Ctrl+C      {}",
        t(
            "clear the draft, else interrupt the reply, else stop background tasks, else exit",
            "先清空输入；输入为空则中断回复；再无回复则停止后台任务；都没有则退出"
        )
    );
    println!("  Ctrl+D      {}", t("exit", "退出"));
}

pub(in crate::cli) fn repl_command_suggestions_line(
    suggestions: &[&str],
    max_width: usize,
) -> String {
    let line = if suggestions.len() == 1 {
        suggestions[0].to_string()
    } else {
        suggestions.join("  ")
    };
    truncate_visible_width(&line, max_width)
}
