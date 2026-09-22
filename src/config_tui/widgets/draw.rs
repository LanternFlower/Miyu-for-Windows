//! 菜单、并排列表、提示屏的绘制，以及宽度计算。
//!
//! 2026-09-20 起这里不再自己画框：每屏组一份 [`View`] 交给
//! [`miyu_base::terminal::chrome`]，版面和引导（OOBE）同一套。
//!
//! 两处「不动业务代码」的取巧，都写在这儿：
//! - 菜单项是业务拼好的整串（`配置全局文本模型 (当前: Stub / stub-model)`），
//!   [`split_trailing_note`] 把行尾那对括号拆成右列，于是每一行都成了
//!   「名字 + 当前值」两列，几百处 `format!` 一个都不用改。
//! - 帮助文案也是业务拼好的整串（`[j/k]移动 [Enter]选择 [q]返回`），
//!   [`key_bar`] 把它拆成按键条；括号外的前缀（搜索词那种）落到底部那一行。
//!
//! 宽度处理仍是重点：中文是双宽字符，emoji 更宽，`display_width` / `pad` /
//! `truncate` 全部按显示宽度算而不是按字符数——按字符数算会让所有含中文的行
//! 错位。光标移动同理，`byte_index_for_char` 负责在字节与字符之间换算。

use crate::config_tui::*;
use miyu_base::terminal::chrome::{self, ln, nil, Cx, View};
use miyu_base::terminal::palette::{BLUE, CORAL, DIM, GOLD};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 名字那一列最窄/最宽多少。太窄右列会贴脸，太宽右列被挤出屏幕。
pub(in crate::config_tui) const NAME_COL_MIN: usize = 22;
pub(in crate::config_tui) const NAME_COL_MAX: usize = 46;

pub(in crate::config_tui) fn draw_menu(
    ui: &mut Ui,
    title: &str,
    options: &[String],
    selected: usize,
    status: &str,
) -> Result<()> {
    draw_menu_with_editing(ui, title, options, selected, status, None)
}

pub(in crate::config_tui) fn draw_menu_with_editing(
    ui: &mut Ui,
    title: &str,
    options: &[String],
    selected: usize,
    status: &str,
    editing: Option<(usize, &str, &str, usize)>,
) -> Result<()> {
    let cx = ui.cx();
    let theme = ui.theme();
    let split: Vec<(String, String)> = options
        .iter()
        .map(|item| split_trailing_note(item))
        .collect();
    let name_col = name_column(ui, &split);
    let mut body: Vec<Line<'static>> = Vec::with_capacity(options.len());
    let mut caret = None;
    for (index, (name, note)) in split.iter().enumerate() {
        match editing {
            // 正在这一行上打字：整行换成 `标签: 值`，光标落在字里。
            Some((edit_index, label, value, cursor)) if edit_index == index => {
                let prefix = format!("{}{label}: ", theme.cursor());
                caret = Some((
                    body.len(),
                    display_width(&prefix) + display_width(&take_chars(value, cursor)),
                ));
                body.push(cx.select(ln(vec![
                    Span::styled(prefix, theme.fg(BLUE)),
                    Span::raw(value.to_string()),
                ])));
            }
            _ => body.push(cx.row(index == selected, name, note, name_col)),
        }
    }
    let help = if editing.is_some() {
        t(
            "[←→ Home/End]move caret [⏎]save [Esc]cancel",
            "[←→ Home/End]移动光标 [⏎]保存 [Esc]取消",
        )
    } else {
        menu_help(status)
    };
    let (footer, keys) = key_bar(&cx, help);
    ui.show(
        title,
        View {
            body,
            cursor_row: selected,
            caret,
            footer,
            counter: counter(selected, options.len()),
            keys,
            ..View::default()
        },
    )
}

/// 并排的几列（供应商 | 组织 | 模型）。列与列之间留一格，列宽按权重分。
pub(in crate::config_tui) struct Column<'a> {
    pub(in crate::config_tui) title: &'a str,
    pub(in crate::config_tui) items: &'a [String],
    pub(in crate::config_tui) selected: usize,
    pub(in crate::config_tui) scroll: usize,
    pub(in crate::config_tui) active: bool,
    /// 这一列占总宽的几份。
    pub(in crate::config_tui) weight: usize,
}

