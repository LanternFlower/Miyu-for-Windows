//! 命令那一步在过程时间线里的长相:抬头给 title、正文给命令本身。
//!
//! 单独成文件是因为 `timeline.rs` 已经超了 AGENTS §6.2 的行数上限,门禁不许它
//! 再长。

use super::timeline::{block_id_in, timeline_renderer_with_preview_rows, with_blocks};
use crate::render::t;

/// 全屏：跑完之后抬头底下留着的是**命令本身**（超出配置行数的在底部换成省略
/// 标记），输出只在点开里；收成 `Worked for` 后点开那一块，命令与输出都在。
#[test]
fn a_finished_commands_preview_rows_follow_the_configured_count() {
    with_blocks(|| {
        // 全屏与静态时间线同一个量,都归 display.command_output_lines 管。
        let mut renderer = timeline_renderer_with_preview_rows(6);
        renderer.use_buffered_output();
        let command = (1..=8)
            .map(|index| format!("echo cmd-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let arguments = serde_json::json!({ "command": command, "title": "跑八行" });
        renderer
            .write_tool_call("run_command", &arguments.to_string())
            .unwrap();
        for index in 1..=8 {
            renderer
                .write_command_output(
                    "run_command",
                    crate::tools::CommandOutputStream::Stdout,
                    format!("line-{index}\n").as_bytes(),
                )
                .unwrap();
        }
        renderer
            .write_tool_result("run_command", true, r#"{"success":true,"exit_code":0}"#)
            .unwrap();
        renderer.finalize_tools_summary().unwrap();
        let (_, live) = renderer.timeline_live(Vec::new());
        let live = crate::render::strip_ansi_text(&live.unwrap_or_default());
        assert!(live.contains("跑八行"), "抬头没给 title: {live:?}");
        for kept in ["echo cmd-1", "echo cmd-5"] {
            assert!(
                live.contains(kept),
                "跑完之后 {kept} 没留在抬头底下: {live:?}"
            );
        }
        // 留头不留尾:省略标记在底部。
        assert!(
            live.contains("⋮") && !live.contains("echo cmd-8"),
            "超出六行的没在底部换成省略标记: {live:?}"
        );
        assert!(!live.contains("line-"), "输出不该露在抬头底下: {live:?}");
        // 命令行从连线穿过：`  │ echo cmd-1`。
        assert!(
            live.lines().any(|line| line.starts_with("  │ echo cmd-1")),
            "命令行没有连线前缀: {live:?}"
        );
        // 收成 Worked for 之后，点开那一块里命令与输出都在。
        renderer.cut_timeline().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        let id = block_id_in(&frame).expect("收缩行没挂块");
        let expanded = crate::render::blocks::get(id).unwrap_or_default();
        // 完整命令与输出挂在那一步自己的块上。
        let detail = expanded
            .iter()
            .filter_map(|line| block_id_in(line))
            .filter_map(crate::render::blocks::get)
            .flatten()
            .map(|line| crate::render::strip_ansi_text(&line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            detail.contains("echo cmd-8") && detail.contains("line-8"),
            "点开里该有完整命令与输出: {detail:?}"
        );
    });

    // 0 = 一行预览都不露(用户明确要的那一档)。
    with_blocks(|| {
        let mut renderer = timeline_renderer_with_preview_rows(0);
        renderer.use_buffered_output();
        renderer
            .write_tool_call("run_command", r#"{"command":"seq 1 8"}"#)
            .unwrap();
        for index in 1..=8 {
            renderer
                .write_command_output(
                    "run_command",
                    crate::tools::CommandOutputStream::Stdout,
                    format!("line-{index}\n").as_bytes(),
                )
                .unwrap();
        }
        // 覆盖范围说清楚:这里断的是「跑完之后」那条路(tool_summary.rs 的
        // detail_tail)。「跑着的时候」那条在 timeline_running_tool_lines 里,
        // 要渲染器处在真有工具在飞的活动帧才走得到,本夹具够不着——那一行的
        // 行数来源也改成了同一个配置,但没有测试守着,改它时当心。
        renderer
            .write_tool_result("run_command", true, r#"{"success":true,"exit_code":0}"#)
            .unwrap();
        renderer.finalize_tools_summary().unwrap();
        let (_, live) = renderer.timeline_live(Vec::new());
        let live = crate::render::strip_ansi_text(&live.unwrap_or_default());
        for line in 1..=8 {
            assert!(
                !live.contains(&format!("line-{line}")),
                "设 0 跑完之后还留着预览行: {live:?}"
            );
        }
    });
}
