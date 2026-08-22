//! 行内选择器：模糊筛选与单选。
//!
//! 「行内」指的是不进全屏备用缓冲区——终端里已有的输出要留着。所以要自己算占
//! 几行、画完擦干净（`clear_inline_fuzzy`），`InlineRawMode` 的 `Drop` 保证异常
//! 退出时也能把终端还回去。

use crate::cli::*;

pub(in crate::cli) fn fuzzy_matches(matcher: &SkimMatcherV2, items: &[String], query: &str) -> Vec<(i64, usize)> {
    let mut matches = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            if query.trim().is_empty() {
                Some((0, index))
            } else {
                matcher.fuzzy_match(item, query).map(|score| (score, index))
            }
        })
        .collect::<Vec<_>>();
    if !query.trim().is_empty() {
        matches.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    }
    matches
}

pub(in crate::cli) fn draw_inline_fuzzy(
    stdout: &mut io::Stdout,
    anchor_y: u16,
    menu_lines: u16,
    query: &str,
    items: &[String],
    matches: &[(i64, usize)],
    selected: usize,
    scroll: usize,
    active: &[bool],
) -> Result<()> {
    let (cols, _) = terminal::size().unwrap_or((80, 24));
    let bar = inline_fuzzy_bar();
    let width = (cols as usize).saturating_sub(visible_width(&bar)).max(1);
    let visible = matches.len().min(menu_lines.saturating_sub(2) as usize);
    queue!(stdout, Hide)?;
    for row in 0..menu_lines {
        queue!(
            stdout,
            MoveTo(0, anchor_y + row),
            Clear(ClearType::CurrentLine)
        )?;
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y),
        Print(&bar),
        Print(inline_fuzzy_header(query, width)),
    )?;
    if matches.is_empty() {
        queue!(
            stdout,
            MoveTo(0, anchor_y + 1),
            Print(&bar),
            Print(format!("\x1b[2m{}\x1b[0m", t("no matches", "没有匹配项")))
        )?;
    } else {
        for (row, (_, item_index)) in matches.iter().skip(scroll).take(visible).enumerate() {
            queue!(
                stdout,
                MoveTo(0, anchor_y + row as u16 + 1),
                Print(&bar),
                Print(inline_fuzzy_item_line(
                    items[*item_index].as_str(),
                    scroll + row == selected,
                    active.get(*item_index).copied().unwrap_or(false),
                    width
                ))
            )?;
        }
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y + menu_lines.saturating_sub(1)),
        Print(&bar),
        Print(inline_fuzzy_help_line(width))
    )?;
    stdout.flush()?;
    Ok(())
}

pub(in crate::cli) fn inline_fuzzy_scroll(selected: usize, scroll: usize, visible: usize) -> usize {
    if visible == 0 || selected < scroll {
        selected
    } else if selected >= scroll + visible {
        selected + 1 - visible
    } else {
        scroll
    }
}

pub(in crate::cli) fn inline_fuzzy_bar() -> String {
    input_prompt_bar(AgentMode::Normal)
}

pub(in crate::cli) fn inline_fuzzy_header(query: &str, width: usize) -> String {
    let title = t("Select model", "选择模型");
    let line = if query.trim().is_empty() {
        title.to_string()
    } else {
        format!("{title} · {}", query.trim())
    };
    format!("\x1b[1m{}\x1b[0m", truncate_visible_width(&line, width))
}

pub(in crate::cli) fn inline_fuzzy_item_line(item: &str, selected: bool, active: bool, width: usize) -> String {
    let marker = if active { "[*]" } else { "[ ]" };
    let line = if selected {
        format!("› {marker} {item}")
    } else {
        format!("  {marker} {item}")
    };
    let line = truncate_visible_width(&line, width);
    if selected {
        format!(
            "\x1b[1m\x1b[35m›\x1b[0m\x1b[1m{}\x1b[0m",
            line.strip_prefix('›').unwrap_or(&line)
        )
    } else if active {
        format!("\x1b[1m\x1b[32m{}\x1b[0m", line)
    } else {
        format!("\x1b[2m{}\x1b[0m", line)
    }
}

pub(in crate::cli) fn inline_fuzzy_help_line(width: usize) -> String {
    let line = t(
        "type search · j/k move · Tab toggle · Enter/q confirm",
        "输入搜索 · j/k 移动 · Enter 选定 · Tab 多选 · q 完成",
    );
    format!("\x1b[2m{}\x1b[0m", truncate_visible_width(line, width))
}

pub(in crate::cli) fn clear_inline_fuzzy(stdout: &mut io::Stdout, anchor_y: u16, lines: u16) -> Result<()> {
    for row in 0..lines {
        queue!(
            stdout,
            MoveTo(0, anchor_y + row),
            Clear(ClearType::CurrentLine)
        )?;
    }
    queue!(stdout, MoveTo(0, anchor_y), Show)?;
    stdout.flush()?;
    Ok(())
}

