//! 全屏后端的帧成本量尺（BUG-07「内容多了之后巨卡，展开 tag 行后特别卡」）。
//!
//! 不是断言，是尺子：先量出「每帧花多久」随会话长度、展开块数怎么涨，改完再量一遍
//! 对比。走的是 `paint` 同一条路（`prepare_frame` + 每一可见行 `frame_line`），只是
//! 不写终端。
//!
//!     cargo test -p miyu --lib -- cli::tests::tui_perf --ignored --nocapture
//!
//! 用户跑的是 debug 二进制，所以按 debug 量才是他手上的感觉。

use super::tui_blocks::with_blocks;
use crate::cli::repl::tail::screen::Screen;
use miyu_hosts::render::blocks;
use std::time::Instant;

/// 一段过程 = 一个思考块（`open` 决定它出来是不是展开态）+ 一个工具块 + 一段正文。
fn long_session(screen: &mut Screen, segments: usize, thought_open: bool) {
    for i in 0..segments {
        let thought = blocks::register(
            (0..5)
                .map(|k| format!("    思考正文 {i}-{k} 先看一眼需求，再决定怎么下手。"))
                .collect(),
        )
        .expect("思考块没登记");
        let tool = blocks::register((0..3).map(|k| format!("    命令输出 {i}-{k}")).collect())
            .expect("工具块没登记");
        let text = format!(
            "{}  ✳ 已思考 · 第 {i} 段 · 1.2s{}\r\n  │\r\n{}  $ 运行命令 · ls · 12ms{}\r\n\r\n这是第 {i} 段正文，说点什么好让缓冲长起来。\r\n\r\n",
            blocks::begin_marker_in(thought, thought_open),
            blocks::END_MARKER,
            blocks::begin_marker(tool),
            blocks::END_MARKER
        );
        screen.feed_for_test(text.as_bytes());
    }
}

/// 一帧：和 `paint` 一样，先 `prepare_frame`，再把视口里每一行算出来。
fn frame(screen: &mut Screen) -> usize {
    let body = screen.prepare_frame(4);
    let top = screen.scroll_for_test();
    let mut bytes = 0;
    for y in 0..usize::from(body) {
        bytes += screen.frame_line(top + y).len();
    }
    bytes
}

#[test]
#[ignore]
fn frame_cost_by_session_length() {
    with_blocks(|| {
        for &(segments, open) in &[
            (50, false),
            (50, true),
            (300, false),
            (300, true),
            (800, true),
        ] {
            let mut screen = Screen::detached(120, 50);
            long_session(&mut screen, segments, open);
            frame(&mut screen); // 预热：替用户开块
            let frames = 30u32;
            let t = Instant::now();
            for _ in 0..frames {
                frame(&mut screen);
            }
            let idle = t.elapsed() / frames;
            let t = Instant::now();
            for k in 0..frames {
                screen.feed_for_test(format!("流式第 {k} 行\r\n").as_bytes());
                frame(&mut screen);
            }
            let streaming = t.elapsed() / frames;
            println!(
                "segments={segments:>4} open={open:<5} blocks={:>4} expanded={:>4} view={:>6} idle/frame={idle:?} streaming/frame={streaming:?}",
                screen.block_count(),
                screen.expanded_count(),
                screen.view_len()
            );
        }
    });
}

#[test]
#[ignore]
fn toggling_a_huge_diff_block() {
    with_blocks(|| {
        let mut screen = Screen::detached(120, 50);
        long_session(&mut screen, 100, true);
        let big = blocks::register(
            (0..3000)
                .map(|k| format!("    +第 {k} 行 这是一份很长的 diff 的一行，凑点宽度好让它像真的"))
                .collect(),
        )
        .expect("大块没登记");
        screen.feed_for_test(
            format!(
                "{}  ✎ 编辑文件 · big.rs · +3000 -0{}\r\n\r\n",
                blocks::begin_marker(big),
                blocks::END_MARKER
            )
            .as_bytes(),
        );
        frame(&mut screen);
        let expanded_before = screen.expanded_count();
        let t = Instant::now();
        assert!(screen.toggle_block(big), "大块点不开");
        let toggle = t.elapsed();
        let frames = 30u32;
        let t = Instant::now();
        for _ in 0..frames {
            frame(&mut screen);
        }
        let after = t.elapsed() / frames;
        println!(
            "expanded before={expanded_before} after={} toggle={toggle:?} frame_after={after:?} view={}",
            screen.expanded_count(),
            screen.view_len()
        );
    });
}
