//! 等待动画的光标记账。
//!
//! 症状(08-26 用户实测,claude-code 供应商):工具与输出、工具与工具之间偶尔
//! 出现很大的空档。空档的本质就是动画收尾后光标停在了比起点更低的行——后面
//! 的内容便从那里开始打印,中间留下一片空白。
//!
//! 这里把动画产生的字节流喂给 `terminal_frame_layout`(仓库里已有的 VTE 追踪
//! 器),直接断言"起点进、起点出"。多行子块在两帧之间伸缩是最可疑的场景:
//! 清除按**上一帧记录的宽度**算行数,新帧行数不同就要靠 tick 自己配平。

use crate::cli::*;
use crate::render::wait_spinner::{SpinnerStyle, WaitSpinner};

const COLUMNS: u16 = 120;

/// 把一段输出跑进追踪器,返回光标最终落在第几行(从 0 起算)。
fn final_row(frame: &[u8]) -> u16 {
    terminal_frame_layout(frame, (0, 0), COLUMNS, None).cursor.1
}

fn sub_block(lines: usize) -> String {
    (0..lines)
        .map(|index| format!("第 {index} 条进度"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn spinner_returns_the_cursor_to_its_starting_row() {
    for style in [SpinnerStyle::Scanner, SpinnerStyle::Braille] {
        for lines in [0usize, 1, 3, 6] {
            let mut spinner = WaitSpinner::start("加载中".to_string(), style);
            if lines > 0 {
                spinner.set_sub_phase(Some(sub_block(lines)));
            }
            let mut frame = Vec::new();
            for _ in 0..4 {
                spinner.tick(&mut frame).unwrap();
            }
            spinner.stop(&mut frame).unwrap();
            assert_eq!(
                final_row(&frame),
                0,
                "子块 {lines} 行时动画没把光标还回起点"
            );
        }
    }
}

/// 子块在动画运行中伸缩——工具进度行数变化时天天发生。
#[test]
fn spinner_survives_a_growing_and_shrinking_sub_block() {
    let mut spinner = WaitSpinner::start("加载中".to_string(), SpinnerStyle::Braille);
    let mut frame = Vec::new();
    for lines in [1usize, 4, 7, 2, 5, 1] {
        spinner.set_sub_phase(Some(sub_block(lines)));
        spinner.tick(&mut frame).unwrap();
    }
    spinner.stop(&mut frame).unwrap();
    assert_eq!(final_row(&frame), 0, "子块伸缩后光标没回到起点");
}

/// 子块整个消失(工具跑完、进度行清空)。
#[test]
fn spinner_survives_the_sub_block_disappearing() {
    let mut spinner = WaitSpinner::start("加载中".to_string(), SpinnerStyle::Scanner);
    let mut frame = Vec::new();
    spinner.set_sub_phase(Some(sub_block(5)));
    spinner.tick(&mut frame).unwrap();
    spinner.set_sub_phase(None);
    spinner.tick(&mut frame).unwrap();
    spinner.stop(&mut frame).unwrap();
    assert_eq!(final_row(&frame), 0, "子块消失后光标没回到起点");
}

/// 块模式:多个工具各占一块,块之间用空行分隔。普通路径会把空行**过滤掉**,
/// 块模式却要留着——两条路对"一帧有几行"的口径不同,是最可疑的地方。
#[test]
fn spinner_returns_the_cursor_in_block_mode() {
    let marker = crate::render::wait_spinner::BLOCK_MARKER;
    let blocks = |count: usize| {
        (0..count)
            .map(|index| format!("{marker}工具 {index}\n  明细一\n  明细二"))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let mut spinner = WaitSpinner::start(String::new(), SpinnerStyle::Braille);
    let mut frame = Vec::new();
    for count in [1usize, 3, 2, 4, 1] {
        spinner.set_sub_phase(Some(blocks(count)));
        spinner.tick(&mut frame).unwrap();
    }
    spinner.stop(&mut frame).unwrap();
    assert_eq!(final_row(&frame), 0, "块模式收尾后光标没回到起点");
}

/// 命令块的行数记账。
///
/// 症状(08-26 用户复现口径:"工具输出和正文之间**必定**有一个大空档","工具
/// 和加载进度条之间也必定有")。实时块在跑的时候画 N 行,落地时按
/// `rendered_line_widths` 清掉重画;清得少了就留空行,而这个边界每轮必经,所以
/// 是必现而不是偶发。
///
/// 量的是**完整字节流**(实时若干帧 + 落地)跑完之后光标停在第几行,和静态写法
/// 的行数对比。第一版只把落地那一帧单独从 (0,0) 量,等于假装实时块没画过——
/// MoveUp 在第 0 行是空操作,漏掉的正是要查的东西。
#[test]
fn committed_command_block_leaves_exactly_one_trailing_blank() {
    use crate::render::CommandLiveDisplay;
    use crate::tools::CommandOutputStream;

    for output_lines in [1usize, 3, 5, 12, 40] {
        let mut frame = Vec::new();
        let mut display = CommandLiveDisplay::new("echo hi", 10, true, false);
        for index in 0..output_lines {
            display.push(
                CommandOutputStream::Stdout,
                format!("输出第 {index} 行\n").as_bytes(),
            );
            display.tick(&mut frame).unwrap();
        }
        display.set_result(true);
        display.commit(&mut frame, true).unwrap();
        let live_rows = terminal_frame_layout(&frame, (0, 0), COLUMNS, None)
            .cursor
            .1;

        let mut static_frame = Vec::new();
        let mut static_display = CommandLiveDisplay::new("echo hi", 10, true, false);
        for index in 0..output_lines {
            static_display.push(
                CommandOutputStream::Stdout,
                format!("输出第 {index} 行\n").as_bytes(),
            );
        }
        static_display.set_result(true);
        static_display
            .write_static(&mut static_frame, true)
            .unwrap();
        let static_rows = terminal_frame_layout(&static_frame, (0, 0), COLUMNS, None)
            .cursor
            .1;

        assert_eq!(
            live_rows,
            static_rows,
            "输出 {output_lines} 行时,走实时块比静态写法多占 {} 行",
            live_rows as i32 - static_rows as i32
        );
    }
}

/// 工具卡片落地之后,紧接着的东西被顶下去多少行。
///
/// 用户复现口径(08-26):"工具输出和正文之间**必定**有一个大空档","工具和
/// 加载进度条之间也必定有"。共同点是工具落地这一刻,而不是某一种后续内容
/// ——所以量的就是这个边界本身留下几个空行。设计上只该留一个。
#[test]
fn a_settled_tool_card_leaves_exactly_one_blank_before_what_follows() {
    use crate::llm::{ChatStreamChunk, ChatStreamKind};
    use crate::render::{ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode};

    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.use_buffered_output();
    renderer.live_summary = true;

    // 先有一段正文(真实回合里工具前后都夹着正文),再跑一个非命令工具。
    renderer
        .write_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: "先说一句".to_string(),
        })
        .unwrap();
    renderer.write_tool_call("gpustoggle", "{}").unwrap();
    renderer.write_tool_result("gpustoggle", false, "").unwrap();
    let _ = renderer.take_output_frame();

    // 从这里开始量:工具已落地,下一样东西该紧跟着来。
    renderer
        .write_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: "工具之后的正文".to_string(),
        })
        .unwrap();
    let frame = renderer.take_output_frame();
    let rows = terminal_frame_layout(&frame, (0, 0), COLUMNS, None)
        .cursor
        .1;
    assert!(
        rows <= 1,
        "工具落地到下一段正文之间空出了 {rows} 行,设计上最多 1 行"
    );
}

