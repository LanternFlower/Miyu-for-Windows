//! 消息历史测试共用的 fixture。

use crate::platforms::plugins::message_history::store::*;
use tempfile::TempDir;

pub(super) fn group(account: &str, group_id: &str) -> GroupKey {
    GroupKey::new("onebot", account, group_id).unwrap()
}

pub(super) fn message(
    group: GroupKey,
    message_id: impl Into<String>,
    sender_id: &str,
    sender_name: &str,
    text: impl Into<String>,
    sent_at: i64,
) -> NewHistoryMessage {
    NewHistoryMessage {
        group,
        message_id: message_id.into(),
        sender_id: sender_id.to_string(),
        sender_name: sender_name.to_string(),
        content: SanitizedContent::new(text, Vec::new()),
        reply_to_message_id: None,
        is_bot: false,
        sent_at,
        ingress_order: None,
    }
}

pub(super) fn test_store() -> (TempDir, HistoryStore) {
    let temp = tempfile::tempdir().unwrap();
    let store = HistoryStore::new(temp.path().join("nested/group_history.db"));
    (temp, store)
}
