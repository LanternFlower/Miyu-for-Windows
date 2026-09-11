//! 消息热度记账。

use crate::platforms::*;

/// 回归(PR#31):sender_messages 每个唯一发送者一条、永不清,
/// 常驻 daemon 慢速泄漏;超限清表后计数重新起算、总数不受影响。
#[test]
fn message_activity_sender_counts_are_bounded() {
    let registry = MessageActivityRegistry::default();
    let now = Instant::now();
    // 持有一个 handle 保住 scope 的 Arc,循环里的弱引用才升级得到。
    let (_keep, _, _) = registry.observe("onebot:1:group:9", "m0", "sender-0", now);
    for i in 1..MESSAGE_ACTIVITY_SENDER_LIMIT {
        registry.observe(
            "onebot:1:group:9",
            &format!("m{i}"),
            &format!("sender-{i}"),
            now,
        );
    }
    // 表满;新面孔触发清表,计数从 1 起
    let (_, newcomer, _) = registry.observe("onebot:1:group:9", "mx1", "newcomer", now);
    assert_eq!(newcomer.sender_messages, 1);
    // 老发送者被清,回到 1 而不是 2;总量计数不受清表影响
    let (_, again, _) = registry.observe("onebot:1:group:9", "mx2", "sender-0", now);
    assert_eq!(again.sender_messages, 1);
    assert_eq!(
        again.total_messages,
        (MESSAGE_ACTIVITY_SENDER_LIMIT + 2) as u64
    );
}

#[test]
fn message_activity_counts_other_senders_and_deduplicates_events() {
    let registry = MessageActivityRegistry::default();
    let now = Instant::now();
    let (activity, start, _) = registry.observe("onebot:1:group:2", "m1", "alice", now);
    assert_eq!(start.total_messages, 1);
    assert_eq!(start.sender_messages, 1);

    let (_, first_other, first_received_at) =
        registry.observe("onebot:1:group:2", "m2", "bob", now);
    let (_, duplicate, duplicate_received_at) = registry.observe(
        "onebot:1:group:2",
        "m2",
        "bob",
        now + Duration::from_secs(10),
    );
    assert_eq!(duplicate, first_other);
    assert_eq!(duplicate_received_at, first_received_at);
    registry.observe("onebot:1:group:2", "m3", "alice", now);

    let current = activity.position_for("alice");
    assert_eq!(current.total_messages, 3);
    assert_eq!(current.sender_messages, 2);
    let other_messages = current
        .total_messages
        .saturating_sub(start.total_messages)
        .saturating_sub(
            current
                .sender_messages
                .saturating_sub(start.sender_messages),
        );
    assert_eq!(other_messages, 1);

    let (_, isolated, _) = registry.observe("onebot:1:group:3", "m4", "bob", now);
    assert_eq!(isolated.total_messages, 1);
}

/// 跨回合近似闸(08-25 并发重复答题):同会话高相似判重、低相似放行、
/// 跨会话隔离、短文本不记账。
#[test]
fn recent_conversation_reply_similarity_gate() {
    let text = "拯救者装Linux能跑，但体验是七分完美三分折腾。核显日用没问题，想用独显打游戏就得跟NVIDIA驱动搏斗，风扇和性能模式控制是硬伤。";
    record_recent_conversation_reply("gate-scope-a", text);
    let paraphrase = "拯救者装Linux完全能跑，但体验就是七分完美三分折腾，核显日用没有问题，想用独显打游戏就得跟NVIDIA驱动搏斗，风扇和性能模式控制是硬伤。";
    assert!(recent_conversation_reply_similar(
        "gate-scope-a",
        paraphrase
    ));
    assert!(!recent_conversation_reply_similar(
        "gate-scope-a",
        "今晚吃火锅还是日料？随便挑一个，别纠结太久。"
    ));
    assert!(!recent_conversation_reply_similar(
        "gate-scope-b",
        paraphrase
    ));
    // 短文本不记账也不判重。
    record_recent_conversation_reply("gate-scope-c", "好。");
    assert!(!recent_conversation_reply_similar("gate-scope-c", "好。"));
}

/// 移除台账(08-25 踢人假失败):同会话记账可查、跨会话隔离、空 id 不记。
#[test]
fn group_removal_ledger_scopes_and_records() {
    record_group_removal("kick-scope-a", "10001");
    assert!(recent_group_removal("kick-scope-a", "10001"));
    assert!(!recent_group_removal("kick-scope-a", "10002"));
    assert!(!recent_group_removal("kick-scope-b", "10001"));
    record_group_removal("kick-scope-a", "");
    assert!(!recent_group_removal("kick-scope-a", ""));
}
