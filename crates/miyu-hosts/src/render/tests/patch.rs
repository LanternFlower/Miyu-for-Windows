//! 补丁 diff 的渲染。

use super::shared::*;
use crate::render::*;

#[test]
fn patch_diff_uses_muted_change_backgrounds() {
    let diff = "--- a/demo.txt\n+++ b/demo.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n";
    let output = render_patch_diff("demo.txt", diff);

    assert!(output.contains("\x1b[48;2;60;41;53m"));
    assert!(output.contains("\x1b[48;2;32;52;67m"));
    assert!(!output.contains("\x1b[48;5;52m"));
    assert!(!output.contains("\x1b[48;5;22m"));
}

/// diff 的行是不是"带行号槽"的那种。
///
/// 槽的形状：右对齐的行号 + 空格 + 符号 + 空格。**没有竖线**——符号那一列
/// 已经把增删说清楚了，再加一根分隔线只是把正文往右推两格（用户拍板）。
/// 行号栏多宽按这一段 diff 里最大的行号来（最少两格），所以这里不数格子，
/// 只认形状。
fn is_gutter_line(line: &str) -> bool {
    // 这些用例的 diff 都只有一两行，栏宽就是下限的两格：`NN S ` 五格。
    // 续行的槽是空的（两格空 + 空格 + 两格空），一样认。
    let chars: Vec<char> = line.chars().take(5).collect();
    chars.len() == 5
        && chars[..2]
            .iter()
            .all(|ch| ch.is_ascii_digit() || *ch == ' ')
        && chars[2] == ' '
        && matches!(chars[3], '+' | '-' | ' ')
        && chars[4] == ' '
        && !line.trim().is_empty()
}

#[test]
fn patch_diff_wraps_long_lines_with_aligned_gutter() {
    let diff = format!(
        "--- a/run-vm.sh\n+++ b/run-vm.sh\n@@ -1,0 +1,1 @@\n+{}\n",
        "RESULT=$(sudo virsh qemu-agent-command archlinux ".repeat(8)
    );
    let output = render_patch_diff("run-vm.sh", &diff);
    let visible = strip_ansi_for_test(&output);
    let diff_lines = visible
        .lines()
        .filter(|line| is_gutter_line(line))
        .collect::<Vec<_>>();
    assert!(diff_lines.len() > 1, "diff line was not wrapped: {visible}");
    // 行号栏按这一段最大的行号定宽（最少两格）：一行的文件就是 ` 1 + `，
    // 不再是固定四格把整段 diff 往右推。
    assert!(diff_lines[0].starts_with(" 1 + "), "{:?}", diff_lines[0]);
    // 续行的槽要和首行一样宽，正文才对得上列。
    assert!(diff_lines[1].starts_with("     "), "{:?}", diff_lines[1]);
    assert!(!diff_lines[1].starts_with("      "), "{:?}", diff_lines[1]);
    assert!(!visible.contains('│'), "diff 里不该再有竖线: {visible}");

    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output
        .lines()
        .filter(|line| is_gutter_line(&strip_ansi_for_test(line)))
    {
        assert!(
            visible_width(line) < terminal_width,
            "diff line too wide: {line}"
        );
    }
}

#[test]
fn patch_diff_wraps_wide_character_lines() {
    let diff = format!(
        "--- a/demo.txt\n+++ b/demo.txt\n@@ -1,0 +1,1 @@\n+{}\n",
        "软换行问题".repeat(30)
    );
    let output = render_patch_diff("demo.txt", &diff);
    let visible = strip_ansi_for_test(&output);
    assert!(visible.lines().filter(|line| is_gutter_line(line)).count() > 1);

    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output
        .lines()
        .filter(|line| is_gutter_line(&strip_ansi_for_test(line)))
    {
        assert!(
            visible_width(line) < terminal_width,
            "wide-char diff line too wide: {line}"
        );
    }
}

/// 「改文件」这一步点开要看得到 diff，而且路径只出现一次。
///
/// 原来 diff 是直接打屏的：既不在时间线里（点不开、收不起），也不在缓冲里
/// （重开就没了）；表头又把路径重复一遍，时间线那一行已经写过了。
#[test]
fn edit_step_expands_into_a_diff_without_repeating_the_path() {
    let payload = serde_json::json!({
        "path": "demo.txt",
        "diff": "--- a/demo.txt\n+++ b/demo.txt\n@@ -1,1 +1,1 @@\n-旧的一行\n+新的一行\n",
    })
    .to_string();
    let lines = patch_preview_lines(&payload, 60).expect("有 diff 就该有得展开");
    let visible: Vec<String> = lines.iter().map(|line| strip_ansi_for_test(line)).collect();
    assert!(
        visible.iter().any(|line| line.contains("旧的一行")),
        "看不到删掉的那行: {visible:?}"
    );
    assert!(
        visible.iter().any(|line| line.contains("新的一行")),
        "看不到加上的那行: {visible:?}"
    );
    assert!(
        !visible.iter().any(|line| line.contains("demo.txt")),
        "路径不该再出现一次（时间线那一行已经写了）: {visible:?}"
    );
    for line in &visible {
        assert!(
            visible_width(line) <= 60,
            "diff 宽度得按传进去的算，不是整屏: {line:?}"
        );
    }
    // 没有 diff 就没什么可展开的
    assert!(patch_preview_lines("{\"path\":\"x\",\"diff\":\"\"}", 60).is_none());
    assert!(patch_preview_lines("not json", 60).is_none());
}
