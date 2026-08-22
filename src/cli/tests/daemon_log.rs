//! daemon 日志的格式化、跟随与增量读取。

// 被测的东西散在 cli::mod 与 repl 的兄弟模块里，这里全都要够到。
use crate::cli::*;
use super::shared::*;
#[test]
fn config_reload_response_uses_codes_and_supports_legacy_busy_errors() {
    assert_eq!(
        validate_config_reload_response(Some(IpcFrame::coded_error(
            ipc::ErrorCode::Busy,
            "localized busy message",
        )))
        .unwrap(),
        ConfigReloadResponse::Busy
    );
    assert_eq!(
        validate_config_reload_response(Some(IpcFrame::error(ipc::ADMIN_BUSY_MESSAGE)))
            .unwrap(),
        ConfigReloadResponse::Busy
    );
    assert!(
        validate_config_reload_response(Some(IpcFrame::error("invalid configuration")))
            .is_err()
    );
}

#[test]
fn daemon_log_formatter_parses_targets_and_preserves_multiline_content() {
    let parsed = parse_daemon_log_line(
        "2026-07-29T12:34:56.789Z  INFO miyu::qq: listener ready port=8090",
    )
    .unwrap();
    assert_eq!(parsed.level, "INFO");
    assert_eq!(parsed.module, "miyu::qq");
    assert_eq!(parsed.message, "listener ready port=8090");

    let rendered = format_daemon_log_line(
        "2026-07-29T12:34:56.789Z  INFO miyu::qq: listener ready port=8090",
        false,
    );
    assert!(!rendered.contains('\x1b'));
    assert!(rendered.ends_with("[INFO] [miyu::qq] listener ready port=8090"));
    assert_eq!(
        format_daemon_log_line("判断原因：保留这一行原有的内容", true),
        "判断原因：保留这一行原有的内容"
    );
}

#[test]
fn daemon_log_formatter_supports_legacy_lines_and_tty_colors() {
    let legacy = "2026-07-29T12:34:56.789Z  WARN OneBot connection closed reason=timeout";
    let parsed = parse_daemon_log_line(legacy).unwrap();
    assert_eq!(parsed.module, "miyu");
    assert_eq!(parsed.message, "OneBot connection closed reason=timeout");

    let rendered = format_daemon_log_line(legacy, true);
    assert!(rendered.contains('\x1b'));
    assert!(rendered.contains("[WARN]"));
    assert!(rendered.ends_with("OneBot connection closed reason=timeout"));
}

#[test]
fn daemon_log_formatter_colors_entire_active_reply_decisions() {
    let mut formatter = DaemonLogStreamFormatter::default();
    let mut reply = Vec::new();
    formatter
        .push(
            b"2026-07-29T12:34:56.789Z  INFO miyu::qq: \xe3\x80\x90\xe7\xbb\xad\xe8\x81\x8a\xe7\xaa\x97\xe5\x8f\xa3\xe5\x88\xa4\xe6\x96\xad\xef\xbc\x9a\xe5\x9b\x9e\xe5\xa4\x8d\xe3\x80\x91\n\xe7\xbb\x93\xe6\x9e\x9c\xef\xbc\x9a\xe5\x9b\x9e\xe5\xa4\x8d\n",
            true,
            &mut reply,
        )
        .unwrap();
    let reply = String::from_utf8(reply).unwrap();
    assert!(reply.lines().all(|line| line.contains('\x1b')));

    let mut no_reply = Vec::new();
    formatter
        .push(
            b"2026-07-29T12:34:57.789Z  INFO miyu::qq: \xe3\x80\x90\xe4\xb8\xbb\xe5\x8a\xa8\xe5\x9b\x9e\xe5\xa4\x8d\xe5\x88\xa4\xe6\x96\xad\xef\xbc\x9a\xe4\xb8\x8d\xe5\x9b\x9e\xe5\xa4\x8d\xe3\x80\x91\n\xe7\xbb\x93\xe6\x9e\x9c\xef\xbc\x9a\xe4\xb8\x8d\xe5\x9b\x9e\xe5\xa4\x8d\n",
            true,
            &mut no_reply,
        )
        .unwrap();
    let no_reply = String::from_utf8(no_reply).unwrap();
    assert!(no_reply.lines().all(|line| line.contains('\x1b')));
    assert_ne!(reply, no_reply);

    let mut reset = Vec::new();
    formatter
        .push(
            b"2026-07-29T12:34:58.789Z  INFO miyu::qq: listener ready\nplain continuation\n",
            false,
            &mut reset,
        )
        .unwrap();
    let reset = String::from_utf8(reset).unwrap();
    assert!(reset.ends_with("[INFO] [miyu::qq] listener ready\nplain continuation\n"));
}

