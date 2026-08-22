//! 查询：最近、搜索、活跃度、分页。

use crate::platforms::plugins::message_history::store::*;
use super::shared::*;

#[tokio::test]
async fn activity_ranking_is_scoped_stable_and_counts_recalled_messages() {
    let (_temp, store) = test_store();
    let key = group("bot-a", "group-1");
    let other_group = group("bot-a", "group-2");
    let other_account = group("bot-b", "group-1");
    let first_day = SECONDS_PER_DAY * 10 + 43_200;
    let second_day = first_day + SECONDS_PER_DAY * 2;
    let mut bot_one = message(
        key.clone(),
        "bot-1",
        "bot-alias-1",
        "Miyu old",
        "bot",
        second_day + 20,
    );
    bot_one.is_bot = true;
    let mut bot_two = message(
        key.clone(),
        "bot-2",
        "bot-alias-2",
        "Miyu",
        "bot",
        second_day + 30,
    );
    bot_two.is_bot = true;
    store
        .record_messages(vec![
            message(key.clone(), "a-1", "1", "Alice old", "one", first_day),
            message(key.clone(), "a-2", "1", "Alice", "two", second_day + 10),
            message(
                key.clone(),
                "a-3",
                "1",
                "Alice newest",
                "three",
                second_day + 40,
            ),
            message(key.clone(), "b-1", "2", "Bob", "one", first_day + 10),
            message(key.clone(), "b-2", "2", "Bob", "two", second_day + 20),
            bot_one,
            bot_two,
            message(
                other_group,
                "other-group",
                "3",
                "Other",
                "ignored",
                second_day,
            ),
            message(
                other_account,
                "other-account",
                "4",
                "Other",
                "ignored",
                second_day,
            ),
        ])
        .await
        .unwrap();
    store
        .record_recall(NewRecall {
            group: key.clone(),
            message_id: "a-1".to_string(),
            operator_id: Some("1".to_string()),
            recalled_at: first_day + 100,
        })
        .await
        .unwrap();

    let ranking = store
        .activity_ranking(ActivityRankingQuery {
            group: key.clone(),
            since: first_day,
            until: second_day + 100,
            limit: 2,
            include_bot: true,
        })
        .await
        .unwrap();
    assert_eq!(ranking.total_messages, 7);
    assert_eq!(ranking.participant_count, 3);
    assert_eq!(ranking.items.len(), 2);
    assert_eq!(ranking.items[0].sender_id, "1");
    assert_eq!(ranking.items[0].sender_name, "Alice newest");
    assert_eq!(ranking.items[0].message_count, 3);
    assert_eq!(ranking.items[0].active_days, 2);
    assert_eq!(ranking.items[1].sender_id, "bot-a");
    assert_eq!(ranking.items[1].sender_name, "Miyu");
    assert_eq!(ranking.items[1].rank, 2);

    let without_bot = store
        .activity_ranking(ActivityRankingQuery {
            group: key,
            since: first_day,
            until: second_day + 100,
            limit: usize::MAX,
            include_bot: false,
        })
        .await
        .unwrap();
    assert_eq!(without_bot.total_messages, 5);
    assert_eq!(without_bot.participant_count, 2);
    assert_eq!(without_bot.items.len(), 2);
    assert_eq!(without_bot.items[1].sender_id, "2");
}

#[tokio::test]
async fn activity_ranking_validates_time_range_and_includes_both_boundaries() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    store
        .record_messages(vec![
            message(key.clone(), "before", "1", "A", "before", 9),
            message(key.clone(), "start", "1", "A", "start", 10),
            message(key.clone(), "end", "2", "B", "end", 20),
            message(key.clone(), "after", "2", "B", "after", 21),
        ])
        .await
        .unwrap();

    let result = store
        .activity_ranking(ActivityRankingQuery {
            group: key.clone(),
            since: 10,
            until: 20,
            limit: 20,
            include_bot: true,
        })
        .await
        .unwrap();
    assert_eq!(result.total_messages, 2);
    assert_eq!(result.participant_count, 2);
    assert!(store
        .activity_ranking(ActivityRankingQuery {
            group: key,
            since: 20,
            until: 10,
            limit: 20,
            include_bot: true,
        })
        .await
        .is_err());
}

#[tokio::test]
async fn fts_search_is_safe_paginated_and_capped_at_one_thousand() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    for batch_start in (0..1_005).step_by(MAX_BATCH_MESSAGES) {
        let end = (batch_start + MAX_BATCH_MESSAGES).min(1_005);
        let batch = (batch_start..end)
            .map(|index| {
                message(
                    key.clone(),
                    format!("m{index}"),
                    "u",
                    "Search User",
                    format!("needle item {index}"),
                    index as i64,
                )
            })
            .collect();
        store.record_messages(batch).await.unwrap();
    }
    store
        .record_message(message(
            key.clone(),
            "chinese",
            "u",
            "中文用户",
            "今天天气很好",
            1_000,
        ))
        .await
        .unwrap();

    let first = store
        .search(SearchQuery::new(
            HistoryScope::Group(key.clone()),
            "needle",
            usize::MAX,
        ))
        .await
        .unwrap();
    assert_eq!(first.messages.len(), MAX_PAGE_SIZE);
    assert!(first.next_cursor.is_some());
    let mut second_query =
        SearchQuery::new(HistoryScope::Group(key.clone()), "needle", MAX_PAGE_SIZE);
    second_query.before = first.next_cursor;
    let second = store.search(second_query).await.unwrap();
    assert_eq!(second.messages.len(), 5);
    assert!(second.next_cursor.is_none());

    let quoted = store
        .search(SearchQuery::new(
            HistoryScope::Group(key.clone()),
            "needle \"item\"",
            10,
        ))
        .await;
    assert!(quoted.is_ok());

    let chinese_trigram = store
        .search(SearchQuery::new(
            HistoryScope::Group(key.clone()),
            "天气很",
            10,
        ))
        .await
        .unwrap();
    assert_eq!(chinese_trigram.messages[0].message_id, "chinese");
    let chinese_short_fallback = store
        .search(SearchQuery::new(HistoryScope::Group(key), "天气", 10))
        .await
        .unwrap();
    assert_eq!(chinese_short_fallback.messages[0].message_id, "chinese");
}

#[tokio::test]
async fn search_can_filter_recent_messages_by_sender_id() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    store
        .record_messages(vec![
            message(key.clone(), "a1", "10001", "A", "first", 1),
            message(key.clone(), "b1", "10002", "B", "other", 2),
            message(key.clone(), "a2", "10001", "A", "second", 3),
            message(key.clone(), "a3", "10001", "A", "third", 4),
        ])
        .await
        .unwrap();

    let mut query = SearchQuery::new(HistoryScope::Group(key), "", 10);
    query.sender_id = Some("10001".to_string());
    let page = store.search(query).await.unwrap();

    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a3", "a2", "a1"]
    );
}

#[tokio::test]
async fn history_pages_are_limited_by_message_count_only() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    let large_text = format!("needle {}", "x".repeat(60 * 1024));
    let messages = (0..10)
        .map(|index| {
            message(
                key.clone(),
                format!("large-{index}"),
                "u",
                "Search User",
                large_text.clone(),
                index,
            )
        })
        .collect();
    store.record_messages(messages).await.unwrap();

    let page = store
        .search(SearchQuery::new(
            HistoryScope::Group(key),
            "needle",
            MAX_PAGE_SIZE,
        ))
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 10);
    assert!(page.next_cursor.is_none());
}