pub(in crate::cli) fn reserve_inline_fuzzy_space(lines: u16) -> Result<()> {
    for _ in 1..lines {
        println!();
    }
    io::stdout().flush()?;
    Ok(())
}

pub(in crate::cli) fn inline_fuzzy_lines(item_count: usize) -> u16 {
    ((item_count.min(10) + 2) as u16).max(3)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::cli) fn draw_inline_single(
    stdout: &mut io::Stdout,
    anchor_y: u16,
    menu_lines: u16,
    title: &str,
    query: &str,
    lines: &[String],
    matches: &[(i64, usize)],
    selected: usize,
    scroll: usize,
    deletable: bool,
    confirm_label: Option<&str>,
) -> Result<()> {
    let (cols, _) = terminal::size().unwrap_or((80, 24));
    let bar = inline_fuzzy_bar();
    let width = (cols as usize).saturating_sub(visible_width(&bar)).max(1);
    let visible = matches.len().min(menu_lines.saturating_sub(2) as usize);
    queue!(stdout, Hide)?;
    for row in 0..menu_lines {
        queue!(
            stdout,
            MoveTo(0, anchor_y + row),
            Clear(ClearType::CurrentLine)
        )?;
    }
    let header = match confirm_label {
        Some(label) => inline_single_confirm_header(label, width),
        None => inline_single_header(title, query, width),
    };
    queue!(stdout, MoveTo(0, anchor_y), Print(&bar), Print(header))?;
    if matches.is_empty() {
        queue!(
            stdout,
            MoveTo(0, anchor_y + 1),
            Print(&bar),
            Print(format!("\x1b[2m{}\x1b[0m", t("no matches", "没有匹配项")))
        )?;
    } else {
        for (row, (_, item_index)) in matches.iter().skip(scroll).take(visible).enumerate() {
            queue!(
                stdout,
                MoveTo(0, anchor_y + row as u16 + 1),
                Print(&bar),
                Print(inline_single_item_line(
                    lines[*item_index].as_str(),
                    scroll + row == selected,
                    width
                ))
            )?;
        }
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y + menu_lines.saturating_sub(1)),
        Print(&bar),
        Print(inline_single_help_line(width, deletable))
    )?;
    stdout.flush()?;
    Ok(())
}

pub(in crate::cli) fn inline_single_header(title: &str, query: &str, width: usize) -> String {
    let line = if query.trim().is_empty() {
        title.to_string()
    } else {
        format!("{title} · {}", query.trim())
    };
    format!("\x1b[1m{}\x1b[0m", truncate_visible_width(&line, width))
}

pub(in crate::cli) fn inline_single_item_line(item: &str, selected: bool, width: usize) -> String {
    let line = if selected {
        format!("› {item}")
    } else {
        format!("  {item}")
    };
    let line = truncate_visible_width(&line, width);
    if selected {
        format!(
            "\x1b[1m\x1b[35m›\x1b[0m\x1b[1m{}\x1b[0m",
            line.strip_prefix('›').unwrap_or(&line)
        )
    } else {
        format!("\x1b[2m{line}\x1b[0m")
    }
}

pub(in crate::cli) fn inline_single_confirm_header(label: &str, width: usize) -> String {
    let line = if is_zh() {
        format!("删除「{label}」？y/N")
    } else {
        format!("delete \"{label}\"? y/N")
    };
    format!("\x1b[1m\x1b[31m{}\x1b[0m", truncate_visible_width(&line, width))
}

pub(in crate::cli) fn inline_single_help_line(width: usize, deletable: bool) -> String {
    let line = if deletable {
        t(
            "type search · j/k move · Enter select · Ctrl+D delete · Esc cancel",
            "输入搜索 · j/k 移动 · Enter 选择 · Ctrl+D 删除 · Esc 取消",
        )
    } else {
        t(
            "type search · j/k move · Enter select · Esc cancel",
            "输入搜索 · j/k 移动 · Enter 选择 · Esc 取消",
        )
    };
    format!("\x1b[2m{}\x1b[0m", truncate_visible_width(line, width))
}

pub(in crate::cli) fn truncate_display(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        format!(
            "{}…",
            value
                .chars()
                .take(max.saturating_sub(1))
                .collect::<String>()
        )
    }
}

pub(in crate::cli) struct InlineRawMode {
    pub(in crate::cli) stdout: io::Stdout,
}

impl InlineRawMode {
    pub(in crate::cli) fn start() -> Result<Self> {
        terminal::enable_raw_mode()?;
    spawn_hangup_watchdog();
        Ok(Self {
            stdout: io::stdout(),
        })
    }
}

impl Drop for InlineRawMode {
    fn drop(&mut self) {
        let _ = execute!(self.stdout, Show);
        let _ = terminal::disable_raw_mode();
    }
}
