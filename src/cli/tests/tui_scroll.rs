//! 翻页与跟随（BUG-06：PgUp 到顶、PgDn 到底之后视口不再跟着输出走）。
//!
//! 这一类不变量旁边每一个同类状态（展开收起、提问面板、浮层）都有用例守着，唯独
//! 翻页这一格是空的——初诊报告 §8。

use crate::cli::repl::tail::screen::Screen;

fn screen_with_lines(count: usize) -> Screen {
    let mut screen = Screen::detached(80, 30);
    let mut text = String::new();
    for i in 0..count {
        text.push_str(&format!("第 {i} 行\r\n"));
    }
    screen.feed_for_test(text.as_bytes());
    screen
}

/// 翻到底就该恢复跟随——哪怕按键那一刻算的「底」和落帧时的不一样（活动区变高了）。
#[test]
fn paging_back_to_the_bottom_restores_follow_even_if_the_layout_moved() {
    let mut screen = screen_with_lines(120);
    screen.prepare_frame(6);
    assert!(screen.following());
    screen.scroll_by(-40);
    screen.prepare_frame(6);
    assert!(!screen.following(), "翻上去了还说在跟随");
    // 活动区先变高（排队行 / 后台状态行冒出来）：按这一帧的底，PgDn 一页停在底
    // 上方几行，不跟随是对的。
    screen.prepare_frame(10);
    screen.scroll_by(40);
    screen.prepare_frame(10);
    assert!(!screen.following(), "离底还有几行就说在跟随");
    // 活动区又缩回去（状态行没了）：底往下挪，视口被夹到底——那就是到底了，
    // 跟随要跟着扶正，不能停在「屏幕上在底部、状态上不跟随」的不动点上。
    screen.prepare_frame(2);
    assert!(screen.following(), "被夹到底之后应该恢复跟随");
    // 之后来了新内容，视口要跟着走。
    screen.feed_for_test("新的一行\r\n新的两行\r\n".as_bytes());
    screen.prepare_frame(10);
    let top = screen.scroll_for_test();
    assert!(
        screen.frame_line(top + 19).contains("新的两行")
            || screen.frame_line(top + 18).contains("新的两行"),
        "视口没跟着新内容走: scroll={top}"
    );
}

/// 已经在底了再按 PgDn：视口没动，就不该整屏重画（原来每按一次闪一下）。
#[test]
fn page_down_at_the_bottom_does_not_force_a_repaint() {
    let mut screen = screen_with_lines(120);
    screen.prepare_frame(6);
    assert!(screen.following());
    // 把「强制重画」那一位消耗掉：模拟画过一帧。
    let _ = screen.take_force_for_test();
    screen.scroll_by(20);
    assert!(!screen.repaint_pending(), "到底之后按 PgDn 还是整屏重画");
    assert!(screen.following());
    // 真翻动了才重画。
    screen.scroll_by(-20);
    assert!(screen.repaint_pending());
}
