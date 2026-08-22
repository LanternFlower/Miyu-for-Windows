//! 日记的批次、晋升与清理。

use crate::memory::*;
use crate::config::AppConfig;
use super::shared::*;

#[test]
fn diary_batch_starts_only_at_the_configured_turn_count() {
    let temp = tempfile::tempdir().unwrap();
    let config = diary_config(14);
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    for index in 0..13 {
        assert!(record_turn(
            &store,
            &format!("问题 {index}"),
            &format!("回答 {index}")
        ));
    }
    assert!(store.next_organization_batch().unwrap().is_none());
    assert!(record_turn(&store, "第十四问", "第十四答"));
    let batch = store.next_organization_batch().unwrap().unwrap();
    assert_eq!(batch.diaries.len(), 14);
    assert_eq!(batch.diaries[0].origin.kind, "local");
    assert_eq!(batch.diaries[0].origin.session_id, "test-session");

    store
        .apply_organized_batch(
            &batch,
            OrganizedOutput {
                knowledge: Vec::new(),
                long_diaries: Vec::new(),
            },
        )
        .unwrap();
    let conn = store.data_conn().unwrap();
    assert_eq!(
        count_where(
            &conn,
            "episodes",
            "retention='short_term' AND consolidated_at IS NULL"
        )
        .unwrap(),
        0
    );
    assert_eq!(
        count_where(&conn, "episodes", "retention='short_term'").unwrap(),
        14
    );
}

#[test]
fn third_recall_requires_and_applies_long_diary_promotion() {
    let temp = tempfile::tempdir().unwrap();
    let config = diary_config(14);
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    assert!(record_turn(&store, "Wayland 输入法配置", "设置 XMODIFIERS"));
    for _ in 0..3 {
        assert!(store.association("Wayland 输入法", None).unwrap().is_some());
    }
    let batch = store.next_organization_batch().unwrap().unwrap();
    assert_eq!(batch.diaries.len(), 1);
    assert!(batch.diaries[0].force_long_term);
    let source_id = batch.diaries[0].id;
    store
        .apply_organized_batch(
            &batch,
            OrganizedOutput {
                knowledge: Vec::new(),
                long_diaries: vec![LongDiaryDraft {
                    content: "我曾帮助处理 Wayland 输入法配置。".to_string(),
                    importance: 3,
                    confidence: 0.9,
                    visibility: VISIBILITY_PRIVILEGED.to_string(),
                    subjects: Vec::new(),
                    tags: vec!["Wayland".to_string(), "输入法".to_string()],
                    diary_ids: vec![source_id],
                }],
            },
        )
        .unwrap();

    let conn = store.data_conn().unwrap();
    let (pending, promoted): (i64, Option<String>) = conn
        .query_row(
            "SELECT promotion_pending, promoted_at FROM episodes WHERE id=?1",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(pending, 0);
    assert!(promoted.is_some());
    assert_eq!(
        count_where(&conn, "episodes", "retention='long_term'").unwrap(),
        1
    );
}

#[test]
fn existing_episodes_migrate_as_long_term_diaries() {
    let temp = tempfile::tempdir().unwrap();
    let config = AppConfig::default();
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    std::fs::create_dir_all(store.data_db.parent().unwrap()).unwrap();
    let conn = Connection::open(&store.data_db).unwrap();
    conn.execute_batch(
        "CREATE TABLE episodes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'episode',
            status TEXT NOT NULL DEFAULT 'active',
            strength REAL NOT NULL DEFAULT 1.0,
            recall_count INTEGER NOT NULL DEFAULT 0,
            last_recalled_at TEXT,
            last_decay_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         INSERT INTO episodes (content, created_at, updated_at)
         VALUES ('旧版长期经历', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z');",
    )
    .unwrap();
    drop(conn);

    store.init().unwrap();
    let conn = store.data_conn().unwrap();
    assert_eq!(
        conn.query_row("SELECT retention FROM episodes", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        LONG_TERM
    );
    assert_eq!(count_rows(&conn, "episodes").unwrap(), 1);
}
