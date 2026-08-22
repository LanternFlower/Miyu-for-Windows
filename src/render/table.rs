//! Markdown 表格的渲染。
//!
//! 列宽要按**显示宽度**算（`char_display_width`）：中文双宽、emoji 更宽，按字
//! 符数算的表格一遇中文就散架。
//!
//! `bounded_table_widths` 在终端放不下时收窄列宽，但保底
//! （`readable_table_min_width`）——压到两三个字符的表格不如不画。

use crate::render::*;

pub(crate) struct ActiveTable {
    pub(crate) widths: Vec<usize>,
    pub(crate) alignments: Vec<TableAlign>,
}

pub(crate) fn render_table(lines: &[String]) -> String {
    render_table_with_header_style(lines, true)
}

pub(crate) fn render_table_with_header_style(lines: &[String], bold_header: bool) -> String {
    let alignments = lines
        .get(1)
        .filter(|line| is_table_separator(line))
        .map(|line| parse_table_alignments(line))
        .unwrap_or_default();
    let rows = lines
        .iter()
        .filter(|line| !is_table_separator(line))
        .map(|line| {
            line.trim()
                .trim_matches('|')
                .split('|')
                .map(|cell| render_inline(cell.trim()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let widths = table_widths_for_rows(&rows);
    let mut output = String::new();
    output.push_str(&top_table_border(&widths));
    for (row_index, row) in rows.iter().enumerate() {
        output.push_str(&render_table_row(
            row,
            &widths,
            &alignments,
            bold_header && row_index == 0,
        ));
        if row_index + 1 < rows.len() {
            output.push_str(&middle_table_border(&widths));
        }
    }
    output.push_str(&bottom_table_border(&widths));
    output
}

pub(crate) fn parse_table_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| render_table_cell(cell.trim()))
        .collect()
}

/// 表格单元格:整格为一条公式($…$ / \(…\) 完整包裹)时走二维转写,
/// 分式排成真正的上下结构(多行格);其余走常规行内渲染。
pub(crate) fn render_table_cell(cell: &str) -> String {
    let tex = cell
        .strip_prefix('$')
        .and_then(|rest| rest.strip_suffix('$'))
        .filter(|inner| !inner.is_empty() && !inner.contains('$'))
        .or_else(|| {
            cell.strip_prefix("\\(")
                .and_then(|rest| rest.strip_suffix("\\)"))
        });
    if let Some(tex) = tex {
        if !tex.trim().is_empty() {
            let lines = math::unicode_math_lines(tex);
            let styled = lines
                .iter()
                .map(|line| format!("{PRIMARY_STYLE}{line}{RESET}"))
                .collect::<Vec<_>>();
            return styled.join("\n");
        }
    }
    render_inline(cell)
}

pub(crate) fn table_widths_for_rows(rows: &[Vec<String>]) -> Vec<usize> {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            // 多行格(二维公式)取最宽一行。
            let cell_width = cell.split('\n').map(visible_width).max().unwrap_or(0);
            widths[index] = widths[index].max(cell_width);
        }
    }
    let readable_min = readable_table_min_width(cols);
    for width in &mut widths {
        *width = (*width).max(readable_min);
    }
    bounded_table_widths(widths)
}

pub(crate) fn readable_table_min_width(cols: usize) -> usize {
    match cols {
        0 => 0,
        1 => 16,
        2 => 14,
        3 | 4 => 10,
        _ => 8,
    }
}

pub(crate) fn render_table_row(
    row: &[String],
    widths: &[usize],
    alignments: &[TableAlign],
    header: bool,
) -> String {
    let wrapped = widths
        .iter()
        .enumerate()
        .map(|(index, width)| {
            let cell = row.get(index).map(String::as_str).unwrap_or("");
            cell.split('\n')
                .flat_map(|part| wrap_ansi_text(part, *width))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let row_height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    let mut output = String::new();
    for line_index in 0..row_height {
        push_table_vertical(&mut output);
        for (index, width) in widths.iter().enumerate() {
            let cell = wrapped
                .get(index)
                .and_then(|lines| lines.get(line_index))
                .map(String::as_str)
                .unwrap_or("");
            let cell = if header && !cell.is_empty() {
                format!("{BOLD_STYLE}{cell}{RESET}")
            } else {
                cell.to_string()
            };
            output.push(' ');
            output.push_str(&aligned_cell(
                &cell,
                *width,
                alignments.get(index).copied().unwrap_or(TableAlign::Left),
            ));
            output.push(' ');
            push_table_vertical(&mut output);
        }
        output.push('\n');
    }
    output
}

pub(crate) fn top_table_border(widths: &[usize]) -> String {
    table_border(widths, '┌', '┬', '┐')
}

pub(crate) fn middle_table_border(widths: &[usize]) -> String {
    table_border(widths, '├', '┼', '┤')
}

pub(crate) fn bottom_table_border(widths: &[usize]) -> String {
    table_border(widths, '└', '┴', '┘')
}

pub(crate) fn bounded_table_widths(mut widths: Vec<usize>) -> Vec<usize> {
    if widths.is_empty() {
        return widths;
    }
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100)
        .saturating_sub(1)
        .max(20);
    let border_overhead = widths.len().saturating_mul(3).saturating_add(1);
    let available = terminal_width
        .saturating_sub(border_overhead)
        .max(widths.len());
    while widths.iter().sum::<usize>() > available {
        let Some((index, width)) = widths
            .iter()
            .enumerate()
            .max_by_key(|(_, width)| **width)
            .map(|(index, width)| (index, *width))
        else {
            break;
        };
        if width <= 1 {
            break;
        }
        widths[index] -= 1;
    }
    widths
}

pub(crate) fn wrap_ansi_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            current.push(ch);
            for next in chars.by_ref() {
                current.push(next);
                if next == 'm' {
                    break;
                }
            }
            continue;
        }
        let ch_width = char_display_width(ch);
        if current_width > 0 && current_width + ch_width > width {
            lines.push(current);
            current = String::new();
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }
    lines.push(current);
    lines
}

