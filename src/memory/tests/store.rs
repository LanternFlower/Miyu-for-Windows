//! 存取、检索与重置。

use crate::memory::*;
use crate::config::AppConfig;
use super::shared::*;

#[test]
fn evicted_search_is_indexed_and_can_be_narrowed_by_time() {
    let temp = tempfile::tempdir().unwrap();
    let store = MemoryStore::new(&AppConfig::default(), &test_paths(&temp));
    store.init().unwrap();
    let rows: Vec<EvictedTurn> = (0..1200)
        .map(|index| EvictedTurn {
            source_id: format!("t{index}:user"),
            timestamp: format!("2026-08-{:02}T10:00:00+00:00", (index % 28) + 1),
            role: "user".to_string(),
            content: format!("第 {index} 轮，聊到了 蓝色小刺猬 这个话题"),
            ..EvictedTurn::default()
        })
        .collect();
    store.remember_evicted_turns(&rows).unwrap();

    // The scan used to stop at the newest 1000 rows, so anything older was
    // stored forever and reachable never.
    let oldest = store
        .search_evicted_context_readonly("第 3 轮", 50, None, None)
        .unwrap();
    assert!(
        oldest["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["snippet"]
                .as_str()
                .unwrap_or_default()
                .contains("第 3 轮")),
        "{oldest}"
    );

    // "What were we talking about that morning" is a question about when.
    let ranged = store
        .search_evicted_context_readonly(
            "蓝色小刺猬",
            50,
            Some("2026-08-05T00:00:00+00:00"),
            Some("2026-08-05T23:59:59+00:00"),
        )
        .unwrap();
    let hits = ranged["results"].as_array().unwrap();
    assert!(!hits.is_empty(), "{ranged}");
    assert!(
        hits.iter().all(|hit| hit["timestamp"]
            .as_str()
            .unwrap_or_default()
            .starts_with("2026-08-05")),
        "{ranged}"
    );
}

#[test]
fn compact_jieba_matches_reference_segmentation() {
    let reference = jieba_rs::Jieba::new();
    for input in [
        "我们中出了一个叛徒",
        "Wayland 输入法需要 XMODIFIERS",
        "Niri窗口规则和中文输入法配置",
        "podman-compose 不能直接重新创建容器",
        "北京烤鸭真好吃，后天天气不好。",
        "Rust 2024 edition与C++20",
    ] {
        assert_eq!(
            JIEBA.cut(input),
            reference.cut(input, false),
            "segmentation differs for {input}"
        );
    }
}

#[test]
fn remembers_and_recalls_fact() {
    let temp = tempfile::tempdir().unwrap();
    let config = AppConfig::default();
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    store
        .remember_fact("Niri 输入法需要 XMODIFIERS", "test")
        .unwrap();
    let result = store.recall_memories("Niri XMODIFIERS", 5, false).unwrap();
    assert!(result.to_string().contains("XMODIFIERS"));
}

#[test]
fn evicted_context_uses_the_same_principal_filter() {
    let temp = tempfile::tempdir().unwrap();
    let config = AppConfig::default();
    let paths = test_paths(&temp);
    let origin_a = platform_origin("7", "Alice");
    let origin_b = platform_origin("8", "Bob");
    let user_a = scoped_store(&config, &paths, &origin_a, false);
    let user_b = scoped_store(&config, &paths, &origin_b, false);
    user_a
        .remember_evicted_turns(&[EvictedTurn {
            source_id: "a:user".to_string(),
            timestamp: "now".to_string(),
            role: "user".to_string(),
            content: "淘汰记忆 Alice 专属".to_string(),
            ..EvictedTurn::default()
        }])
        .unwrap();
    user_b
        .remember_evicted_turns(&[EvictedTurn {
            source_id: "b:user".to_string(),
            timestamp: "now".to_string(),
            role: "user".to_string(),
            content: "淘汰记忆 Bob 专属".to_string(),
            ..EvictedTurn::default()
        }])
        .unwrap();

    let a = user_a
        .search_evicted_context("淘汰记忆", 10)
        .unwrap()
        .to_string();
    assert!(a.contains("Alice 专属"));
    assert!(!a.contains("Bob 专属"));
    let all = MemoryStore::new(&config, &paths)
        .search_evicted_context("淘汰记忆", 10)
        .unwrap()
        .to_string();
    assert!(all.contains("Alice 专属"));
    assert!(all.contains("Bob 专属"));
}

#[test]
fn reset_all_clears_facts_and_episodes() {
    let temp = tempfile::tempdir().unwrap();
    let config = AppConfig::default();
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    store
        .remember_fact("Niri 输入法需要 XMODIFIERS", "test")
        .unwrap();
    store.remember_pending_event("你好", "在呢").unwrap();
    store.flush_pending_events().unwrap();

    let before = store.recall_memories("你好 XMODIFIERS", 5, false).unwrap();
    assert!(!before["facts"].as_array().unwrap().is_empty());
    assert!(!before["episodes"].as_array().unwrap().is_empty());

    store.reset_all(false).unwrap();

    let after = store.recall_memories("你好 XMODIFIERS", 5, false).unwrap();
    assert!(after["facts"].as_array().unwrap().is_empty());
    assert!(after["episodes"].as_array().unwrap().is_empty());
}

