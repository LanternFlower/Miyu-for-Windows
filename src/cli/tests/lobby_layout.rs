//! 空会话大厅的输入框几何：窄框宽度、折行、光标、旧活动区清理。
//!
//! 这一组盯的是同一件事——**测量、绘制、光标、清除必须用同一个宽度**。
//! 大厅里输入框只有终端的三分之二宽，以前测行数和算光标各自去问终端宽度，
//! 于是「字按 80 列折了行，光标按 120 列停在右边空地上」。

use crate::cli::repl::editor::{repl_input_lines, repl_input_rendered_rows};
use crate::cli::repl::input_layout::*;
use crate::cli::repl::layout::{
    repl_cursor_position_for_cols, repl_move_cursor_vertical_for_cols,
    repl_wrapped_input_rows_for_cols,
};
use crate::cli::repl::width::visible_width;

/// 120 列终端下的窄框：2/3 = 80 列，居中。
const TERMINAL_COLS: usize = 120;
const ART_COLS: usize = 30;

fn lobby_box() -> EditorBox {
    EditorBox::lobby(TERMINAL_COLS, ART_COLS)
}

/// 宽度 90 列的中文输入：窄框（内容宽 78）放不下，整终端宽（内容宽 118）放得下。
fn overflowing_input() -> String {
    let input = "中".repeat(45);
    assert_eq!(visible_width(&input), 90);
    input
}

#[test]
fn lobby_box_is_two_thirds_of_the_terminal_and_centred() {
    let area = lobby_box();

    assert_eq!(area.width, 80);
    assert_eq!(area.left, 20);
}

#[test]
fn lobby_box_never_overflows_a_narrow_terminal() {
    for cols in 1usize..200 {
        let area = EditorBox::lobby(cols, ART_COLS);
        // 至少留得下提示前缀加一个字；窄终端上宽度可以顶到终端本身，但
        // 「左边距 + 宽度」不能把框推出屏幕外。
        assert!(area.width >= 3, "cols={cols} width={}", area.width);
        if area.width <= cols {
            assert!(
                usize::from(area.left) + area.width <= cols,
                "cols={cols} left={} width={}",
                area.left,
                area.width
            );
        }
    }
}

/// 光标的落点必须和**画出来的**折行对得上：折了两行，光标就在第二行上。
#[test]
fn lobby_cursor_matches_narrow_editor_width() {
    let area = lobby_box();
    let input = overflowing_input();
    let end = input.chars().count();

    let rows = repl_wrapped_input_rows_for_cols("  ", &repl_input_lines(&input), area.width);
    let (col, row) = repl_cursor_position_for_cols("  ", &input, end, area.width);

    assert_eq!(rows.len(), 2);
    assert_eq!(row, 1);
    // 第二行上剩 90 - 78 = 12 列，加上两列提示前缀。
    assert_eq!(col, 14);
    assert!(usize::from(col) <= area.width, "光标不能落到窄框外面");
}

/// 同一段输入按整终端宽算出来的是另一套数字——修复前光标走的就是这条路，
/// 算出来的列直接落在窄框右边的星空里（用户截图里那根竖条）。
#[test]
fn terminal_width_cursor_falls_outside_the_lobby_box() {
    let area = lobby_box();
    let input = overflowing_input();
    let end = input.chars().count();

    let (col, row) = repl_cursor_position_for_cols("  ", &input, end, TERMINAL_COLS);

    assert_eq!(row, 0);
    assert!(usize::from(col) > area.width);
}

/// 活动区的高度也要按窄框测：拿终端宽去测会少算一行，整块跟着排错位置。
#[test]
fn lobby_activity_rows_measure_at_box_width() {
    let area = lobby_box();
    let input = overflowing_input();

    let boxed = repl_input_rendered_rows(&input, 0, false, area.width);
    let full = repl_input_rendered_rows(&input, 0, false, TERMINAL_COLS);

    // 文字两行 + 上下竖条 + footer。
    assert_eq!(boxed, 5);
    assert_eq!(full, 4);
}

/// 上下方向键按「第几个物理行」找落点，宽度错了就跳错行。
#[test]
fn lobby_vertical_cursor_moves_within_the_narrow_box() {
    let area = lobby_box();
    let input = overflowing_input();
    let end = input.chars().count();

    // 窄框里末尾在第二行，上一行存在，光标要真的挪走。
    let up = repl_move_cursor_vertical_for_cols("  ", &input, end, -1, area.width);
    assert_ne!(up, end);
    // 整终端宽下这段只有一行，上一行无处可去——修复前按的就是这套。
    let up_full = repl_move_cursor_vertical_for_cols("  ", &input, end, -1, TERMINAL_COLS);
    assert_eq!(up_full, end);
}

/// 中文宽字符跨边界：内容宽 78 是偶数，45 个中文字正好在 39 个字处折行。
#[test]
fn lobby_wraps_wide_graphemes_on_the_box_edge() {
    let area = lobby_box();
    let input = overflowing_input();

    let rows = repl_wrapped_input_rows_for_cols("  ", &repl_input_lines(&input), area.width);

    assert_eq!(rows[0].chars().count(), 39);
    assert_eq!(rows[1].chars().count(), 6);
}

/// 显式换行（Shift+Enter）也按窄框算行数。
#[test]
fn lobby_counts_explicit_newlines_at_box_width() {
    let area = lobby_box();

    let rows = repl_input_rendered_rows("abc\ndef\nghi", 0, false, area.width);

    assert_eq!(rows, 3 + 3);
}

/// 缩行时旧 footer 那一行必须被挑出来重画，否则屏幕上同时挂着两条。
#[test]
fn shrinking_lobby_keeps_one_footer() {
    // 输入从两行缩回一行：活动区高度 6 → 5，整块位置没动。
    let stale = stale_activity_rows(Some((10, 6)), Some((10, 5)));

    assert_eq!(stale, vec![15]);
}

/// 整块往上挪（终端变高、banner 重新居中）时，旧活动区底下那几行同样要擦。
#[test]
fn lobby_moving_up_cleans_the_rows_it_leaves_behind() {
    let stale = stale_activity_rows(Some((12, 6)), Some((9, 6)));

    assert_eq!(stale, vec![15, 16, 17]);
}
