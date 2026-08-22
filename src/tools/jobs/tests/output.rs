//! 日志读取、截断与完成结论。

use crate::tools::jobs::*;
use super::shared::*;

/// 尾部读取只取最后 N 行,超长行截断,前面还有内容时开头补 `…`。
#[test]
fn log_tail_keeps_the_last_lines_and_clips_long_ones() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("j.log");

    // 行数不足时全给,且开头不加省略号。
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let (tail, size) = read_log_tail(&path, 10);
    assert_eq!(tail, "one\ntwo\n");
    assert_eq!(size, 8);
    assert!(!tail.starts_with('…'));

    // 超出时只留最后几行,并标出前面还有。
    let many = (1..=20).map(|n| format!("line{n}")).collect::<Vec<_>>().join("\n");
    std::fs::write(&path, &many).unwrap();
    let (tail, _) = read_log_tail(&path, 3);
    assert!(tail.starts_with("…\n"), "{tail:?}");
    assert!(tail.contains("line18\nline19\nline20"), "{tail:?}");
    assert!(!tail.contains("line17"), "{tail:?}");

    // 单行超长要截断,不能把额度吃光。
    std::fs::write(&path, "x".repeat(MAX_TAIL_LINE_CHARS * 3)).unwrap();
    let (tail, _) = read_log_tail(&path, 10);
    assert!(tail.chars().count() <= MAX_TAIL_LINE_CHARS + 2, "{}", tail.chars().count());
    assert!(tail.trim_end().ends_with('…'));

    // 日志不存在时是空的,不是报错。
    let (tail, size) = read_log_tail(&dir.path().join("missing.log"), 10);
    assert!(tail.is_empty());
    assert_eq!(size, 0);
}

/// 子代理的结论是交付物,必须整段取回、不截断——截了模型还得回头读日志。
#[test]
fn completion_result_returns_the_whole_subagent_conclusion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("k.log");
    let conclusion = format!("{{\"answer\":\"{}\"}}", "长".repeat(20_000));
    std::fs::write(
        &path,
        format!("过程日志第一行\n第二行\n\n{SUBAGENT_RESULT_MARKER}\n{conclusion}\n"),
    )
    .unwrap();

    let (label, body) = completion_result(&path, true, true).unwrap();
    assert_eq!(label, "子代理结论");
    assert_eq!(body, conclusion, "结论不能被截断");
    assert!(!body.contains("过程日志"), "只要标记之后的部分");
}

#[test]
fn completion_result_recognises_a_failed_subagent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("k.log");
    std::fs::write(&path, format!("…\n{SUBAGENT_ERROR_MARKER}\nmodel refused\n")).unwrap();
    let (label, body) = completion_result(&path, true, false).unwrap();
    assert_eq!(label, "子代理失败");
    assert_eq!(body, "model refused");
}

/// 命令没有「结论」,日志尾部就是结果;失败时给得多,因为报错根因常在上面几行。
#[test]
fn completion_result_gives_a_command_more_lines_when_it_failed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.log");
    let body = (1..=40).map(|n| format!("line{n}")).collect::<Vec<_>>().join("\n");
    std::fs::write(&path, &body).unwrap();

    let (label, ok_tail) = completion_result(&path, false, true).unwrap();
    assert_eq!(label, "输出结尾");
    assert!(ok_tail.contains("line40") && !ok_tail.contains("line30"));

    let (_, err_tail) = completion_result(&path, false, false).unwrap();
    assert!(err_tail.contains("line11"), "失败时要往上多给一些");
    assert!(err_tail.lines().count() > ok_tail.lines().count());
}

/// 日志为空时不硬塞一段空白进唤醒。
#[test]
fn completion_result_is_absent_when_there_is_nothing_to_show() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.log");
    std::fs::write(&path, "").unwrap();
    assert!(completion_result(&path, false, true).is_none());
    assert!(completion_result(&path, true, true).is_none());
    assert!(completion_result(&dir.path().join("missing.log"), true, true).is_none());
}

/// 任务越多每条给得越少,总量有界。
#[test]
fn tail_budget_shrinks_as_jobs_pile_up() {
    assert_eq!(tail_lines_for(1), 10);
    assert_eq!(tail_lines_for(6), 10);
    assert_eq!(tail_lines_for(7), 5);
    assert_eq!(tail_lines_for(15), 5);
    assert_eq!(tail_lines_for(40), 3);
}

#[tokio::test]
async fn incremental_output_reads_from_offset() {
    shared_init();
    #[cfg(unix)]
    let command = "printf 'AAABBB'";
    #[cfg(windows)]
    let command = "echo|set /p=AAABBB";
    let spawned: Value =
        serde_json::from_str(&spawn_background(command, None, &test_progress()).await.unwrap()).unwrap();
    let job_id = spawned["job_id"].as_str().unwrap().to_string();
    await_terminal(&job_id).await;
    let first: Value = serde_json::from_str(
        &job_status(json!({"job_id": job_id})).await.unwrap(),
    )
    .unwrap();
    assert_eq!(first["output"]["content"], "AAABBB");
    let second: Value = serde_json::from_str(
        &job_status(json!({"job_id": job_id, "offset": 3})).await.unwrap(),
    )
    .unwrap();
    assert_eq!(second["output"]["content"], "BBB");
}
