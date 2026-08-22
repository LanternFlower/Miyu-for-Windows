//! 补丁 diff 的渲染。

use crate::render::*;
use super::shared::*;

#[test]
fn patch_diff_uses_muted_change_backgrounds() {
    let diff = "--- a/demo.txt\n+++ b/demo.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n";
    let output = render_patch_diff("demo.txt", diff);

    assert!(output.contains("\x1b[48;2;60;41;53m"));
    assert!(output.contains("\x1b[48;2;32;52;67m"));
    assert!(!output.contains("\x1b[48;5;52m"));
    assert!(!output.contains("\x1b[48;5;22m"));
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
        .filter(|line| line.contains('│'))
        .collect::<Vec<_>>();
    assert!(diff_lines.len() > 1, "diff line was not wrapped: {visible}");
    assert!(diff_lines[0].starts_with("    1 + │ "));
    assert!(diff_lines[1].starts_with("        │ "));

    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output.lines().filter(|line| line.contains('│')) {
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
    assert!(visible.lines().filter(|line| line.contains('│')).count() > 1);

    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output.lines().filter(|line| line.contains('│')) {
        assert!(
            visible_width(line) < terminal_width,
            "wide-char diff line too wide: {line}"
        );
    }
}