/// 命令工具卡片落地之后,紧接着的东西被顶下去多少行。
///
/// 08-27 用户补的两条口径把范围钉死了:空档"必定"出现在工具与正文之间、工具
/// 与进度条之间,而且 **shell-hook 也一样**——那条路 `live` 传的是 None,渲染
/// 器直写终端,不经过实时尾部。所以病灶只能在 `StreamRenderer` 内部,而且截图
/// 里空档后面跟的都是**命令**工具块(此前那条用例用的是非命令工具,漏掉了这条
/// 路)。
#[test]
fn a_settled_command_card_leaves_exactly_one_blank_before_what_follows() {
    use crate::llm::{ChatStreamChunk, ChatStreamKind};
    use crate::render::{ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode};
    use crate::tools::CommandOutputStream;

    for output_lines in [1usize, 3, 8, 30] {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.use_buffered_output();
        // 测试里 stdout 不是终端,`live_summary` 默认为假 → 实时块根本不画,
        // 量到的是静态路径。用户看到的是实时那条,必须显式打开。
        renderer.live_summary = true;
        renderer
            .write_chunk(ChatStreamChunk {
                kind: ChatStreamKind::Content,
                text: "先说一句".to_string(),
            })
            .unwrap();
        renderer.write_tool_call("run_command", "ls").unwrap();
        for index in 0..output_lines {
            renderer
                .write_command_output(
                    "run_command",
                    CommandOutputStream::Stdout,
                    format!("输出第 {index} 行\n").as_bytes(),
                )
                .unwrap();
        }
        renderer.write_tool_result("run_command", true, "").unwrap();
        let _ = renderer.take_output_frame();

        // 从这里开始量:命令卡片已落地,下一样东西该紧跟着来。
        renderer
            .write_chunk(ChatStreamChunk {
                kind: ChatStreamKind::Content,
                text: "工具之后的正文".to_string(),
            })
            .unwrap();
        let frame = renderer.take_output_frame();
        let rows = terminal_frame_layout(&frame, (0, 0), COLUMNS, None)
            .cursor
            .1;
        assert!(
            rows <= 1,
            "输出 {output_lines} 行的命令卡片落地后,到下一段正文空出了 {rows} 行"
        );
    }
}

