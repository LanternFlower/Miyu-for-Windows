//! 补丁与 diff 的渲染。
//!
//! 只认 unified diff 的 hunk 头（`parse_diff_hunk_header`），解析不出来就原样
//! 打——工具产出的 diff 格式不总是规范的，猜错不如不猜。

use crate::render::*;

pub(crate) fn write_tool_payload(stdout: &mut impl Write, label: &str, payload: &str) -> Result<()> {
    let formatted = format_tool_payload(payload);
    writeln!(stdout, "\x1b[2m{label}:\x1b[0m")?;
    for line in formatted.lines() {
        writeln!(stdout, "\x1b[2m  {line}\x1b[0m")?;
    }
    Ok(())
}

pub(crate) fn write_patch_result(stdout: &mut impl Write, output: &str) -> Result<bool> {
    let Ok(value) = serde_json::from_str::<Value>(output.trim()) else {
        return Ok(false);
    };
    let path = value.get("path").and_then(Value::as_str).unwrap_or("file");
    let diff = value.get("diff").and_then(Value::as_str).unwrap_or("");
    if diff.trim().is_empty() {
        return Ok(false);
    }
    write!(stdout, "{}", render_patch_diff(path, diff))?;
    Ok(true)
}

pub(crate) fn render_patch_diff(path: &str, diff: &str) -> String {
    let mut output = String::new();
    // apply_patch 是唯一编辑器(增/改/删同一语义),标签按 diff 形态区分:
    // 纯 + 无上下文=新建,纯 - 无上下文=删除,其余=修改。
    let mut plus = false;
    let mut minus = false;
    let mut context = false;
    for line in diff.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") || line.starts_with("@@") {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => plus = true,
            Some(b'-') => minus = true,
            Some(_) => context = true,
            None => {}
        }
    }
    let label = if plus && !minus && !context {
        t("Created", "已新建")
    } else if minus && !plus && !context {
        t("Deleted", "已删除")
    } else {
        t("Modified", "已修改")
    };
    output.push_str(&format!(
        "\x1b[2m{label}  \x1b[38;5;250m{path}\x1b[0m\n\n"
    ));

    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);

    let mut old_line = 0usize;
    let mut new_line = 0usize;
    for raw_line in diff.lines() {
        if raw_line.starts_with("--- ") || raw_line.starts_with("+++ ") {
            continue;
        }
        if raw_line.starts_with("@@") {
            if let Some((old_start, new_start)) = parse_diff_hunk_header(raw_line) {
                old_line = old_start;
                new_line = new_start;
            }
            if !output.ends_with("\n\n") {
                output.push('\n');
            }
            continue;
        }

        let (line_no, sign, body, style) = if let Some(body) = raw_line.strip_prefix('-') {
            let line_no = old_line;
            old_line += 1;
            (line_no, '-', body, PATCH_DELETE_STYLE)
        } else if let Some(body) = raw_line.strip_prefix('+') {
            let line_no = new_line;
            new_line += 1;
            (line_no, '+', body, PATCH_INSERT_STYLE)
        } else if let Some(body) = raw_line.strip_prefix(' ') {
            let line_no = new_line;
            old_line += 1;
            new_line += 1;
            (line_no, ' ', body, "\x1b[38;5;245m")
        } else {
            (new_line, ' ', raw_line, "\x1b[38;5;245m")
        };

        push_patch_diff_line(&mut output, line_no, sign, body, style, terminal_width);
    }
    output.push('\n');
    output
}

pub(crate) fn push_patch_diff_line(
    output: &mut String,
    line_no: usize,
    sign: char,
    body: &str,
    style: &str,
    terminal_width: usize,
) {
    let first_prefix = format!("\x1b[38;5;102m{line_no:>5}\x1b[0m {style}{sign} │ ");
    let continuation_prefix = format!("\x1b[38;5;102m     \x1b[0m {style}  │ ");
    let prefix_width = visible_width(&first_prefix);
    let body_width = terminal_width.saturating_sub(prefix_width + 1).max(1);
    let wrapped = wrap_ansi_text(body, body_width);

    for (index, segment) in wrapped.iter().enumerate() {
        if index == 0 {
            output.push_str(&first_prefix);
        } else {
            output.push_str(&continuation_prefix);
        }
        output.push_str(segment);
        output.push_str("\x1b[0m\n");
    }
}

pub(crate) fn parse_diff_hunk_header(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.split_whitespace();
    parts.next()?;
    let old_part = parts.next()?.trim_start_matches('-');
    let new_part = parts.next()?.trim_start_matches('+');
    Some((
        parse_diff_range_start(old_part)?,
        parse_diff_range_start(new_part)?,
    ))
}

pub(crate) fn parse_diff_range_start(value: &str) -> Option<usize> {
    value.split(',').next()?.parse().ok()
}

pub(crate) fn format_tool_payload(payload: &str) -> String {
    let text = payload.trim();
    let formatted = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.to_string());
    truncate_chars(&formatted, 2400)
}
