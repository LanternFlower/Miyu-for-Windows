//! 写入、删除与边界。

use crate::platforms::plugins::message_history::store::*;
use super::shared::*;

#[tokio::test]
async fn records_are_idempotent_isolated_and_sanitized() {
    let (_temp, store) = test_store();
    let first_group = group("bot-a", "group-1");
    let other_group = group("bot-a", "group-2");
    let other_account = group("bot-b", "group-1");
    let mut first = message(
        first_group.clone(),
        "m1",
        "u1",
        "Alice\nAdmin",
        " hello\0 world ",
        10,
    );
    first.content.media = vec![
        MediaPlaceholder::new(MediaKind::Image, Some(" cat\nphoto "), Some(" image/png ")),
        MediaPlaceholder::new(MediaKind::File, Some("notes.txt"), None::<String>),
    ];
    first.content.mentioned_user_ids = vec!["u2".to_string(), "u2".to_string()];
    first.content.mentioned_users = vec![PlatformMention {
        user_id: "u2".to_string(),
        display_name: Some("Yu\nyi".to_string()),
    }];

    let outcome = store.record_message(first.clone()).await.unwrap();
    assert!(outcome.inserted);
    let duplicate = store.record_message(first).await.unwrap();
    assert!(!duplicate.inserted);
    assert_eq!(outcome.row_id, duplicate.row_id);
    store
        .record_message(message(
            other_group.clone(),
            "m1",
            "u2",
            "Bob",
            "other group",
            11,
        ))
        .await
        .unwrap();
    store
        .record_message(message(
            other_account.clone(),
            "m1",
            "u3",
            "Carol",
            "other account",
            12,
        ))
        .await
        .unwrap();

    let page = store
        .recent(RecentQuery::for_history(first_group, 20))
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 1);
    let stored = &page.messages[0];
    assert_eq!(stored.sender_name, "Alice Admin");
    assert_eq!(stored.content.text, "hello world");
    assert_eq!(stored.content.media[0].label.as_deref(), Some("cat photo"));
    assert_eq!(stored.content.media[0].mime.as_deref(), Some("image/png"));
    assert_eq!(stored.content.mentioned_user_ids, vec!["u2"]);
    assert_eq!(stored.content.mentioned_users[0].user_id, "u2");
    assert_eq!(
        stored.content.mentioned_users[0].display_name.as_deref(),
        Some("Yu yi")
    );
    assert_eq!(stored.group.group_id(), "group-1");
}

#[tokio::test]
async fn context_ingress_boundary_excludes_current_and_future_messages() {
    let (_temp, store) = test_store();
    let key = group("bot-a", "group-1");
    let mut future = message(key.clone(), "future", "u3", "Future", "future", 10);
    future.ingress_order = Some(300);
    let mut previous = message(key.clone(), "previous", "u1", "Previous", "previous", 10);
    previous.ingress_order = Some(100);
    let mut current = message(key.clone(), "current", "u2", "Current", "current", 10);
    current.ingress_order = Some(200);

    // Deliberately persist in transport-opposite order to reproduce an
    // earlier message waiting on async metadata while a later one records.
    store
        .record_messages(vec![future, previous, current])
        .await
        .unwrap();

    let page = store
        .recent(RecentQuery::for_context(key, "default", 20).before_ingress_order(Some(200)))
        .await
        .unwrap();
    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["previous"]
    );
}

#[tokio::test]
async fn reset_boundary_only_changes_automatic_context() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    store
        .record_messages(vec![
            message(key.clone(), "m1", "u", "A", "before one", 10),
            message(key.clone(), "m2", "u", "A", "before two", 20),
        ])
        .await
        .unwrap();
    let boundary = store
        .reset_context(key.clone(), "default".to_string(), 25)
        .await
        .unwrap();
    assert_eq!(boundary.after_row_id, 2);
    store
        .record_message(message(key.clone(), "m3", "u", "A", "after reset", 30))
        .await
        .unwrap();

    let context = store
        .recent(RecentQuery::for_context(key.clone(), "default", 20))
        .await
        .unwrap();
    assert_eq!(
        context
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["m3"]
    );
    let history = store
        .recent(RecentQuery::for_history(key, 20))
        .await
        .unwrap();
    assert_eq!(history.messages.len(), 3);
    let other_persona = store
        .recent(RecentQuery::for_context(group("bot", "group"), "other", 20))
        .await
        .unwrap();
    assert_eq!(other_persona.messages.len(), 3);
}