pub(in crate::config_tui) fn draw_columns(
    ui: &mut Ui,
    title: &str,
    columns: &[Column],
    help: &str,
    status: &str,
    status_error: bool,
) -> Result<()> {
    let cx = ui.cx();
    let theme = ui.theme();
    let total_w = ui.body_width();
    let active = columns.iter().find(|column| column.active);
    let counter_text = active.and_then(|column| counter(column.selected, column.items.len()));
    let (prefix, keys) = key_bar(&cx, help);
    // 出错了要显眼：金色是「说一声」的颜色，报错得用暖红。
    let footer = if status.trim().is_empty() {
        prefix
    } else {
        wrapped(
            &cx,
            status.trim(),
            if status_error {
                theme.fg(CORAL)
            } else {
                theme.fg(GOLD)
            },
        )
    };
    // 行数必须跟调用方分页用的是同一个数（`column_visible_rows`），否则光标
    // 走到底时这一列会被截掉几行。第一帧还没有「上一帧」可问，所以这里自己估
    // 一次——两边用的是同一个算法（报错折了几行、按键条折了几行都算进去）。
    // 想占多高按最长那一列的条目数来：三列各自滚，行数取它们的最大值。
    let wanted = columns
        .iter()
        .map(|column| column.items.len())
        .max()
        .unwrap_or(0);
    let rows = ui.viewport(2, footer.len(), &keys, counter_text.as_deref(), wanted);
    let gaps = columns.len().saturating_sub(1);
    let weight_sum: usize = columns
        .iter()
        .map(|column| column.weight)
        .sum::<usize>()
        .max(1);
    let usable = total_w.saturating_sub(gaps);
    let widths: Vec<usize> = columns
        .iter()
        .map(|column| (usable * column.weight / weight_sum).max(8))
        .collect();

    // 表头钉住：列表一长，「哪一列是模型」跟着滚走就没人知道在选什么。
    let mut header: Vec<Span<'static>> = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            header.push(Span::raw(" "));
        }
        let style = if column.active {
            theme.fg(BLUE).add_modifier(Modifier::BOLD)
        } else {
            theme.dim(DIM)
        };
        header.push(Span::styled(
            pad(&truncate(column.title.trim(), widths[index]), widths[index]),
            style,
        ));
    }

    let mut body: Vec<Line<'static>> = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (index, column) in columns.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw(" "));
            }
            let width = widths[index];
            let item_index = column.scroll + row;
            let Some(item) = column.items.get(item_index) else {
                spans.push(Span::raw(" ".repeat(width)));
                continue;
            };
            let text = pad(&truncate(item, width), width);
            let style = if item_index == column.selected {
                // 不在这一列时选中行仍要看得见（另一列的选择是上下文），
                // 但要比当前列淡。
                if column.active {
                    theme.select(theme.fg(BLUE))
                } else {
                    theme.select(theme.dim(DIM))
                }
            } else if column.active {
                Style::new()
            } else {
                theme.dim(DIM)
            };
            spans.push(Span::styled(text, style));
        }
        body.push(Line::from(spans));
    }

    ui.show(
        title,
        View {
            sticky: vec![Line::from(header), nil()],
            body,
            counter: counter_text,
            footer,
            keys,
            ..View::default()
        },
    )
}

/// 一屏提示，按任意键继续。标题留空：这只是当前这层上的一句话，不进面包屑。
pub(in crate::config_tui) fn message(ui: &mut Ui, text: &str) -> Result<()> {
    let cx = ui.cx();
    wait_for_key(ui, move |ui| {
        let mut body: Vec<Line<'static>> = chrome::wrap(text, chrome::body_w())
            .into_iter()
            .map(|line| cx.txt(line, Style::new()))
            .collect();
        body.push(nil());
        ui.show(
            "",
            View {
                body,
                keys: vec![(
                    t("any key", "任意键").to_string(),
                    t("continue", "继续").to_string(),
                )],
                ..View::default()
            },
        )
    })
}

