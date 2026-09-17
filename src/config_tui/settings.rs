//! 通用设置项。
//!
//! 界面语言、工具加载模式、混合端点显示——都是单值开关，没有跨项约束，所以
//! 一个表单装得下。

use crate::config_tui::*;

/// true = save and exit, false = discard and exit. A choice is mandatory:
/// `q`/`Esc` are ignored so an accidental key press cannot lose edits.
pub(in crate::config_tui) fn confirm_save_on_exit(stdout: &mut io::Stdout) -> Result<bool> {
    let options = [
        t("Save", "保存").to_string(),
        t("Discard", "不保存").to_string(),
    ];
    let mut selected = 0usize;
    loop {
        draw_menu(
            stdout,
            t(" SAVE EDITED CHANGES? ", " 是否保存已编辑内容 "),
            &options,
            selected,
            t("[j/k]move [Enter]confirm", "[j/k]移动 [Enter]确认"),
        )?;
        match read_key()? {
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(1),
            KeyCode::Enter => return Ok(selected == 0),
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn edit_settings(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
) -> Result<()> {
    let language = language_choice_value(&config.display.language).unwrap_or("auto");
    let mut fields = vec![
        Field::boolean(t("Enable tools", "工具启用"), config.tools.enabled),
        Field::new(
            t("Maximum tool rounds", "工具最大轮数"),
            config.tools.max_rounds.to_string(),
        ),
        Field::new(
            t("Tool loading mode", "工具加载模式"),
            normalize_tools_loading_mode(&config.tools.loading_mode),
        )
        .choices(&["full", "stub"]),
        Field::boolean(
            t("Remember loaded tools", "记住已加载工具"),
            config.tools.persist_loaded_tools,
        ),
        Field::boolean(t("Enable skills", "Skills 启用"), config.skills.enabled),
        Field::boolean(
            t("Allow command execution", "允许执行命令"),
            config.skills.allow_command_execution,
        ),
        Field::new(t("Interface language", "界面语言"), language.to_string())
            .choices(&["auto", "en", "zh"]),
        Field::new(
            t("Show reasoning", "显示思考过程"),
            config.display.reasoning.clone(),
        )
        .choices(&["summary", "full", "hidden"]),
        Field::new(
            t("Show tool call details", "显示工具调用信息"),
            config.display.tool_calls.clone(),
        )
        .choices(&["summary", "full", "hidden"]),
        Field::new(
            t("Command lines", "命令显示行数"),
            config.display.command_output_lines.to_string(),
        ),
        Field::boolean(
            t("Readable tool names", "工具名可读显示"),
            config.display.readable_tool_names,
        ),
        Field::boolean(
            t(
                "Show token usage in shell conversations",
                "Shell 无缝对话显示 Token 计数",
            ),
            config.display.show_token_usage,
        ),
        Field::new(
            t(
                "Show current provider/model in Mixed mode",
                "Mixed 时显示本次供应商/模型",
            ),
            parse_mixed_endpoint_display(&config.display.mixed_model_endpoint_display),
        )
        .choices(&["off", "interactive", "all"]),
        // Appended rather than inserted: the read-back below is positional.
        Field::new(
            t(
                "Turns replayed when reopening the TUI",
                "重开 TUI 回放的轮数",
            ),
            config.display.repl_replay_turns.to_string(),
        ),
        Field::boolean(
            t("Block dangerous commands", "高危命令拦截"),
            config.tools.block_dangerous_commands,
        ),
        Field::boolean(
            t("Keep the timeline open", "不自动收起过程"),
            config.display.keep_timeline_open,
        ),
    ];
    // The read-back below is by index, so an insert in the middle silently
    // writes every later value into the wrong setting. This catches that in
    // debug builds; new fields go on the end.
    debug_assert_eq!(
        fields.len(),
        16,
        "global settings fields changed: update the positional read-back below"
    );
    run_form_without_buttons(stdout, t(" GLOBAL SETTINGS ", " 全局设置 "), &mut fields)?;
    config.tools.enabled = parse_bool_field(&fields[0].value)?;
    config.tools.max_rounds = fields[1].value.trim().parse::<usize>()?;
    config.tools.loading_mode = normalize_tools_loading_mode(&fields[2].value);
    config.tools.persist_loaded_tools = parse_bool_field(&fields[3].value)?;
    config.skills.enabled = parse_bool_field(&fields[4].value)?;
    config.skills.allow_command_execution = parse_bool_field(&fields[5].value)?;
    config.display.language = language_choice_value(&fields[6].value)
        .unwrap_or("auto")
        .to_string();
    config.display.reasoning = fields[7].value.trim().to_string();
    config.display.tool_calls = fields[8].value.trim().to_string();
    config.display.command_output_lines = fields[9]
        .value
        .trim()
        .parse::<usize>()?
        .min(MAX_COMMAND_OUTPUT_LINES);
    config.display.readable_tool_names = parse_bool_field(&fields[10].value)?;
    config.display.show_token_usage = parse_bool_field(&fields[11].value)?;
    config.display.mixed_model_endpoint_display = parse_mixed_endpoint_display(&fields[12].value);
    config.display.repl_replay_turns = fields[13]
        .value
        .trim()
        .parse::<usize>()?
        .min(MAX_REPL_REPLAY_TURNS);
    config.tools.block_dangerous_commands = parse_bool_field(&fields[14].value)?;
    config.display.keep_timeline_open = parse_bool_field(&fields[15].value)?;
    Ok(())
}

pub(in crate::config_tui) fn language_choice_label(value: &str, zh: bool) -> Option<&'static str> {
    match (value.trim(), zh) {
        ("auto", false) => Some("Auto"),
        ("auto", true) => Some("自动"),
        ("en", false) => Some("English"),
        ("en", true) => Some("英语"),
        ("zh", false) => Some("Simplified Chinese"),
        ("zh", true) => Some("简体中文"),
        _ => None,
    }
}

pub(in crate::config_tui) fn language_choice_value(value: &str) -> Option<&'static str> {
    match value.trim() {
        "auto" | "Auto" | "自动" => Some("auto"),
        "en" | "English" | "英语" => Some("en"),
        "zh" | "Simplified Chinese" | "简体中文" => Some("zh"),
        _ => None,
    }
}

pub(in crate::config_tui) fn parse_mixed_endpoint_display(value: &str) -> String {
    match value.trim() {
        "关" | "Off" | "off" => "off".to_string(),
        "全部模式" | "All modes" | "all" => "all".to_string(),
        _ => "interactive".to_string(),
    }
}

pub(in crate::config_tui) fn normalize_tools_loading_mode(value: &str) -> String {
    // hybrid 档 09-01 删除;它和 lazy 同属懒加载家族,历史值一律归入需加载,
    // 悄悄升成 full 会让旧配置的工具面字节数翻好几倍。
    match value.trim() {
        "full" => "full".to_string(),
        _ => "stub".to_string(),
    }
}

pub(in crate::config_tui) fn parse_bool_field(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "1" | "on" | "启用" | "是" => Ok(true),
        "false" | "no" | "n" | "0" | "off" | "禁用" | "否" => Ok(false),
        value => {
            if is_zh() {
                bail!("无效的布尔值: {value}")
            } else {
                bail!("Invalid boolean value: {value}")
            }
        }
    }
}
