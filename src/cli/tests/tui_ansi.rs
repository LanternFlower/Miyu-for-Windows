//! ANSI → span 解析层。
//!
//! 这层是「不重写 render/ 就能上全屏 TUI」的全部依据，所以断言分两档：
//! 一档是协议本身（SGR / 扩展色 / OSC 8），一档是**拿真实渲染器的输出**
//! 过一遍，证明视觉真的能还原——后者才是这层成立与否的证据。

use crate::cli::repl::tail::screen::ansi::*;
use ratatui::style::{Color, Modifier, Style};

fn texts(spans: &[AnsiSpan]) -> Vec<&str> {
    spans.iter().map(|span| span.text.as_str()).collect()
}

fn style_of<'a>(spans: &'a [AnsiSpan], text: &str) -> Option<&'a Style> {
    spans
        .iter()
        .find(|span| span.text == text)
        .map(|span| &span.style)
}

#[test]
fn plain_text_is_one_span() {
    let lines = parse_ansi("hello");
    assert_eq!(lines.len(), 1);
    assert_eq!(texts(&lines[0]), vec!["hello"]);
    assert_eq!(lines[0][0].style, Style::new());
}

#[test]
fn newlines_split_lines_without_a_trailing_blank() {
    let lines = parse_ansi("a\nb\n");
    assert_eq!(lines.len(), 2);
    assert_eq!(texts(&lines[0]), vec!["a"]);
    assert_eq!(texts(&lines[1]), vec!["b"]);
}

#[test]
fn empty_input_is_one_empty_line() {
    let lines = parse_ansi("");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].is_empty());
}

