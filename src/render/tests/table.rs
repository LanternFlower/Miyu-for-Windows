//! 表格与 todo 表。

use crate::render::*;
use super::shared::*;

#[test]
fn buffers_tables_until_non_table_line() {
    let mut renderer = MarkdownStreamRenderer::new();
    assert_eq!(renderer.push("| a | b |\n"), "");
    assert_eq!(renderer.push("| - | - |\n"), "");
    let output = renderer.push("| 1 | 2 |\n");
    assert!(output.contains(&format!("{BOLD_STYLE}a{RESET}")));
    assert!(output.contains("1"));
    assert!(output.contains('┌'));
    assert!(output.contains('┬'));
    assert!(output.contains('├'));
    assert!(output.contains('┼'));
    assert!(output.contains("\x1b[2m│\x1b[0m"));
    assert!(output.contains('─'));
    assert!(!output.contains('+'));
    let output = renderer.push("done\n");
    assert!(output.contains('└'));
    assert!(output.ends_with("done\n"));
}

#[test]
fn short_tables_use_content_width() {
    let output = render_table(&[
        "| 项目 | 内容 |".to_string(),
        "|---|---|".to_string(),
        "| 名字 | 未有 / Miyu |".to_string(),
        "| 年龄 | 18 |".to_string(),
    ]);
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    let widest = output.lines().map(visible_width).max().unwrap_or(0);
    assert!(widest < terminal_width / 2, "table too wide: {widest}");
}

#[test]
fn todo_output_uses_single_column_rendered_table() {
    let output = render_todo_table(&[
        "| #Todo |".to_string(),
        "|---|".to_string(),
        "| [·] 修复 todo 表格渲染 |".to_string(),
        "| [ ] 补充单元测试 |".to_string(),
        "| [✔] 跑 cargo test |".to_string(),
    ]);
    let visible = strip_ansi_for_test(&output);
    assert!(output.contains('┌'));
    assert!(output.contains('├'));
    assert!(output.contains('└'));
    assert!(!output.contains('┬'));
    assert!(!output.contains('┼'));
    assert!(!output.contains('┴'));
    assert!(visible.contains("#Todo"));
    assert!(!output.contains(&format!("{BOLD_STYLE}#Todo{RESET}")));
    assert_eq!(visible.matches('│').count(), 8);
    assert!(visible.contains("[·]"));
    assert!(visible.contains("todo"));
    assert!(visible.contains("[ ]"));
    assert!(visible.contains("[✔]"));
    assert!(!visible.contains("优先级"));
    assert!(!visible.contains("序号"));
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output.lines() {
        assert!(
            visible_width(line) < terminal_width,
            "line too wide: {line}"
        );
    }
}

#[test]
fn todo_status_symbols_contribute_to_table_width() {
    assert_eq!(visible_width("把冰箱门打开"), 12);
    assert_eq!(visible_width("[✔] 把冰箱门打开"), 16);
    assert_eq!(visible_width("[·] 把冰箱门打开"), 16);

    let lines = [
        "| #Todo |".to_string(),
        "|---|".to_string(),
        "| [✔] 把冰箱门打开 |".to_string(),
        "| [·] 把冰箱门关上 |".to_string(),
    ];
    let normal = render_table(&lines);
    let output = render_todo_table(&lines);
    let visible = strip_ansi_for_test(&output);
    assert_eq!(
        visible_width(output.lines().next().unwrap()),
        visible_width(normal.lines().next().unwrap())
    );
    assert!(!output.contains(&format!("{BOLD_STYLE}#Todo{RESET}")));
    assert!(visible.contains("[✔]"));
    assert!(visible.contains("[·]"));
    assert_eq!(visible.lines().filter(|line| line.contains('│')).count(), 3);
}

#[test]
fn wraps_wide_table_cells_to_terminal_width() {
    let output = render_table(&[
        "| 项目 | 内容 |".to_string(),
        "|---|---|".to_string(),
        format!("| 很长 | {} |", "这是一段非常长的内容".repeat(20)),
    ]);
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output.lines() {
        assert!(
            visible_width(line) < terminal_width,
            "line too wide: {line}"
        );
    }
    assert!(output.lines().count() > 5);
}

#[test]
fn many_column_tables_stay_within_terminal_width() {
    let output = render_table(&[
        "| 参数名 | 参数类型 | 默认值 | 是否必填 | 说明 | 取值范围 | 示例值 | 适用版本 | 更新日志 | 备注 |".to_string(),
        "|---|---|---|---|---|---|---|---|---|---|".to_string(),
        "| database_host | string | localhost | 否 | 数据库主机地址 | 合法IP或域名 | 192.168.1.100 | v1.0+ | 无 | 支持IPv6 |".to_string(),
    ]);
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output.lines() {
        assert!(
            visible_width(line) < terminal_width,
            "line too wide: {line}"
        );
    }
}

#[test]
fn supports_table_alignment_markers() {
    let mut renderer = MarkdownStreamRenderer::new();
    let output =
        renderer.push("| left | mid | right |\n| :--- | :---: | ---: |\n| a | b | c |\n");
    let output = format!("{output}{}", renderer.flush());
    assert!(output.contains('┌'));
    assert!(output.contains('│'));
    assert!(!output.contains('+'));
    assert!(!output.contains(":---"));
    assert!(output.contains(&format!("{BOLD_STYLE}left{RESET}")));
}

#[test]
fn does_not_buffer_plain_lines_with_pipes_as_tables() {
    let mut renderer = MarkdownStreamRenderer::new();
    let output = renderer.push("echo hi | wc -l\nnext\n");
    assert!(output.contains("echo hi | wc -l\nnext\n"));
}