pub(crate) fn char_display_width(ch: char) -> usize {
    if ch.is_ascii() {
        1
    } else if (ch as u32) >= 0x2e80 {
        2
    } else {
        1
    }
}

#[derive(Clone, Copy)]
pub(crate) enum TableAlign {
    Left,
    Center,
    Right,
}

pub(crate) fn parse_table_alignments(line: &str) -> Vec<TableAlign> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| {
            let cell = cell.trim();
            match (cell.starts_with(':'), cell.ends_with(':')) {
                (true, true) => TableAlign::Center,
                (false, true) => TableAlign::Right,
                _ => TableAlign::Left,
            }
        })
        .collect()
}

pub(crate) fn aligned_cell(cell: &str, width: usize, align: TableAlign) -> String {
    let padding = width.saturating_sub(visible_width(cell));
    match align {
        TableAlign::Left => format!("{cell}{}", " ".repeat(padding)),
        TableAlign::Right => format!("{}{cell}", " ".repeat(padding)),
        TableAlign::Center => {
            let left = padding / 2;
            let right = padding - left;
            format!("{}{cell}{}", " ".repeat(left), " ".repeat(right))
        }
    }
}

pub(crate) fn table_border(widths: &[usize], left: char, mid: char, right: char) -> String {
    let mut output = String::new();
    output.push_str("\x1b[2m");
    output.push(left);
    for (index, width) in widths.iter().enumerate() {
        output.push_str(&"─".repeat(width + 2));
        output.push(if index + 1 == widths.len() {
            right
        } else {
            mid
        });
    }
    output.push_str("\x1b[0m\n");
    output
}

pub(crate) fn push_table_vertical(output: &mut String) {
    output.push_str("\x1b[2m│\x1b[0m");
}

pub(crate) fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim().trim_matches('|').trim();
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|ch| matches!(ch, '-' | ':' | '|' | ' '))
        && trimmed.contains('-')
}

pub(crate) fn looks_like_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 2
}

pub(crate) fn write_todo_table(stdout: &mut impl Write, output: &str) -> Result<bool> {
    let Ok(value) = serde_json::from_str::<Value>(output.trim()) else {
        return Ok(false);
    };
    let Some(todos) = value.get("todos").and_then(Value::as_array) else {
        return Ok(false);
    };

    if todos.is_empty() {
        let lines = vec![
            format!("| {} |", t("Todo List", "任务列表")),
            "|---|".to_string(),
            format!("| {} |", t("empty", "空")),
        ];
        write!(stdout, "{}", render_todo_table(&lines))?;
        return Ok(true);
    }

    let mut lines = vec![
        format!("| {} |", t("Todo List", "任务列表")),
        "|---|".to_string(),
    ];
    for item in todos {
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        let content = item.get("content").and_then(Value::as_str).unwrap_or("");
        let cell = escape_table_cell(content);
        let cell = if status == "in_progress" {
            format!("{TERTIARY_STYLE}{cell}{RESET}")
        } else {
            cell
        };
        lines.push(format!("| {} {} |", todo_status_marker(status), cell));
    }
    write!(stdout, "{}", render_todo_table(&lines))?;
    Ok(true)
}

pub(crate) fn render_todo_table(lines: &[String]) -> String {
    render_table_with_header_style(lines, false)
}

pub(crate) fn todo_status_marker(status: &str) -> &'static str {
    match status {
        "completed" => "[✔]",
        "in_progress" => "[·]",
        "cancelled" => "[×]",
        _ => "[ ]",
    }
}

pub(crate) fn escape_table_cell(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace('\n', " ")
        .trim()
        .to_string()
}
