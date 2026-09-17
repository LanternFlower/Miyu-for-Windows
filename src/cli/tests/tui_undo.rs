//! `/undo` 在全屏画布上只截掉最后一轮：轮标记（`TURN_START_MARKER`）记起点，
//! 截回去之后前面的行、块、滚动历史都原样留着（用户 09-18：整段回放会把历史丢掉）。

use super::tui_blocks::with_blocks;
use crate::cli::repl::tail::screen::Screen;
use miyu_hosts::render::blocks;

fn turn(id: u64, prompt: &str, reply: &str) -> String {
    format!(
        "{}\r\n  {prompt}\r\n{}  ✳ 已思考{}\r\n{reply}\r\n",
        blocks::TURN_START_MARKER,
        blocks::begin_marker(id),
        blocks::END_MARKER
    )
}

#[test]
fn undo_truncates_only_the_last_turn_and_keeps_the_rest() {
    with_blocks(|| {
        let mut screen = Screen::detached(80, 24);
        let first = blocks::register(vec!["  想了一下".into()]).unwrap();
        let second = blocks::register(vec!["  又想了一下".into()]).unwrap();
        screen.feed_for_test(turn(first, "第一句", "第一句的回复").as_bytes());
        let rows_after_first = screen.view_len();
        screen.feed_for_test(turn(second, "第二句", "第二句的回复").as_bytes());
        assert_eq!(screen.block_count(), 2);
        let text = |screen: &Screen| {
            (0..screen.view_len())
                .map(|index| {
                    screen
                        .view_row(index)
                        .iter()
                        .map(|span| span.text.clone())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(text(&screen).contains("第二句的回复"));

        assert!(screen.truncate_last_turn(), "没找到轮标记");
        let after = text(&screen);
        assert!(after.contains("第一句的回复"), "前一轮被截掉了: {after}");
        assert!(!after.contains("第二句"), "撤掉的那一轮还在: {after}");
        assert_eq!(screen.view_len(), rows_after_first, "行数没截回第一轮末尾");
        assert_eq!(screen.block_count(), 1, "撤掉那一轮的块没跟着走");
        // 再撤一次：第一轮也没了，画布空了；第三次没有标记可撤。
        assert!(screen.truncate_last_turn());
        assert!(!text(&screen).contains("第一句"));
        assert!(!screen.truncate_last_turn());
    });
}

/// 上一轮的最后一行没换行就提交下一句：标记落在下一行，截回去时那半行留着。
#[test]
fn undo_keeps_a_half_line_from_the_previous_turn() {
    with_blocks(|| {
        let mut screen = Screen::detached(80, 24);
        screen.feed_for_test(
            b"\xe6\xb2\xa1\xe6\x8d\xa2\xe8\xa1\x8c\xe7\x9a\x84\xe5\xb0\xbe\xe5\xb7\xb4",
        ); // 没换行的尾巴
        screen.feed_for_test(
            format!("{}\r\n  第二句\r\n回复\r\n", blocks::TURN_START_MARKER).as_bytes(),
        );
        assert!(screen.truncate_last_turn());
        let first_row = screen
            .view_row(0)
            .iter()
            .map(|span| span.text.clone())
            .collect::<String>();
        assert!(
            first_row.contains("没换行的尾巴"),
            "半行被截掉了: {first_row:?}"
        );
        assert!(
            screen.view_len() <= 2,
            "撤掉的那一轮还在: {}",
            screen.view_len()
        );
    });
}
