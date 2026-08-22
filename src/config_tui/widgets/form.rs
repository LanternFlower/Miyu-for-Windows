//! 表单：字段、编辑、按钮。
//!
//! `run_form_from` 是主循环，`Field` 是它的单元。字段的显示值与内部值分开
//! （`field_display_value`）——密钥要显示成掩码，布尔要显示成「开/关」。

use crate::config_tui::*;

pub(in crate::config_tui) fn edit_u16_value(
    stdout: &mut io::Stdout,
    label: &'static str,
    current: u16,
) -> Result<Option<u16>> {
    let mut fields = vec![Field::new(label, current.to_string())];
    if !run_form_editing(stdout, t(" EDIT VALUE ", " 编辑数值 "), &mut fields)? {
        return Ok(None);
    }
    match fields[0].value.trim().parse() {
        Ok(value) => Ok(Some(value)),
        Err(_) => {
            message(stdout, t("Invalid number.", "数值无效。"))?;
            Ok(None)
        }
    }
}

pub(in crate::config_tui) fn edit_inline_value(
    stdout: &mut io::Stdout,
    title: &str,
    current: &str,
    sensitive: bool,
) -> Result<Option<String>> {
    let mut value = current.to_string();
    let mut cursor = value.chars().count();
    let mut fcitx = FcitxState::new();
    fcitx.enter_editing();
    loop {
        draw_inline_editor(stdout, title, &value, cursor, sensitive)?;
        match read_key()? {
            KeyCode::Esc => {
                fcitx.leave_editing();
                execute!(stdout, Hide)?;
                return Ok(None);
            }
            KeyCode::Enter => {
                fcitx.leave_editing();
                execute!(stdout, Hide)?;
                return Ok(Some(value));
            }
            KeyCode::Left => cursor = cursor.saturating_sub(1),
            KeyCode::Right => cursor = (cursor + 1).min(value.chars().count()),
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = value.chars().count(),
            KeyCode::Backspace if cursor > 0 => remove_char_before_cursor(&mut value, &mut cursor),
            KeyCode::Delete => remove_char_at_cursor(&mut value, cursor),
            KeyCode::Char(ch) => insert_char_at_cursor(&mut value, &mut cursor, ch),
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn draw_inline_editor(
    stdout: &mut io::Stdout,
    title: &str,
    value: &str,
    cursor: usize,
    sensitive: bool,
) -> Result<()> {
    let (cols, rows) = terminal::size()?;
    let width = 72_u16.min(cols.saturating_sub(2)).max(12);
    let height = rows.clamp(1, 6);
    let x = cols.saturating_sub(width) / 2;
    let y = rows.saturating_sub(height) / 2;
    let capacity = width.saturating_sub(4) as usize;
    let chars = value.chars().collect::<Vec<_>>();
    let cursor = cursor.min(chars.len());
    let start = cursor
        .saturating_sub(capacity.saturating_sub(1))
        .min(chars.len().saturating_sub(capacity));
    let end = (start + capacity).min(chars.len());
    let visible = if sensitive {
        "*".repeat(end.saturating_sub(start))
    } else {
        chars[start..end].iter().collect::<String>()
    };

    queue!(stdout, Hide, Clear(ClearType::All))?;
    draw_box(stdout, x, y, width, height, title)?;
    queue!(
        stdout,
        MoveTo(x + 2, y + 2),
        Print(pad(&visible, capacity)),
        MoveTo(x + 2, y + 4),
        SetAttribute(Attribute::Dim),
        Print(truncate(
            t("[Enter]save  [Esc]cancel", "[Enter]保存  [Esc]取消"),
            capacity,
        )),
        SetAttribute(Attribute::Reset),
        MoveTo(
            x + 2 + u16::try_from(cursor.saturating_sub(start)).unwrap_or(u16::MAX),
            y + 2,
        ),
        Show,
    )?;
    stdout.flush()?;
    Ok(())
}

pub(in crate::config_tui) fn run_form(
    stdout: &mut io::Stdout,
    title: &str,
    fields: &mut [Field],
) -> Result<bool> {
    run_form_from(stdout, title, fields, false)
}

/// `start_editing` puts the caret in the first field straight away, for forms
/// reached from a menu row that already showed the value: the row said what it
/// was, Enter said "change it", so a second Enter to begin typing is a keypress
/// that asks a question nobody had.
pub(in crate::config_tui) fn run_form_editing(
    stdout: &mut io::Stdout,
    title: &str,
    fields: &mut [Field],
) -> Result<bool> {
    run_form_from(stdout, title, fields, true)
}

pub(in crate::config_tui) fn run_form_from(
    stdout: &mut io::Stdout,
    title: &str,
    fields: &mut [Field],
    start_editing: bool,
) -> Result<bool> {
    let mut selected = 0usize;
    let mut fcitx = FcitxState::new();
    // Only a plain text field can be typed into directly; the others open
    // their own picker on Enter, so landing "inside" them would mean typing
    // free text where a choice was expected.
    let mut editing = start_editing
        && fields.first().is_some_and(|field| {
            !field.boolean && !field.textarea && !field.modalities && field.choices.is_empty()
        });
    if editing {
        fcitx.enter_editing();
    }
    let mut cursors = fields
        .iter()
        .map(|field| field.value.chars().count())
        .collect::<Vec<_>>();
    loop {
        draw_form(stdout, title, fields, selected, editing, &cursors, true)?;
        match read_key()? {
            KeyCode::Esc if editing => {
                fcitx.leave_editing();
                editing = false;
            }
            KeyCode::Esc | KeyCode::Char('q') if !editing => return Ok(false),
            KeyCode::Enter if editing => {
                fcitx.leave_editing();
                editing = false;
            }
            KeyCode::Enter if !editing && selected == fields.len() => return Ok(true),
            KeyCode::Enter if !editing && selected == fields.len() + 1 => return Ok(false),
            KeyCode::Enter if !editing && fields[selected].boolean => {
                let value = select_bool(
                    stdout,
                    fields[selected].label,
                    parse_bool_field(&fields[selected].value)?,
                )?;
                fields[selected].value = value.to_string();
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].modalities => {
                fields[selected].value = select_multi_choice(
                    stdout,
                    fields[selected].label,
                    &fields[selected].value,
                    &["text", "image", "audio", "video", "pdf"]
                        .iter()
                        .map(|item| item.to_string())
                        .collect::<Vec<_>>(),
                )?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && !fields[selected].choices.is_empty() => {
                fields[selected].value = select_choice(
                    stdout,
                    fields[selected].label,
                    &fields[selected].value,
                    &fields[selected].choices,
                    fields[selected].empty_choice_label,
                    fields[selected].raw_choice_labels,
                )?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].dialog_list => {
                edit_dialog_list(stdout, &mut fields[selected].value)?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].textarea => {
                edit_textarea(stdout, &mut fields[selected].value)?;
                cursors[selected] = fields[selected].value.chars().count();
                if !fields[selected].sensitive {
                    return Ok(true);
                }
            }
            KeyCode::Enter if !editing => {
                if !fields[selected].boolean {
                    fcitx.enter_editing();
                    editing = true;
                }
            }
            KeyCode::Char('s') if !editing => return Ok(true),
            KeyCode::Up | KeyCode::Char('k') if !editing => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') if !editing => {
                selected = (selected + 1).min(fields.len() + 1)
            }
            KeyCode::Left | KeyCode::Char('h') if !editing && selected == fields.len() + 1 => {
                selected = fields.len()
            }
            KeyCode::Right | KeyCode::Char('l') if !editing && selected == fields.len() => {
                selected = fields.len() + 1
            }
            KeyCode::Left if editing => cursors[selected] = cursors[selected].saturating_sub(1),
            KeyCode::Right if editing => {
                cursors[selected] =
                    (cursors[selected] + 1).min(fields[selected].value.chars().count())
            }
            KeyCode::Home if editing => cursors[selected] = 0,
            KeyCode::End if editing => cursors[selected] = fields[selected].value.chars().count(),
            KeyCode::Backspace if editing => {
                if cursors[selected] > 0 {
                    remove_char_before_cursor(&mut fields[selected].value, &mut cursors[selected]);
                }
            }
            KeyCode::Delete if editing => {
                remove_char_at_cursor(&mut fields[selected].value, cursors[selected])
            }
            KeyCode::Char(char) if editing => {
                insert_char_at_cursor(&mut fields[selected].value, &mut cursors[selected], char)
            }
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn run_form_without_buttons(
    stdout: &mut io::Stdout,
    title: &str,
    fields: &mut [Field],
) -> Result<()> {
    let mut selected = 0usize;
    let mut editing = false;
    let mut fcitx = FcitxState::new();
    let mut cursors = fields
        .iter()
        .map(|field| field.value.chars().count())
        .collect::<Vec<_>>();
    loop {
        draw_form(stdout, title, fields, selected, editing, &cursors, false)?;
        match read_key()? {
            KeyCode::Esc if editing => {
                fcitx.leave_editing();
                editing = false;
            }
            KeyCode::Esc | KeyCode::Char('q') if !editing => return Ok(()),
            KeyCode::Enter if editing => {
                fcitx.leave_editing();
                editing = false;
            }
            KeyCode::Enter if !editing && fields[selected].boolean => {
                let value = select_bool(
                    stdout,
                    fields[selected].label,
                    parse_bool_field(&fields[selected].value)?,
                )?;
                fields[selected].value = value.to_string();
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].modalities => {
                fields[selected].value = select_multi_choice(
                    stdout,
                    fields[selected].label,
                    &fields[selected].value,
                    &["text", "image", "audio", "video", "pdf"]
                        .iter()
                        .map(|item| item.to_string())
                        .collect::<Vec<_>>(),
                )?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && !fields[selected].choices.is_empty() => {
                fields[selected].value = select_choice(
                    stdout,
                    fields[selected].label,
                    &fields[selected].value,
                    &fields[selected].choices,
                    fields[selected].empty_choice_label,
                    fields[selected].raw_choice_labels,
                )?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].textarea => {
                edit_textarea(stdout, &mut fields[selected].value)?;
                cursors[selected] = fields[selected].value.chars().count();
                if !fields[selected].sensitive {
                    return Ok(());
                }
            }
            KeyCode::Enter if !editing => {
                if !fields[selected].boolean {
                    fcitx.enter_editing();
                    editing = true;
                }
            }
            KeyCode::Up | KeyCode::Char('k') if !editing => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') if !editing => {
                selected = (selected + 1).min(fields.len().saturating_sub(1))
            }
            KeyCode::Left if editing => cursors[selected] = cursors[selected].saturating_sub(1),
            KeyCode::Right if editing => {
                cursors[selected] =
                    (cursors[selected] + 1).min(fields[selected].value.chars().count())
            }
            KeyCode::Home if editing => cursors[selected] = 0,
            KeyCode::End if editing => cursors[selected] = fields[selected].value.chars().count(),
            KeyCode::Backspace if editing => {
                if cursors[selected] > 0 {
                    remove_char_before_cursor(&mut fields[selected].value, &mut cursors[selected]);
                }
            }
            KeyCode::Delete if editing => {
                remove_char_at_cursor(&mut fields[selected].value, cursors[selected])
            }
            KeyCode::Char(char) if editing => {
                insert_char_at_cursor(&mut fields[selected].value, &mut cursors[selected], char)
            }
            _ => {}
        }
    }
}

/// 预设对话列表式编辑器(验收 #19):每行一对 user/assistant,回车编辑、
/// [a] 新增、[d] 删除;退出时把列表写回 `user:`/`assistant:` 行格式,
/// 与手写 dialogs 文件同构,存量文件无需迁移。
pub(in crate::config_tui) fn edit_dialog_list(
    stdout: &mut io::Stdout,
    value: &mut String,
) -> Result<()> {
    let mut pairs = crate::persona_hint::parse_dialogs(value);
    let mut selected = 0usize;
    loop {
        let mut options: Vec<String> = pairs
            .iter()
            .map(|(question, answer)| {
                format!(
                    "user: {}  assistant: {}",
                    truncate(question.lines().next().unwrap_or(""), 20),
                    truncate(answer.lines().next().unwrap_or(""), 20),
                )
            })
            .collect();
        if options.is_empty() {
            options.push(t("(no preset dialogs)", "(暂无预设对话)").to_string());
        }
        selected = selected.min(options.len() - 1);
        draw_menu(
            stdout,
            t(" PRESET DIALOGS ", " 预设对话 "),
            &options,
            selected,
            t(
                "[Enter]edit [a]add [d]delete [j/k]move [q]done",
                "[Enter]编辑 [a]新增 [d]删除 [j/k]移动 [q]完成",
            ),
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => {
                *value = crate::persona_hint::format_dialogs(&pairs);
                return Ok(());
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Char('a') => {
                if let Some(pair) =
                    edit_dialog_pair(stdout, t(" NEW DIALOG ", " 新增对话 "), "", "")?
                {
                    pairs.push(pair);
                    selected = pairs.len() - 1;
                }
            }
            KeyCode::Enter if !pairs.is_empty() => {
                let (question, answer) = pairs[selected].clone();
                if let Some(pair) =
                    edit_dialog_pair(stdout, t(" EDIT DIALOG ", " 编辑对话 "), &question, &answer)?
                {
                    pairs[selected] = pair;
                }
            }
            KeyCode::Char('d') if !pairs.is_empty() => {
                pairs.remove(selected);
            }
            _ => {}
        }
    }
}

/// user/assistant 双框表单:打开即落在 user 框内直接输入,回车确认后
/// j 移到 assistant 框。空的一侧视为放弃(与 `parse_dialogs` 丢弃
/// 空对的语义一致)。
pub(in crate::config_tui) fn edit_dialog_pair(
    stdout: &mut io::Stdout,
    title: &str,
    question: &str,
    answer: &str,
) -> Result<Option<(String, String)>> {
    let mut fields = vec![
        Field::new("user", question.to_string()),
        Field::new("assistant", answer.to_string()),
    ];
    if !run_form_editing(stdout, title, &mut fields)? {
        return Ok(None);
    }
    let question = fields[0].value.trim().to_string();
    let answer = fields[1].value.trim().to_string();
    if question.is_empty() || answer.is_empty() {
        return Ok(None);
    }
    Ok(Some((question, answer)))
}

pub(in crate::config_tui) fn edit_textarea(
    stdout: &mut io::Stdout,
    value: &mut String,
) -> Result<()> {
    execute!(
        stdout,
        Show,
        LeaveAlternateScreen,
        Clear(ClearType::All),
        MoveTo(0, 0)
    )?;
    stdout.flush()?;
    terminal::disable_raw_mode()?;
    let temp_path = textarea_temp_file(value)?;
    let path = temp_path.to_path_buf();
    let status = open_text_editor(&path);
    if let Err(err) = status {
        if is_zh() {
            eprintln!("无法打开编辑器: {err}");
        } else {
            eprintln!("Failed to open editor: {err}");
        }
    } else if cfg!(windows) {
        writeln!(
            stdout,
            "{}",
            t(
                "Save the file in the editor, then return here and press Enter to continue.",
                "请在编辑器中保存文件，然后回到这里按 Enter 继续。"
            )
        )?;
        stdout.flush()?;
        let mut confirmation = String::new();
        io::stdin().read_line(&mut confirmation)?;
    }
    *value = std::fs::read_to_string(&path)?.trim().to_string();
    terminal::enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, Clear(ClearType::All), Hide)?;
    Ok(())
}

fn open_text_editor(path: &std::path::Path) -> std::io::Result<()> {
    for var in ["VISUAL", "EDITOR"] {
        if let Some(command) = std::env::var(var).ok().filter(|value| !value.trim().is_empty()) {
            let mut parts = command.split_whitespace();
            if let Some(program) = parts.next() {
                if Command::new(program).args(parts).arg(path).status().is_ok() {
                    return Ok(());
                }
            }
        }
    }
    let candidates: &[&str] = if cfg!(windows) {
        &["notepad"]
    } else {
        &["vim", "nano", "vi"]
    };
    for editor in candidates {
        let launched = if cfg!(windows) {
            Command::new(*editor).arg(path).spawn().map(|_| ())
        } else {
            Command::new(*editor).arg(path).status().map(|_| ())
        };
        if launched.is_ok() {
            return Ok(());
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no text editor found (set $VISUAL or $EDITOR)",
    ))
}

pub(in crate::config_tui) fn textarea_temp_file(
    value: &str,
) -> std::io::Result<tempfile::TempPath> {
    let mut file = tempfile::Builder::new()
        .prefix("miyu-persona-")
        .suffix(".md")
        .tempfile()?;
    file.write_all(value.as_bytes())?;
    file.flush()?;
    Ok(file.into_temp_path())
}

pub(in crate::config_tui) fn draw_form(
    stdout: &mut io::Stdout,
    title: &str,
    fields: &[Field],
    selected: usize,
    editing: bool,
    cursors: &[usize],
    show_buttons: bool,
) -> Result<()> {
    let (cols, rows) = terminal::size()?;
    let width = cols.saturating_sub(8).min(96).max(48);
    let height = (fields.len() as u16 + 8)
        .min(rows.saturating_sub(4))
        .max(10);
    let x = cols.saturating_sub(width) / 2;
    let y = rows.saturating_sub(height) / 2;
    queue!(stdout, Clear(ClearType::All))?;
    draw_box(stdout, x, y, width, height, title)?;
    queue!(
        stdout,
        MoveTo(x + 2, y + 1),
        Print(if show_buttons {
            t(
                "[j/k]move [Enter]edit/open editor [s]confirm [q]back",
                "[j/k]移动 [Enter]编辑/打开编辑器 [s]确认 [q]返回",
            )
        } else {
            t(
                "[j/k]move [Enter]edit/open editor [q]back",
                "[j/k]移动 [Enter]编辑/打开编辑器 [q]返回",
            )
        })
    )?;
    let mut cursor = None;
    for (index, field) in fields.iter().enumerate() {
        let row_y = y + index as u16 + 3;
        queue!(stdout, MoveTo(x + 2, row_y))?;
        let marker = if index == selected { ">" } else { " " };
        let value = field_display_value(field, index == selected && editing);
        let prefix = format!("{marker} {}: ", field.label);
        let line = truncate(
            &format!("{prefix}{value}"),
            width.saturating_sub(4) as usize,
        );
        if index == selected && !editing {
            queue!(
                stdout,
                SetAttribute(Attribute::Reverse),
                Print(pad(&line, width.saturating_sub(4) as usize)),
                SetAttribute(Attribute::Reset)
            )?;
        } else {
            queue!(stdout, Print(pad(&line, width.saturating_sub(4) as usize)))?;
        }
        if index == selected && editing {
            let cursor_text = take_chars(&field.value.replace('\n', " "), cursors[index]);
            let cursor_x = x
                + 2
                + display_width(&prefix) as u16
                + display_width(&truncate(&cursor_text, width.saturating_sub(4) as usize)) as u16;
            cursor = Some((cursor_x.min(x + width.saturating_sub(3)), row_y));
        }
    }
    if show_buttons {
        let button_y = y + fields.len() as u16 + 4;
        draw_form_button(
            stdout,
            x + 2,
            button_y,
            t(" Save ", " 保存 "),
            selected == fields.len() && !editing,
        )?;
        draw_form_button(
            stdout,
            x + 14,
            button_y,
            t(" Back ", " 返回 "),
            selected == fields.len() + 1 && !editing,
        )?;
    }

    let mode = if editing {
        t(
            "Editing; Enter/Esc finishes editing",
            "编辑中，Enter/Esc 结束编辑",
        )
    } else if show_buttons {
        t(
            "Navigating; Enter selects the current item",
            "导航中，Enter 选择当前项",
        )
    } else {
        t(
            "Navigating; Enter selects the current item; [q]back",
            "导航中，Enter 选择当前项，[q]返回",
        )
    };
    queue!(
        stdout,
        MoveTo(x + 2, y + height.saturating_sub(1)),
        Print(truncate(mode, width.saturating_sub(4) as usize))
    )?;
    if let Some((x, y)) = cursor {
        queue!(stdout, Show, MoveTo(x, y))?;
    } else {
        queue!(stdout, Hide)?;
    }
    stdout.flush()?;
    Ok(())
}

pub(in crate::config_tui) fn field_display_value(field: &Field, reveal_sensitive: bool) -> String {
    if field.dialog_list {
        // 列表式字段没有 $EDITOR;摘要成对数,原始序列化文本不上屏。
        let pairs = crate::persona_hint::parse_dialogs(&field.value).len();
        return if pairs == 0 {
            t("(empty; Enter opens the list)", "(空,回车进列表)").to_string()
        } else if is_zh() {
            format!("[{pairs} 对对话]")
        } else {
            format!("[{pairs} dialog pair(s)]")
        };
    }
    if field.sensitive && !field.value.is_empty() && !reveal_sensitive {
        if field.textarea {
            if is_zh() {
                format!("[已配置 {} 项]", parse_key_list(&field.value).len())
            } else {
                format!("[{} configured]", parse_key_list(&field.value).len())
            }
        } else {
            "********".to_string()
        }
    } else if !field.choices.is_empty() && field.value.is_empty() {
        field.empty_choice_label.to_string()
    } else if !field.choices.is_empty() {
        choice_display_label(
            &field.value,
            field.empty_choice_label,
            field.raw_choice_labels,
        )
    } else if field.boolean {
        match parse_bool_field(&field.value) {
            Ok(value) => boolean_label(value).to_string(),
            Err(_) => field.value.clone(),
        }
    } else if field.modalities {
        parse_modalities(&field.value)
            .iter()
            .map(|value| choice_label(value, ""))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        truncate(&field.value.replace('\n', " "), 70)
    }
}

pub(in crate::config_tui) fn draw_form_button(
    stdout: &mut io::Stdout,
    x: u16,
    y: u16,
    label: &str,
    selected: bool,
) -> Result<()> {
    queue!(stdout, MoveTo(x, y))?;
    if selected {
        queue!(
            stdout,
            SetAttribute(Attribute::Reverse),
            Print(label),
            SetAttribute(Attribute::Reset)
        )?;
    } else {
        queue!(stdout, Print(label))?;
    }
    Ok(())
}

pub(in crate::config_tui) struct Field {
    pub(in crate::config_tui) label: &'static str,
    pub(in crate::config_tui) value: String,
    pub(in crate::config_tui) textarea: bool,
    /// 预设对话列表:Enter 进入列表式子编辑器而不是 $EDITOR(验收 #19),
    /// value 仍是 `user:`/`assistant:` 行格式的序列化文本。
    pub(in crate::config_tui) dialog_list: bool,
    pub(in crate::config_tui) sensitive: bool,
    pub(in crate::config_tui) boolean: bool,
    pub(in crate::config_tui) modalities: bool,
    pub(in crate::config_tui) choices: Vec<String>,
    pub(in crate::config_tui) empty_choice_label: &'static str,
    pub(in crate::config_tui) raw_choice_labels: bool,
}

impl Field {
    pub(in crate::config_tui) fn new(label: &'static str, value: String) -> Self {
        Self {
            label,
            value,
            textarea: false,
            dialog_list: false,
            sensitive: false,
            boolean: false,
            modalities: false,
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
        }
    }

    pub(in crate::config_tui) fn boolean(label: &'static str, value: bool) -> Self {
        Self {
            label,
            value: value.to_string(),
            textarea: false,
            dialog_list: false,
            sensitive: false,
            boolean: true,
            modalities: false,
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
        }
    }

    pub(in crate::config_tui) fn textarea(label: &'static str, value: String) -> Self {
        Self {
            label,
            value,
            textarea: true,
            dialog_list: false,
            sensitive: false,
            boolean: false,
            modalities: false,
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
        }
    }

    pub(in crate::config_tui) fn dialog_list(label: &'static str, value: String) -> Self {
        Self {
            dialog_list: true,
            ..Self::textarea(label, value)
        }
    }

    pub(in crate::config_tui) fn choices(mut self, choices: &[&str]) -> Self {
        self.choices = choices.iter().map(|item| item.to_string()).collect();
        self
    }

    pub(in crate::config_tui) fn sensitive(mut self) -> Self {
        self.sensitive = true;
        self
    }

    pub(in crate::config_tui) fn modalities(label: &'static str, value: String) -> Self {
        Self {
            label,
            value,
            textarea: false,
            dialog_list: false,
            sensitive: false,
            boolean: false,
            modalities: true,
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
        }
    }

    pub(in crate::config_tui) fn choices_owned(mut self, choices: Vec<String>) -> Self {
        self.choices = choices;
        self
    }

    pub(in crate::config_tui) fn empty_choice_label(mut self, label: &'static str) -> Self {
        self.empty_choice_label = label;
        self
    }

    pub(in crate::config_tui) fn raw_choice_labels(mut self) -> Self {
        self.raw_choice_labels = true;
        self
    }
}