/// 命令卡片之间的空档——截图里空档两侧正好都是命令块(08-27)。
#[test]
fn back_to_back_command_cards_leave_exactly_one_blank() {
    use crate::render::{ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode};
    use crate::tools::CommandOutputStream;

    for output_lines in [1usize, 3, 8, 30] {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.use_buffered_output();
        renderer.live_summary = true;

        // 超宽命令与超宽输出:真实终端里这些会折行,而清除按记录的宽度换算
        // 物理行数(`rendered_physical_rows`)。短行永远试不出折行相关的错位,
        // 截图里那条命令明明白白折了两行(08-27)。
        let wide_command = format!(
            "cd ~/Projects/{} && cargo build 2>&1 | tail -60",
            "x".repeat(130)
        );
        renderer
            .write_tool_call("run_command", &wide_command)
            .unwrap();
        for index in 0..output_lines {
            renderer
                .write_command_output(
                    "run_command",
                    CommandOutputStream::Stdout,
                    format!("输出第 {index} 行{}\n", "长".repeat(70)).as_bytes(),
                )
                .unwrap();
        }
        renderer.write_tool_result("run_command", true, "").unwrap();
        let _ = renderer.take_output_frame();

        // 第二张卡片开画:它与上一张之间该只隔一个空行。
        renderer.write_tool_call("run_command", "pwd").unwrap();
        let frame = renderer.take_output_frame();
        let rows = terminal_frame_layout(&frame, (0, 0), COLUMNS, None)
            .cursor
            .1;
        assert!(
            rows <= 1,
            "上一张卡片输出 {output_lines} 行时,两张命令卡片之间空出了 {rows} 行"
        );
    }
}