#[tokio::test]
async fn recall_before_or_after_message_is_applied_and_hidden() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    let early = store
        .record_recall(NewRecall {
            group: key.clone(),
            message_id: "early".to_string(),
            operator_id: Some("moderator".to_string()),
            recalled_at: 12,
        })
        .await
        .unwrap();
    assert!(early.newly_recorded);
    assert!(!early.matched_message);
    store
        .record_messages(vec![
            message(key.clone(), "early", "u1", "A", "hidden early", 10),
            message(key.clone(), "late", "u2", "B", "hidden late", 20),
            message(key.clone(), "visible", "u3", "C", "visible", 30),
        ])
        .await
        .unwrap();
    let late = store
        .record_recall(NewRecall {
            group: key.clone(),
            message_id: "late".to_string(),
            operator_id: None,
            recalled_at: 22,
        })
        .await
        .unwrap();
    assert!(late.matched_message);

    let visible = store
        .recent(RecentQuery::for_history(key.clone(), 20))
        .await
        .unwrap();
    assert_eq!(visible.messages.len(), 1);
    assert_eq!(visible.messages[0].message_id, "visible");

    let mut with_recalls = RecentQuery::for_history(key, 20);
    with_recalls.include_recalled = true;
    let page = store.recent(with_recalls).await.unwrap();
    assert_eq!(page.messages.len(), 3);
    assert_eq!(page.messages[0].recalled_at, Some(12));
    assert_eq!(page.messages[1].recalled_at, Some(22));
}

#[tokio::test]
async fn retained_reset_boundary_does_not_hide_reused_rowids_after_cleanup() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    let day = SECONDS_PER_DAY;

    store
        .record_message(message(key.clone(), "before-reset", "u", "A", "old", day))
        .await
        .unwrap();
    let boundary = store
        .reset_context(key.clone(), "default".to_string(), day * 10)
        .await
        .unwrap();
    assert_eq!(boundary.after_row_id, 1);

    // The message is outside the retention window, while the reset itself
    // is recent enough to remain. Deleting the sole message lets SQLite
    // reuse rowid 1 for the next insert.
    store
        .delete_history(
            DeleteRequest::keep_days(HistoryScope::Group(key.clone()), 3, day * 10).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .context_boundary(key.clone(), "default".to_string())
            .await
            .unwrap()
            .unwrap()
            .after_row_id,
        0
    );

    let inserted = store
        .record_message(message(
            key.clone(),
            "after-cleanup",
            "u",
            "A",
            "new",
            day * 10,
        ))
        .await
        .unwrap();
    assert_eq!(inserted.row_id, 1);
    let context = store
        .recent(RecentQuery::for_context(key, "default", 20))
        .await
        .unwrap();
    assert_eq!(
        context
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["after-cleanup"]
    );
}

#[test]
fn opening_an_existing_database_repairs_a_stale_reset_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.db");
    let key = group("bot", "group");
    {
        let conn = open_database(&path).unwrap();
        conn.execute(
            "INSERT INTO context_boundaries (
                 platform, account_id, conversation_kind, conversation_id,
                 persona_scope, after_row_id, reset_at
             ) VALUES (?1, ?2, ?3, ?4, 'default', 99, 123)",
            params![
                key.platform(),
                key.account_id(),
                key.conversation_kind(),
                key.conversation_id()
            ],
        )
        .unwrap();
    }

    let conn = open_database(&path).unwrap();
    assert_eq!(
        read_boundary(&conn, &key, "default")
            .unwrap()
            .unwrap()
            .after_row_id,
        0
    );
}
