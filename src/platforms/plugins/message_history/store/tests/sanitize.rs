//! 入库前的清洗与上限。

use crate::platforms::plugins::message_history::store::*;
use super::shared::*;


fn private(account: &str, user_id: &str) -> ConversationKey {
    ConversationKey::for_kind("onebot", account, ConversationKind::Private, user_id).unwrap()
}



#[tokio::test]
async fn database_is_lazy_and_uses_bounded_sqlite_settings() {
    let (_temp, store) = test_store();
    assert!(!store.db_path().exists());

    assert!(store
        .recent(RecentQuery::for_context(group("1", "10"), "default", 20))
        .await
        .unwrap()
        .messages
        .is_empty());
    assert!(store.db_path().exists());

    let conn = Connection::open(store.db_path()).unwrap();
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let auto_vacuum: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    assert_eq!(auto_vacuum, 2);
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn version_one_database_migrates_with_nullable_ingress_order() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages (
             id INTEGER PRIMARY KEY,
             platform TEXT NOT NULL,
             account_id TEXT NOT NULL,
             group_id TEXT NOT NULL,
             message_id TEXT NOT NULL,
             sender_id TEXT NOT NULL,
             sender_name TEXT NOT NULL,
             text TEXT NOT NULL,
             media_json TEXT NOT NULL,
             mentions_json TEXT NOT NULL,
             reply_to_message_id TEXT,
             is_bot INTEGER NOT NULL,
             sent_at INTEGER NOT NULL,
             recalled_at INTEGER,
             recorded_at INTEGER NOT NULL DEFAULT (unixepoch()),
             UNIQUE (platform, account_id, group_id, message_id)
         );
         PRAGMA user_version = 1;",
    )
    .unwrap();

    migrate(&conn).unwrap();

    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let has_ingress_order = conn
        .prepare("PRAGMA table_info(messages)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .filter_map(Result::ok)
        .any(|column| column == "ingress_order");
    let has_conversation_kind = conn
        .prepare("PRAGMA table_info(messages)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .filter_map(Result::ok)
        .any(|column| column == "conversation_kind");
    assert_eq!(version, SCHEMA_VERSION);
    assert!(has_ingress_order);
    assert!(has_conversation_kind);
}

#[tokio::test]
async fn private_and_group_conversations_are_isolated_and_filterable() {
    let (_temp, store) = test_store();
    let private_key = private("bot", "42");
    let group_key = group("bot", "42");
    store
        .record_message(message(
            private_key.clone(),
            "same-id",
            "42",
            "Alice",
            "private first",
            10,
        ))
        .await
        .unwrap();
    store
        .record_message(message(
            private_key.clone(),
            "private-2",
            "7",
            "Bob",
            "private second",
            20,
        ))
        .await
        .unwrap();
    store
        .record_message(message(
            group_key.clone(),
            "same-id",
            "42",
            "Alice",
            "group message",
            15,
        ))
        .await
        .unwrap();

    let private_page = store
        .search(SearchQuery::new(
            HistoryScope::Private(private_key.clone()),
            "private",
            20,
        ))
        .await
        .unwrap();
    assert_eq!(private_page.messages.len(), 2);
    assert!(private_page
        .messages
        .iter()
        .all(|message| message.group == private_key));

    let account_page = store
        .search(SearchQuery::new(
            HistoryScope::Account(private_key.account_scope()),
            "",
            20,
        ))
        .await
        .unwrap();
    assert_eq!(account_page.messages.len(), 3);
    assert!(account_page
        .messages
        .iter()
        .any(|message| message.group == group_key));
    assert!(account_page
        .messages
        .iter()
        .any(|message| message.group == private_key));

    let mut request = DeleteRequest::all(HistoryScope::Private(private_key.clone()), 30);
    request.sender_id = Some("42".to_string());
    request.since = Some(10);
    request.until = Some(10);
    let report = store.delete_history(request).await.unwrap();
    assert_eq!(report.messages_deleted, 1);
    let remaining_private = store
        .recent(RecentQuery::for_history(private_key, 20))
        .await
        .unwrap();
    assert_eq!(remaining_private.messages.len(), 1);
    assert_eq!(remaining_private.messages[0].message_id, "private-2");
    assert_eq!(
        store
            .recent(RecentQuery::for_history(group_key, 20))
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
}

#[tokio::test]
async fn the_reply_window_can_start_after_what_a_previous_turn_already_showed() {
    let (_temp, store) = test_store();
    let key = group("bot-a", "group-1");
    let mut first = message(key.clone(), "m1", "u1", "One", "已经发过", 10);
    first.ingress_order = Some(100);
    let mut second = message(key.clone(), "m2", "u2", "Two", "也发过", 10);
    second.ingress_order = Some(200);
    let mut third = message(key.clone(), "m3", "u3", "Three", "新到的", 10);
    third.ingress_order = Some(300);
    store
        .record_messages(vec![first, second, third])
        .await
        .unwrap();

    // Everything up to the watermark is already sitting in the replayed
    // conversation history, so the turn only carries what arrived since.
    let page = store
        .recent(
            RecentQuery::for_context(key.clone(), "default", 20).after_ingress_order(Some(200)),
        )
        .await
        .unwrap();
    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        ["m3"]
    );

    // No watermark yet — the first turn of a conversation still gets a full
    // opening snapshot.
    let page = store
        .recent(RecentQuery::for_context(key, "default", 20))
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 3);
}