#[test]
fn daemon_log_formatter_recognizes_english_active_reply_decisions() {
    assert_eq!(
        active_reply_log_color("[Active reply decision: reply]\nResult: reply"),
        Some(Color::Green)
    );
    assert_eq!(
        active_reply_log_color("[Continuation decision: no reply]\nResult: no reply"),
        Some(Color::DarkGrey)
    );

    let mut color = None;
    let timestamp = format_daemon_log_line_with_state(
        "2026-07-29T12:34:56.789Z  INFO miyu::qq: ",
        true,
        &mut color,
    );
    assert!(timestamp.contains("[INFO]"));
    assert_eq!(color, None);
    let title =
        format_daemon_log_line_with_state("[Active reply decision: reply]", true, &mut color);
    assert_eq!(color, Some(Color::Green));
    assert!(title.contains('\x1b'));
    assert!(
        format_daemon_log_line_with_state("Result: reply", true, &mut color).contains('\x1b')
    );
}

#[test]
fn daemon_log_stream_formatter_waits_for_complete_lines() {
    let mut formatter = DaemonLogStreamFormatter::default();
    let mut output = Vec::new();
    formatter
        .push(
            b"2026-07-29T12:34:56.789Z  INFO miyu::qq: part",
            false,
            &mut output,
        )
        .unwrap();
    assert!(output.is_empty());

    formatter
        .push(b"ial\n  continuation\nlast", false, &mut output)
        .unwrap();
    let rendered = String::from_utf8(output).unwrap();
    assert!(rendered.contains("[INFO] [miyu::qq] partial\n"));
    assert!(rendered.ends_with("  continuation\n"));

    let mut tail = Vec::new();
    formatter.finish(false, &mut tail).unwrap();
    assert_eq!(tail, b"last\n");
}

#[test]
fn recent_daemon_logs_keep_multiline_order_across_rotated_files() {
    let temp = tempfile::tempdir().unwrap();
    let paths = pop_test_paths(temp.path());
    let logs_dir = paths.logs_dir();
    std::fs::create_dir_all(&logs_dir).unwrap();
    std::fs::write(
        logs_dir.join("miyu.2026-07-28.log"),
        "2026-07-28T12:00:00Z  INFO miyu::qq: old event\n  old continuation\n",
    )
    .unwrap();
    std::fs::write(
        logs_dir.join("miyu.2026-07-29.log"),
        "2026-07-29T12:00:00Z  WARN miyu::qq: new event\n  new continuation\n判断原因：保持多行\n",
    )
    .unwrap();

    let lines = recent_daemon_log_lines(&paths, 4).unwrap();
    assert_eq!(
        lines,
        [
            "  old continuation",
            "2026-07-29T12:00:00Z  WARN miyu::qq: new event",
            "  new continuation",
            "判断原因：保持多行",
        ]
    );
    let rendered = lines
        .iter()
        .map(|line| format_daemon_log_line(line, false))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains('\x1b'));
    assert!(rendered.contains("[WARN] [miyu::qq] new event"));
    assert!(rendered.ends_with("  new continuation\n判断原因：保持多行"));
}

#[test]
fn recent_daemon_logs_include_unstructured_daemon_stream_before_rotating_logs() {
    let temp = tempfile::tempdir().unwrap();
    let paths = pop_test_paths(temp.path());
    let logs_dir = paths.logs_dir();
    std::fs::create_dir_all(&logs_dir).unwrap();
    std::fs::write(logs_dir.join("daemon.log"), "startup banner\npanic: boom\n").unwrap();
    std::fs::write(
        logs_dir.join("miyu.2026-07-29.log"),
        "2026-07-29T12:00:00Z  INFO miyu::qq: listener ready\n",
    )
    .unwrap();

    let lines = recent_daemon_log_lines(&paths, 3).unwrap();
    assert_eq!(
        lines,
        [
            "startup banner",
            "panic: boom",
            "2026-07-29T12:00:00Z  INFO miyu::qq: listener ready",
        ]
    );
}