/// 菜单项拆成「名字 + 右列」：行尾那对括号里的东西是当前值/状态，本来就是
/// 说明，摆到右边比挤在名字后面好读。拆不出来就整行当名字。
pub(in crate::config_tui) fn split_trailing_note(option: &str) -> (String, String) {
    let trimmed = option.trim_end();
    let Some(rest) = trimmed
        .strip_suffix(')')
        .or_else(|| trimmed.strip_suffix('）'))
    else {
        return (option.to_string(), String::new());
    };
    let Some(open) = rest.rfind(['(', '（']) else {
        return (option.to_string(), String::new());
    };
    let name = rest[..open].trim_end();
    if name.is_empty() {
        return (option.to_string(), String::new());
    }
    let note = rest[open + rest[open..].chars().next().map_or(1, char::len_utf8)..].trim();
    (name.to_string(), note.to_string())
}

/// 名字那一列多宽：按最长的名字来，夹在上下限之间。
fn name_column(ui: &Ui, split: &[(String, String)]) -> usize {
    let longest = split
        .iter()
        .filter(|(_, note)| !note.is_empty())
        .map(|(name, _)| display_width(name) + 4)
        .max()
        .unwrap_or(NAME_COL_MIN);
    longest
        .clamp(NAME_COL_MIN, NAME_COL_MAX)
        .min(ui.body_width().saturating_sub(12).max(NAME_COL_MIN))
}

/// 一段话按正文宽折成几行。长报错、长搜索提示都走它——底下那一行塞不下就
/// 该换行，而不是让它跑出屏幕（2026-09-20 用户报的：401 的整段 JSON 被截在
/// 屏幕右边）。
fn wrapped(cx: &Cx, text: &str, style: Style) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    chrome::wrap(text, chrome::body_w())
        .into_iter()
        .map(|line| cx.txt(line, style))
        .collect()
}

fn counter(selected: usize, total: usize) -> Option<String> {
    (total > 0).then(|| format!("{}/{total}", (selected + 1).min(total)))
}

/// 把业务拼好的帮助串拆成按键条。`[键]说明 [键]说明` → 一对一对；括号外的
/// 前缀（`搜索: foo_` 那种）不是按键，落到横线上方那一行。
pub(in crate::config_tui) fn key_bar(
    cx: &Cx,
    help: &str,
) -> (Vec<Line<'static>>, Vec<(String, String)>) {
    let mut keys = Vec::new();
    let mut prefix = String::new();
    let mut rest = help;
    while let Some(open) = rest.find('[') {
        let head = &rest[..open];
        if keys.is_empty() {
            prefix.push_str(head);
        }
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        let key = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let label_end = rest.find('[').unwrap_or(rest.len());
        let label = rest[..label_end].trim();
        keys.push((key.trim().to_string(), label.to_string()));
        rest = &rest[label_end..];
    }
    if keys.is_empty() {
        // 一个 `[键]` 都没有：整条当说明，别把它丢了。
        prefix = help.to_string();
    }
    (wrapped(cx, prefix.trim(), cx.theme.fg(GOLD)), keys)
}

pub(in crate::config_tui) fn menu_help(status: &str) -> &str {
    if status.is_empty() {
        t(
            "[↑↓ jk]move [⏎]select [Esc]back",
            "[↑↓ jk]移动 [⏎]选择 [Esc]返回",
        )
    } else {
        status
    }
}

/// 并排列表每列能放几行。跟 [`draw_columns`] 用同一个数：上一帧正文视口有多高。
pub(in crate::config_tui) fn column_visible_rows() -> usize {
    chrome::viewport_rows()
}

pub(in crate::config_tui) fn column_scroll(
    selected: usize,
    scroll: usize,
    visible_rows: usize,
) -> usize {
    if visible_rows == 0 {
        return 0;
    }
    if selected < scroll {
        selected
    } else if selected >= scroll + visible_rows {
        selected + 1 - visible_rows
    } else {
        scroll
    }
}

pub(in crate::config_tui) fn insert_char_at_cursor(
    value: &mut String,
    cursor: &mut usize,
    ch: char,
) {
    let byte_index = byte_index_for_char(value, *cursor);
    value.insert(byte_index, ch);
    *cursor += 1;
}