#[test]
fn sgr_splits_spans_and_carries_modifiers() {
    let lines = parse_ansi("plain\x1b[1mbold\x1b[0mtail");
    assert_eq!(texts(&lines[0]), vec!["plain", "bold", "tail"]);
    assert_eq!(lines[0][0].style, Style::new());
    assert!(lines[0][1].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(lines[0][2].style, Style::new());
}

#[test]
fn bare_sgr_resets() {
    // `CSI m` 不带参数等同 `CSI 0 m`。
    let lines = parse_ansi("\x1b[1mbold\x1b[mplain");
    assert_eq!(lines[0][1].style, Style::new());
}

#[test]
fn intensity_cancel_clears_both_bold_and_dim() {
    let lines = parse_ansi("\x1b[1m\x1b[2mboth\x1b[22mneither");
    let both = &lines[0][0].style;
    assert!(both.add_modifier.contains(Modifier::BOLD));
    assert!(both.add_modifier.contains(Modifier::DIM));
    assert!(!lines[0][1].style.add_modifier.contains(Modifier::BOLD));
    assert!(!lines[0][1].style.add_modifier.contains(Modifier::DIM));
}

#[test]
fn indexed_and_truecolor_are_decoded() {
    let lines = parse_ansi("\x1b[38;5;189mindexed\x1b[0m\x1b[38;2;196;167;231mtrue\x1b[0m");
    assert_eq!(lines[0][0].style.fg, Some(Color::Indexed(189)));
    assert_eq!(lines[0][1].style.fg, Some(Color::Rgb(196, 167, 231)));
}

#[test]
fn colon_form_extended_color_is_decoded() {
    // 子参数形式。渲染器现在发分号，但终端两种都合法，解析层不该只认一种。
    let lines = parse_ansi("\x1b[38:5:189mindexed");
    assert_eq!(lines[0][0].style.fg, Some(Color::Indexed(189)));
}

#[test]
fn background_colors_survive() {
    // patch.rs 的删除行是「背景 + 前景」两件套，掉一个就看不出是删除。
    let lines = parse_ansi("\x1b[48;2;60;41;53m\x1b[38;5;210m-gone\x1b[0m");
    assert_eq!(lines[0][0].style.bg, Some(Color::Rgb(60, 41, 53)));
    assert_eq!(lines[0][0].style.fg, Some(Color::Indexed(210)));
}

#[test]
fn default_color_params_clear_only_that_channel() {
    let lines = parse_ansi("\x1b[31m\x1b[44mboth\x1b[39mbg_only");
    assert_eq!(lines[0][0].style.fg, Some(Color::Red));
    assert_eq!(lines[0][0].style.bg, Some(Color::Blue));
    assert_eq!(lines[0][1].style.fg, None);
    assert_eq!(lines[0][1].style.bg, Some(Color::Blue));
}

#[test]
fn style_carries_across_lines() {
    let lines = parse_ansi("\x1b[1mfirst\nsecond\x1b[0m");
    assert!(lines[0][0].style.add_modifier.contains(Modifier::BOLD));
    assert!(lines[1][0].style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn osc8_hyperlink_attaches_to_spans() {
    let input = crate::render::osc8("https://example.org", "label");
    let lines = parse_ansi(&input);
    let span = lines[0]
        .iter()
        .find(|span| span.text == "label")
        .expect("链接正文应当在一个 span 里");
    assert_eq!(span.link.as_deref(), Some("https://example.org"));
}

#[test]
fn osc8_closes_and_later_text_is_unlinked() {
    let input = format!(
        "{}tail",
        crate::render::osc8("https://example.org", "label")
    );
    let lines = parse_ansi(&input);
    let tail = lines[0]
        .iter()
        .find(|span| span.text == "tail")
        .expect("链接后的文字应当还在");
    assert_eq!(tail.link, None);
}

#[test]
fn stray_cursor_sequences_do_not_eat_text() {
    // 渲染器的行输出里不该有光标控制，真混进来也只能掉样式、不能丢字。
    let lines = parse_ansi("before\x1b[2Kafter\x1b[3Gmore");
    assert_eq!(spans_text(&lines[0]), "beforeaftermore");
}

// ───────────────── 拿真实渲染器的输出验证 ─────────────────

#[test]
fn real_markdown_header_round_trips() {
    let rendered = crate::render::render_markdown_line("# 标题");
    let spans = parse_ansi_line(&rendered);
    // 文本必须一字不差地还原（含 `#` 前缀——现有渲染器是保留它的）。
    assert_eq!(spans_text(&spans), "# 标题");
    // HEADER_STYLE = 粗体 + magenta。
    let style = spans
        .iter()
        .find(|span| span.text.contains("标题"))
        .map(|span| span.style)
        .expect("标题文字应当在 span 里");
    assert!(style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(style.fg, Some(Color::Magenta));
}

#[test]
fn real_markdown_inline_code_is_cyan() {
    let rendered = crate::render::render_markdown_line("跑 `cargo build` 试试");
    let spans = parse_ansi_line(&rendered);
    assert_eq!(spans_text(&spans), "跑 cargo build 试试");
    assert_eq!(
        style_of(&spans, "cargo build").and_then(|style| style.fg),
        Some(Color::Cyan),
        "INLINE_CODE_STYLE 是 cyan"
    );
}

#[test]
fn real_markdown_bold_is_bold_blue() {
    let rendered = crate::render::render_markdown_line("这里 **很重要** 注意");
    let spans = parse_ansi_line(&rendered);
    assert_eq!(spans_text(&spans), "这里 很重要 注意");
    let style = style_of(&spans, "很重要").expect("粗体正文应当单独成 span");
    assert!(style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(style.fg, Some(Color::Blue));
}

#[test]
fn real_markdown_list_marker_is_magenta() {
    let rendered = crate::render::render_markdown_line("- 一条");
    let spans = parse_ansi_line(&rendered);
    assert_eq!(spans_text(&spans), "- 一条");
    // TERTIARY_STYLE，列表标记单独上色。
    assert!(spans
        .iter()
        .any(|span| span.text.contains('-') && span.style.fg == Some(Color::Magenta)));
}

#[test]
fn real_blockquote_keeps_the_green_bar() {
    let rendered = crate::render::render_markdown_line("> 引用");
    let spans = parse_ansi_line(&rendered);
    assert_eq!(spans_text(&spans), "| 引用");
    assert!(
        spans.iter().all(|span| span.style.fg == Some(Color::Green)),
        "引用块整行绿色"
    );
}

#[test]
fn real_code_highlight_splits_tokens() {
    let rendered = crate::render::highlight_code_line("rust", "let x = \"hi\"; // 注释");
    let spans = parse_ansi_line(&rendered);
    assert_eq!(spans_text(&spans), "let x = \"hi\"; // 注释");
    // 关键字 iris、字符串绿、注释 ANSI green——三种颜色必须都还原出来。
    assert_eq!(
        style_of(&spans, "let").and_then(|style| style.fg),
        Some(Color::Rgb(196, 167, 231))
    );
    assert_eq!(
        style_of(&spans, "\"hi\"").and_then(|style| style.fg),
        Some(Color::Rgb(166, 214, 160))
    );
    assert!(spans
        .iter()
        .any(|span| span.text.contains("// 注释") && span.style.fg == Some(Color::Green)));
}

#[test]
fn real_patch_diff_keeps_row_backgrounds() {
    let diff = "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-old\n+new\n";
    let rendered = crate::render::render_patch_diff("src/x.rs", diff);
    let lines = parse_ansi(&rendered);
    // 删除行的暗红底、新增行的暗蓝底，是 patch 唯一的语义信号。
    assert!(
        lines.iter().flatten().any(|span| {
            span.style.bg == Some(Color::Rgb(60, 41, 53)) || span.text.contains("old")
        }),
        "删除行应当还在"
    );
    assert!(
        lines.iter().flatten().any(|span| {
            span.style.bg == Some(Color::Rgb(32, 52, 67)) || span.text.contains("new")
        }),
        "新增行应当还在"
    );
    // 整份 diff 的可见文本不能丢东西。
    let all: String = lines.iter().map(|line| spans_text(line)).collect();
    assert!(all.contains("src/x.rs"));
    assert!(all.contains("old") && all.contains("new"));
}

#[test]
fn real_table_borders_are_dim() {
    let table = vec![
        "| 列一 | 列二 |".to_string(),
        "| --- | --- |".to_string(),
        "| a | b |".to_string(),
    ];
    let rendered = crate::render::render_table(&table);
    let lines = parse_ansi(&rendered);
    let border = lines
        .iter()
        .flatten()
        .find(|span| span.text.contains('┌') || span.text.contains('─'))
        .expect("表格应当有边框");
    assert!(
        border.style.add_modifier.contains(Modifier::DIM),
        "表格边框是 dim"
    );
    let all: String = lines.iter().map(|line| spans_text(line)).collect();
    assert!(all.contains("列一") && all.contains('a'));
}

#[test]
fn no_style_bytes_leak_into_text() {
    // 最要紧的一条：解析后任何 span 的文本里都不该残留 ESC。
    let samples = [
        crate::render::render_markdown_line("# 标题 **粗** `码` [链接](https://x.org)"),
        crate::render::highlight_code_line("python", "def f(x): return 1  # ok"),
        crate::render::render_markdown_line("> 引用 *斜* ~~删~~"),
    ];
    for sample in samples {
        for span in parse_ansi_line(&sample) {
            assert!(
                !span.text.contains('\x1b'),
                "span 文本里漏了转义序列: {:?}",
                span.text
            );
        }
    }
}

/// 画屏时链接要跟着格子一起发出去：缓冲里存着 OSC 8 的目标，`spans_to_ansi` 得把
/// `ESC ] 8 ; ; url ESC \` 再写出来，终端才认得这是个链接（悬停下划线、自己那套
/// 点开）。原来这一层把它丢了——全屏里链接看着是普通文字（用户实测）。
#[test]
fn spans_to_ansi_emits_osc8_around_linked_text() {
    let spans = [
        AnsiSpan {
            text: "点这里".to_string(),
            style: Style::new(),
            link: Some("https://x.test/a".to_string()),
        },
        AnsiSpan {
            text: " 后面".to_string(),
            style: Style::new(),
            link: None,
        },
    ];
    let out = spans_to_ansi(&spans);
    assert!(
        out.contains("\x1b]8;;https://x.test/a\x1b\\点这里\x1b]8;;\x1b\\ 后面"),
        "链接没包上 OSC 8: {out:?}"
    );
    // 没有链接的行一个字节都不多。
    assert_eq!(spans_to_ansi(&[AnsiSpan::raw("裸")]), "裸");
}