#[tokio::test]
async fn context_history_is_ordered_by_transport_ingress() {
    let (_temp, store) = test_store();
    let key = group("bot-a", "group-1");
    let mut first = message(key.clone(), "first", "u1", "First", "first", 30);
    first.ingress_order = Some(100);
    let mut second = message(key.clone(), "second", "u2", "Second", "second", 10);
    second.ingress_order = Some(200);
    let mut third = message(key.clone(), "third", "u3", "Third", "third", 20);
    third.ingress_order = Some(300);

    store
        .record_messages(vec![third, first, second])
        .await
        .unwrap();

    let page = store
        .recent(RecentQuery::for_context(key, "default", 20).before_ingress_order(Some(400)))
        .await
        .unwrap();
    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second", "third"]
    );
}

#[tokio::test]
async fn explicit_deletion_is_batched_and_does_not_cross_scope() {
    let (_temp, store) = test_store();
    let first = group("bot", "first");
    let second = group("bot", "second");
    let other_account = group("other-bot", "first");
    let day = SECONDS_PER_DAY;
    store
        .record_messages(vec![
            message(first.clone(), "old1", "u", "A", "old one", day),
            message(first.clone(), "old2", "u", "A", "old two", day * 2),
            message(first.clone(), "new", "u", "A", "new", day * 9),
            message(second.clone(), "same-account", "u", "A", "keep", day),
            message(
                other_account.clone(),
                "other-account",
                "u",
                "A",
                "keep",
                day,
            ),
        ])
        .await
        .unwrap();
    store
        .reset_context(first.clone(), "default".to_string(), day * 2)
        .await
        .unwrap();
    store
        .record_recall(NewRecall {
            group: first.clone(),
            message_id: "old1".to_string(),
            operator_id: None,
            recalled_at: day * 2,
        })
        .await
        .unwrap();

    let mut request =
        DeleteRequest::keep_days(HistoryScope::Group(first.clone()), 3, day * 10).unwrap();
    request.batch_size = 1;
    let report = store.delete_history(request).await.unwrap();
    assert_eq!(report.messages_deleted, 2);
    assert_eq!(report.recalls_deleted, 1);
    assert_eq!(report.boundaries_deleted, 1);
    assert!(report.batches >= 3);

    let first_page = store
        .recent(RecentQuery::for_history(first.clone(), 20))
        .await
        .unwrap();
    assert_eq!(first_page.messages.len(), 1);
    assert_eq!(first_page.messages[0].message_id, "new");
    assert!(store
        .context_boundary(first.clone(), "default".to_string())
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .recent(RecentQuery::for_history(second.clone(), 20))
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    assert_eq!(
        store
            .recent(RecentQuery::for_history(other_account.clone(), 20))
            .await
            .unwrap()
            .messages
            .len(),
        1
    );

    let all = store
        .delete_history(DeleteRequest::all(
            HistoryScope::Group(first.clone()),
            day * 10,
        ))
        .await
        .unwrap();
    assert_eq!(all.messages_deleted, 1);
    assert!(store
        .recent(RecentQuery::for_history(first, 20))
        .await
        .unwrap()
        .messages
        .is_empty());

    let account_scope = HistoryScope::Account(second.account_scope());
    let account_search = store
        .search(SearchQuery::new(account_scope.clone(), "keep", 20))
        .await
        .unwrap();
    assert_eq!(account_search.messages.len(), 1);
    assert_eq!(account_search.messages[0].group, second);
    let account_report = store
        .delete_history(DeleteRequest::all(account_scope, day * 10))
        .await
        .unwrap();
    assert_eq!(account_report.messages_deleted, 1);
    assert_eq!(
        store
            .recent(RecentQuery::for_history(other_account, 20))
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
}

#[test]
fn identifiers_and_keep_days_are_validated() {
    assert!(GroupKey::new("onebot", "", "group").is_err());
    assert!(GroupKey::new("onebot", "bot", "bad\ngroup").is_err());
    let scope = HistoryScope::Account(AccountKey::new("onebot", "bot").unwrap());
    assert!(DeleteRequest::keep_days(scope, 0, 0).is_err());
}