pub(in crate::config_tui) fn remove_char_before_cursor(value: &mut String, cursor: &mut usize) {
    let end = byte_index_for_char(value, *cursor);
    let start = byte_index_for_char(value, cursor.saturating_sub(1));
    value.replace_range(start..end, "");
    *cursor -= 1;
}

pub(in crate::config_tui) fn remove_char_at_cursor(value: &mut String, cursor: usize) {
    if cursor >= value.chars().count() {
        return;
    }
    let start = byte_index_for_char(value, cursor);
    let end = byte_index_for_char(value, cursor + 1);
    value.replace_range(start..end, "");
}

pub(in crate::config_tui) fn byte_index_for_char(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

pub(in crate::config_tui) fn take_chars(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}

pub(in crate::config_tui) fn active_label(config: &AppConfig) -> String {
    match config.active_provider_model_choices().as_slice() {
        [] => t("Not configured", "未配置").to_string(),
        [choice] => format!("{} / {}", choice.provider_name, choice.model),
        _ => t("Mixed", "混合").to_string(),
    }
}

pub(in crate::config_tui) fn truncate(value: &str, max: usize) -> String {
    if display_width(value) <= max {
        return value.to_string();
    }
    let mut width = 0usize;
    let mut output = String::new();
    let ellipsis_width = 1usize;
    for ch in value.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + char_width + ellipsis_width > max {
            break;
        }
        output.push(ch);
        width += char_width;
    }
    output.push('…');
    output
}

/// 一段文字占几列。
///
/// 2026-09-20 从自己手写的 CJK 区间表换成 `unicode-width`：版面现在由
/// [`miyu_base::terminal::chrome`] 排，它补白用的就是这一套；两边各算各的话，
/// emoji（手写那份按 1 列算，实际占 2 列）会让整行错开一格。
pub(in crate::config_tui) fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

pub(in crate::config_tui) fn pad(value: &str, width: usize) -> String {
    let value = truncate(value, width);
    let len = display_width(&value);
    if len >= width {
        value
    } else {
        format!("{value}{}", " ".repeat(width - len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miyu_base::terminal::palette::{Depth, Theme};

    fn cx() -> Cx {
        Cx::new(Theme {
            depth: Depth::True,
            ascii: false,
        })
    }

    #[test]
    fn trailing_parenthesis_becomes_the_right_column() {
        assert_eq!(
            split_trailing_note("配置全局文本模型 (当前: Stub / stub-model)"),
            ("配置全局文本模型".into(), "当前: Stub / stub-model".into())
        );
        assert_eq!(
            split_trailing_note("接入通讯平台 (未启用)"),
            ("接入通讯平台".into(), "未启用".into())
        );
        // 没括号、只有括号、括号在中间：原样当名字，不瞎拆。
        assert_eq!(
            split_trailing_note("供应商和模型"),
            ("供应商和模型".into(), String::new())
        );
        assert_eq!(split_trailing_note("(空)"), ("(空)".into(), String::new()));
        assert_eq!(
            split_trailing_note("A (B) C"),
            ("A (B) C".into(), String::new())
        );
    }

    #[test]
    fn help_text_becomes_key_pairs() {
        let (footer, keys) = key_bar(&cx(), "[j/k]移动 [Enter]选择 [q]返回");
        assert!(footer.is_empty());
        assert_eq!(
            keys,
            vec![
                ("j/k".to_string(), "移动".to_string()),
                ("Enter".to_string(), "选择".to_string()),
                ("q".to_string(), "返回".to_string()),
            ]
        );
    }

    #[test]
    fn text_before_the_first_key_lands_in_the_footer() {
        let (footer, keys) = key_bar(&cx(), "搜索: gpt_  [Enter]确认 [Esc]取消");
        assert_eq!(
            footer
                .iter()
                .map(|line| line
                    .spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>())
                .collect::<Vec<_>>(),
            vec!["搜索: gpt_".to_string()]
        );
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn help_without_brackets_is_kept_whole() {
        let (footer, keys) = key_bar(&cx(), "导航中，Enter 选择当前项");
        assert!(keys.is_empty());
        assert!(!footer.is_empty());
    }
}
