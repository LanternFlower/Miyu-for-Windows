//! 联想段的生成、去重与格式。

use crate::memory::*;
use crate::config::AppConfig;
use super::shared::*;

#[test]
fn unrelated_and_rejected_memories_are_not_associated() {
    let temp = tempfile::tempdir().unwrap();
    let config = AppConfig::default();
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    let rejected = store.remember_fact("旧的错误结论", "test").unwrap();
    store
        .data_conn()
        .unwrap()
        .execute(
            "UPDATE facts SET truth_status='rejected' WHERE id=?1",
            [rejected],
        )
        .unwrap();
    assert!(store.association("完全无关的主题", None).unwrap().is_none());
    assert!(store.association("错误结论", None).unwrap().is_none());
}

#[test]
fn association_format_always_keeps_its_closing_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = AppConfig::default();
    config.plugins.memory.association_max_chars = 128;
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    let hit = MemoryHit {
        id: 1,
        kind: MemoryKind::Fact,
        content: "很长的知识点".repeat(100),
        score: 1.0,
        timestamp: now(),
        source: "test".to_string(),
        retention: None,
        visibility: VISIBILITY_PUBLIC.to_string(),
        owner_principal: String::new(),
        owner_display_name: String::new(),
        subjects: "[]".to_string(),
        source_episode_ids: Vec::new(),
        origin_session_id: String::new(),
    };
    let formatted = store.format_association(&AssociationContext {
        facts: vec![hit],
        episodes: Vec::new(),
        organization_due: false,
    });
    assert!(formatted.ends_with("</associative-memory>"));
    assert!(formatted.chars().count() <= 128);
}

#[test]
fn association_lines_carry_date_and_dedupe_diary_timestamp() {
    let temp = tempfile::tempdir().unwrap();
    let config = AppConfig::default();
    let paths = test_paths(&temp);
    let store = MemoryStore::new(&config, &paths);
    let stamp = now();
    let date = association_date(&stamp).unwrap();
    let base = MemoryHit {
        id: 1,
        kind: MemoryKind::Fact,
        content: "知识点内容".to_string(),
        score: 1.0,
        timestamp: stamp.clone(),
        source: "test".to_string(),
        retention: None,
        visibility: VISIBILITY_PUBLIC.to_string(),
        owner_principal: String::new(),
        owner_display_name: String::new(),
        subjects: "[]".to_string(),
        source_episode_ids: Vec::new(),
        origin_session_id: String::new(),
    };
    let diary = MemoryHit {
        id: 2,
        kind: MemoryKind::Diary,
        content: format!("{stamp}，对方说：测试；我回：通过"),
        retention: Some(SHORT_TERM.to_string()),
        ..base.clone()
    };
    let formatted = store.format_association(&AssociationContext {
        facts: vec![base],
        episodes: vec![diary],
        organization_due: false,
    });
    assert!(formatted.contains(&format!("[{date}] [公共知识] 知识点内容")));
    assert!(formatted.contains(&format!("[{date}] [公共知识] 对方说：测试；我回：通过")));
    assert!(!formatted.contains(&stamp));
}

#[test]
fn diary_content_reads_as_a_first_person_exchange() {
    let content = diary_content(
        "2026-08-10T12:00:00+00:00",
        "wps 保存文件默认的编码是gbk吗",
        "分情况：纯文本默认 GBK，docx 内部是 UTF-8",
    );
    assert_eq!(
        content,
        "2026-08-10T12:00:00+00:00，对方说：wps 保存文件默认的编码是gbk吗；我回：分情况：纯文本默认 GBK，docx 内部是 UTF-8"
    );
}
