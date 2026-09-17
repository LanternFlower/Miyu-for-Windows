//! 进度标记流的游标算术：`job_trace_after`。
//!
//! 终端的后台子代理面板据它逐条跟（IPC `Command::JobTrace`），不再每 150ms 重读
//! 整份日志。游标是**绝对序号**，而缓冲是环形的（`MAX_TRACE` 条封顶）——「我上次
//! 读到的位置还在不在缓冲里」这件事算错的话，中间那几条会静静消失、面板少一段
//! 过程，而且**没有任何报错**。所以这一段单独钉住。

use super::shared::shared_init;
use crate::tools::jobs;

/// 取整份、接着取、取到头。
#[test]
fn the_cursor_walks_forward_and_stops_at_the_end() {
    shared_init();
    let job_id = jobs::register_test_job("trace-walk");
    for i in 0..5 {
        jobs::publish_job_progress(&job_id, &format!("__subagent_reasoning__第 {i} 段"));
    }

    // `after = 0`：整份。游标停在 5。
    let (markers, cursor, reset) = jobs::job_trace_after(&job_id, 0).expect("任务在");
    assert_eq!(markers.len(), 5);
    assert_eq!(cursor, 5);
    assert!(!reset, "没挤掉东西，不该要求重来");

    // 接着取：还没有新的，空一份，游标不动。
    let (markers, cursor, reset) = jobs::job_trace_after(&job_id, cursor).expect("任务在");
    assert!(markers.is_empty(), "没有新标记却回了东西: {markers:?}");
    assert_eq!(cursor, 5);
    assert!(!reset);

    // 来了两条新的：只回新的那两条。
    for i in 5..7 {
        jobs::publish_job_progress(&job_id, &format!("__subagent_reasoning__第 {i} 段"));
    }
    let (markers, cursor, _) = jobs::job_trace_after(&job_id, cursor).expect("任务在");
    assert_eq!(markers.len(), 2, "回的不是增量: {markers:?}");
    assert!(markers[0].ends_with("第 5 段"));
    assert_eq!(cursor, 7);
}

/// 游标比缓冲里最早那条还靠前 = 中间丢过东西，得整份重来。
///
/// 不报这一声的话调用方会把「缓冲里现存的这一段」直接接在旧的后面，中间少掉的那
/// 几步谁都不知道——面板上就是一段过程凭空消失。
#[test]
fn a_cursor_older_than_the_buffer_asks_for_a_reset() {
    shared_init();
    let job_id = jobs::register_test_job("trace-reset");
    jobs::publish_job_progress(&job_id, "__subagent_reasoning__第一段");
    // 假装环形缓冲已经把前面 1000 条挤掉了。
    jobs::force_trace_dropped_for_test(&job_id, 1000);

    let (markers, cursor, reset) = jobs::job_trace_after(&job_id, 3).expect("任务在");
    assert!(reset, "游标已经被挤掉了，却没要求重来");
    assert_eq!(markers.len(), 1, "重来时该回缓冲里现存的全部: {markers:?}");
    assert_eq!(cursor, 1001, "游标要按绝对序号走");

    // 已经追上之后就不再要求重来。
    let (markers, _, reset) = jobs::job_trace_after(&job_id, cursor).expect("任务在");
    assert!(!reset);
    assert!(markers.is_empty());
}

/// 任务不在 daemon 里（跑完清掉、daemon 重启过）：`None`，让面板退回读日志。
#[test]
fn a_job_that_is_gone_reports_nothing() {
    shared_init();
    assert!(jobs::job_trace_after("no-such-job", 0).is_none());
}