#[test]
fn daemon_log_follow_cursor_starts_after_the_snapshot_for_each_source() {
    let temp = tempfile::tempdir().unwrap();
    let paths = pop_test_paths(temp.path());
    let logs_dir = paths.logs_dir();
    std::fs::create_dir_all(&logs_dir).unwrap();
    let fallback = logs_dir.join("daemon.log");
    let rotating = logs_dir.join("miyu.2026-07-29.log");
    std::fs::write(&fallback, b"before fallback\n").unwrap();
    std::fs::write(&rotating, b"before rotating\n").unwrap();

    let snapshot = recent_daemon_log_snapshot(&paths, 10).unwrap();
    assert_eq!(snapshot.lines, ["before fallback", "before rotating"]);
    let cursor = snapshot.cursor;
    assert_eq!(cursor.current, Some(rotating.clone()));
    assert_eq!(cursor.fallback, Some(fallback.clone()));
    assert_eq!(cursor.current_offset, 16);
    assert_eq!(cursor.fallback_offset, 16);

    let mut formatter = DaemonLogStreamFormatter::default();
    let mut output = Vec::new();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&rotating)
        .unwrap()
        .write_all(b"after rotating\n")
        .unwrap();
    let mut offset = cursor.current_offset;
    assert!(
        write_daemon_log_delta(&rotating, &mut offset, &mut formatter, false, &mut output,)
            .unwrap()
    );
    formatter.finish(false, &mut output).unwrap();
    assert_eq!(String::from_utf8(output).unwrap(), "after rotating\n");
}

#[test]
fn daemon_log_delta_avoids_duplicates_across_append_rotation_and_truncation() {
    fn append(path: &Path, bytes: &[u8]) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(bytes).unwrap();
    }

    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("miyu.2026-07-28.log");
    let current = temp.path().join("miyu.2026-07-29.log");
    std::fs::write(&old, b"old partial").unwrap();

    let mut formatter = DaemonLogStreamFormatter::default();
    let mut output = Vec::new();
    let mut old_offset = 0;
    assert!(
        write_daemon_log_delta(&old, &mut old_offset, &mut formatter, false, &mut output,)
            .unwrap()
    );
    assert!(output.is_empty());
    append(&old, b" completed\nold tail\n");
    assert!(
        write_daemon_log_delta(&old, &mut old_offset, &mut formatter, false, &mut output,)
            .unwrap()
    );
    formatter.finish(false, &mut output).unwrap();

    std::fs::write(
        &current,
        b"2026-07-29T12:00:00Z  INFO miyu::qq: first\n  continuation\n",
    )
    .unwrap();
    let mut offset = 0;
    assert!(
        write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
            .unwrap()
    );
    assert!(
        !write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
            .unwrap()
    );

    append(&current, b"2026-07-29T12:00:01Z  INFO miyu::qq: \xe7\xbe");
    assert!(
        write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
            .unwrap()
    );
    append(&current, b"\xa4\xe8\x81\x8a\n");
    assert!(
        write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
            .unwrap()
    );

    append(&current, b"dangling");
    assert!(
        write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
            .unwrap()
    );
    std::fs::write(&current, b"reset\n").unwrap();
    assert!(
        write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
            .unwrap()
    );
    assert_eq!(offset, 6);

    let rendered = String::from_utf8(output).unwrap();
    assert!(!rendered.contains('\x1b'));
    assert_eq!(rendered.matches("old partial completed").count(), 1);
    assert_eq!(rendered.matches("old tail").count(), 1);
    assert_eq!(rendered.matches("[INFO] [miyu::qq] first").count(), 1);
    assert_eq!(rendered.matches("  continuation").count(), 1);
    assert_eq!(rendered.matches("[INFO] [miyu::qq] 群聊").count(), 1);
    assert_eq!(rendered.matches("dangling").count(), 1);
    assert_eq!(rendered.matches("reset").count(), 1);
}