#[test]
fn evicted_context_can_be_cleared() {
    let temp = tempfile::tempdir().unwrap();
    let config = AppConfig::default();
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    store
        .remember_evicted_turns(&[EvictedTurn {
            source_id: "turn-1:user".to_string(),
            timestamp: "now".to_string(),
            role: "user".to_string(),
            content: "旧上下文 输入法".to_string(),
            ..EvictedTurn::default()
        }])
        .unwrap();
    store
        .remember_evicted_turns(&[EvictedTurn {
            source_id: "turn-1:user".to_string(),
            timestamp: "now".to_string(),
            role: "user".to_string(),
            content: "旧上下文 输入法".to_string(),
            ..EvictedTurn::default()
        }])
        .unwrap();
    assert_eq!(
        store.search_evicted_context("输入法", 5).unwrap()["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_evicted_context("输入法", 5)
        .unwrap()
        .to_string()
        .contains("旧上下文"));
    store.clear_evicted_context().unwrap();
    assert!(!store
        .search_evicted_context("输入法", 5)
        .unwrap()
        .to_string()
        .contains("旧上下文"));
}

#[test]
fn reset_all_invalidates_an_inflight_organization_batch() {
    let temp = tempfile::tempdir().unwrap();
    let config = diary_config(2);
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    assert!(record_turn(&store, "问题一", "回答一"));
    assert!(record_turn(&store, "问题二", "回答二"));
    let batch = store.next_organization_batch().unwrap().unwrap();
    let stale_database_id = batch.database_id.clone();
    let stale_generation = batch.generation;

    store.reset_all(false).unwrap();
    assert!(!store
        .process_after_turn(
            "重置前启动的问题",
            "不应写回",
            &test_origin(),
            &stale_database_id,
            stale_generation,
        )
        .unwrap());
    assert!(store
        .apply_organized_batch(
            &batch,
            OrganizedOutput {
                knowledge: Vec::new(),
                long_diaries: Vec::new(),
            },
        )
        .is_err());
    let conn = store.data_conn().unwrap();
    assert_eq!(count_rows(&conn, "facts").unwrap(), 0);
    assert_eq!(count_rows(&conn, "episodes").unwrap(), 0);
}

#[test]
fn cleanup_deletes_only_expired_consolidated_short_diaries() {
    let temp = tempfile::tempdir().unwrap();
    let config = diary_config(2);
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    store.init().unwrap();
    let conn = store.data_conn().unwrap();
    conn.execute(
        "INSERT INTO episodes (
            content, source, status, created_at, updated_at, retention,
            expires_at, consolidated_at
         ) VALUES ('expired', 'episode', 'active', ?1, ?1, 'short_term', ?1, ?1)",
        ["2020-01-01T00:00:00Z"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO episodes (
            content, source, status, created_at, updated_at, retention,
            expires_at, consolidated_at
         ) VALUES ('pending', 'episode', 'active', ?1, ?1, 'short_term', ?1, NULL)",
        ["2020-01-01T00:00:00Z"],
    )
    .unwrap();
    drop(conn);

    assert_eq!(store.cleanup_expired_short_diaries().unwrap(), 1);
    let conn = store.data_conn().unwrap();
    assert_eq!(count_rows(&conn, "episodes").unwrap(), 1);
    assert_eq!(
        conn.query_row("SELECT content FROM episodes", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "pending"
    );
    assert_eq!(
        conn.query_row("SELECT status FROM episodes", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "forgotten"
    );
}

#[test]
fn organizer_never_recreates_a_moved_persona_database() {
    let temp = tempfile::tempdir().unwrap();
    let config = diary_config(2);
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    assert!(record_turn(&store, "问题一", "回答一"));
    assert!(record_turn(&store, "问题二", "回答二"));
    let batch = store.next_organization_batch().unwrap().unwrap();
    let memory_dir = store.data_db.parent().unwrap().to_path_buf();
    let moved_dir = memory_dir.with_file_name("memory-moved");
    std::fs::rename(&memory_dir, &moved_dir).unwrap();

    assert!(store.next_organization_batch().unwrap().is_none());
    assert!(!memory_dir.exists());
    assert!(store
        .apply_organized_batch(
            &batch,
            OrganizedOutput {
                knowledge: Vec::new(),
                long_diaries: Vec::new(),
            },
        )
        .is_err());
    assert!(!memory_dir.exists());

    store.init().unwrap();
    assert!(store
        .apply_organized_batch(
            &batch,
            OrganizedOutput {
                knowledge: Vec::new(),
                long_diaries: Vec::new(),
            },
        )
        .is_err());
}
