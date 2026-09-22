//! 改窗口宽度时正文要按新宽度重排。
//!
//! 09-18 之前：拉宽窗口，已经落下的行还按旧宽度断着、右边整片空着；收窄，超出
//! 新宽度的那一截直接被画面切掉。这里钉住两件事——**软换行并得回去**、
//! **`\n` 断的行不许并**。

use crate::cli::repl::tail::screen::term::Term;
use miyu_hosts::render::blocks::SOFT_WRAP_MARKER;

fn text(term: &Term) -> Vec<String> {
    (0..term.line_count())
        .map(|row| {
            term.row_spans(row)
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect()
}

fn term_at(cols: usize, bytes: &str) -> Term {
    let mut term = Term::default();
    term.set_cols(cols);
    term.feed(bytes.as_bytes());
    term
}

/// 写到边上自己折的那一下：拉宽了要并回一行。
#[test]
fn a_buffer_wrap_rejoins_when_the_window_gets_wider() {
    let mut term = term_at(10, "abcdefghijklmno");
    assert_eq!(text(&term), vec!["abcdefghij", "klmno"]);

    term.set_cols(20);
    assert_eq!(text(&term), vec!["abcdefghijklmno"], "拉宽后要并回一行");

    term.set_cols(5);
    assert_eq!(
        text(&term),
        vec!["abcde", "fghij", "klmno"],
        "收窄后要重新断，而不是把右边切掉"
    );
}

/// `\n` 断的行是作者自己断的，多宽都不许并。
#[test]
fn hard_newlines_are_never_rejoined() {
    let mut term = term_at(10, "abc\ndef");
    term.set_cols(40);
    assert_eq!(text(&term), vec!["abc", "def"]);
}

/// 渲染器自己折的正文（续行自带装订边那两格）：并的时候摘掉缩进，重排的时候
/// 补回去——不摘的话每折一次就多缩两格。
#[test]
fn renderer_soft_wraps_rejoin_and_keep_the_hanging_indent() {
    let body = format!("  abcdefgh{SOFT_WRAP_MARKER}\n  ijklmnop");
    let mut term = term_at(10, &body);
    assert_eq!(text(&term), vec!["  abcdefgh", "  ijklmnop"]);

    term.set_cols(20);
    assert_eq!(
        text(&term),
        vec!["  abcdefghijklmnop"],
        "并回一行，缩进只留开头那一份"
    );

    term.set_cols(10);
    assert_eq!(
        text(&term),
        vec!["  abcdefgh", "  ijklmnop"],
        "再折回去，续行的装订边要补回来"
    );
}

/// 宽字符按显示宽度断，不能把一个字劈成两半。
#[test]
fn wide_characters_stay_whole_across_a_reflow() {
    let mut term = term_at(10, "中文中文中文");
    assert_eq!(text(&term), vec!["中文中文中", "文"]);

    term.set_cols(12);
    assert_eq!(text(&term), vec!["中文中文中文"]);
}

/// 重排之后光标还落在同一个字上：接着写的内容得接在原处，不能跑到别的行去。
#[test]
fn the_cursor_follows_its_character_through_a_reflow() {
    let mut term = term_at(10, "abcdefghijkl");
    term.set_cols(20);
    term.feed(b"XY");
    assert_eq!(text(&term), vec!["abcdefghijklXY"]);
}

/// 轮标记是 `/undo` 截缓冲的依据，行号全变了也得跟着搬。
#[test]
fn turn_markers_move_with_their_content() {
    crate::cli::tests::tui_blocks::with_blocks(|| {
        let marker = miyu_hosts::render::blocks::TURN_START_MARKER;
        let mut term = term_at(10, &format!("aaaaaaaaaaaaaaa\n{marker}bbb"));
        assert_eq!(term.turn_starts(), &[2]);

        term.set_cols(30);
        assert_eq!(
            term.turn_starts(),
            &[1],
            "前面那段并成一行之后，轮标记跟着往上挪一行"
        );
    });
}

/// 端到端：真渲染器吐出来的正文，进了缓冲之后也要能跟着窗口重排。
///
/// 单测里那几条钉的是缓冲自己的账；这一条钉的是**渲染器有没有把「这处是折
/// 出来的」告诉缓冲**——不告诉的话缓冲只看到一串 `\n`，并不回去。
#[test]
fn a_rendered_body_reflows_with_the_window() {
    use miyu_core::llm::{ChatStreamChunk, ChatStreamKind};
    use miyu_hosts::render::{ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode};

    crate::cli::tests::tui_blocks::with_blocks(|| {
        miyu_hosts::render::set_cols_override(40);
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            4,
        );
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        renderer.use_terminal_surface();
        assert!(renderer.caps().expandable, "该走能点开的那个面");
        renderer
            .write_chunk(ChatStreamChunk {
                kind: ChatStreamKind::Content,
                text: "长正文".repeat(30) + "\n",
            })
            .unwrap();
        let frame = renderer.take_output_frame();
        miyu_hosts::render::set_cols_override(0);

        let mut term = Term::default();
        term.set_cols(40);
        term.feed(&frame);
        let narrow = text(&term)
            .into_iter()
            .filter(|line| line.contains("长正文"))
            .count();
        assert!(narrow >= 3, "40 列下该折成好几行，实际 {narrow} 行");

        term.set_cols(120);
        let wide = text(&term)
            .into_iter()
            .filter(|line| line.contains("长正文"))
            .count();
        assert!(
            wide < narrow,
            "拉宽之后行数该变少（{narrow} → {wide}），说明并回去重排了"
        );
    });
}

/// 和上一条一样，只是正文**没有结尾换行**——桩模型和真模型都这么发，markdown
/// 流会把整段攒着、最后由 `finish` 一次性吐出来，走的是另一条落地路径。
#[test]
fn a_body_without_a_trailing_newline_also_reflows() {
    use miyu_core::llm::{ChatStreamChunk, ChatStreamKind};
    use miyu_hosts::render::{ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode};

    crate::cli::tests::tui_blocks::with_blocks(|| {
        miyu_hosts::render::set_cols_override(40);
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            4,
        );
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        renderer.use_terminal_surface();
        for _ in 0..30 {
            renderer
                .write_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Content,
                    text: "长正文".to_string(),
                })
                .unwrap();
        }
        renderer.finish().unwrap();
        let frame = renderer.take_output_frame();
        miyu_hosts::render::set_cols_override(0);

        let mut term = Term::default();
        term.set_cols(40);
        term.feed(&frame);
        let narrow = text(&term)
            .into_iter()
            .filter(|line| line.contains("长正文"))
            .count();
        term.set_cols(120);
        let wide = text(&term)
            .into_iter()
            .filter(|line| line.contains("长正文"))
            .count();
        assert!(narrow >= 3, "40 列下该折成好几行，实际 {narrow} 行");
        assert!(wide < narrow, "拉宽之后行数该变少（{narrow} → {wide}）");
    });
}

/// inline（非全屏）下字节流里一个标记都不许有：全屏是可选项，为它往所有人的
/// 终端里塞标记是不能接受的。
#[test]
fn inline_bodies_carry_no_soft_wrap_markers() {
    miyu_hosts::render::blocks::set_enabled(false);
    miyu_hosts::render::set_cols_override(40);
    let wrapped = miyu_hosts::render::timeline::indent_body(&"长正文".repeat(30));
    miyu_hosts::render::set_cols_override(0);
    assert!(
        wrapped.contains('\n'),
        "这段该被折成好几行，否则这条测试什么都没验到"
    );
    assert!(
        !wrapped.contains(SOFT_WRAP_MARKER),
        "inline 的字节流里混进了软换行标记"
    );
}
