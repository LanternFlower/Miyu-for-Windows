//! 平台绑定、授权与插件存储的并发正确性。

use crate::state::*;
use super::shared::*;

#[test]
fn platform_access_grants_are_cached_persisted_and_audited() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let store = StateStore::new(&paths).unwrap();
    let peer = StateStore::new(&paths).unwrap();
    let key = PlatformAccessGrantKey {
        platform: "onebot".to_string(),
        account_scope: GLOBAL_PLATFORM_ACCOUNT_SCOPE.to_string(),
        permission: "private_whitelist".to_string(),
        subject_kind: "user".to_string(),
        subject_id: "2477342916".to_string(),
    };
    let actor = PlatformAccessActor {
        platform: "onebot".to_string(),
        account_id: "10000".to_string(),
        user_id: "42".to_string(),
        conversation_kind: "private".to_string(),
        conversation_id: "42".to_string(),
        message_id: "message-1".to_string(),
    };

    assert!(store.add_platform_access_grant(&key, &actor).unwrap());
    assert!(!store.add_platform_access_grant(&key, &actor).unwrap());
    assert!(store.has_platform_access_grant(
        "onebot",
        "10000",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert!(peer.has_platform_access_grant(
        "onebot",
        "10000",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert!(store.has_platform_access_grant(
        "onebot",
        "another-bot",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert_eq!(store.platform_access_grants("onebot").unwrap().len(), 1);

    let reopened = StateStore::new(&paths).unwrap();
    assert!(reopened.has_platform_access_grant(
        "onebot",
        "20000",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert!(reopened.remove_platform_access_grant(&key, &actor).unwrap());
    assert!(!reopened.remove_platform_access_grant(&key, &actor).unwrap());
    assert!(!reopened.has_platform_access_grant(
        "onebot",
        "10000",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert!(!store.has_platform_access_grant(
        "onebot",
        "10000",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert!(!peer.has_platform_access_grant(
        "onebot",
        "10000",
        "private_whitelist",
        "user",
        "2477342916"
    ));

    let denied_key = PlatformAccessGrantKey {
        subject_id: "99".to_string(),
        ..key.clone()
    };
    let denied = store
        .mutate_platform_access_grant_if_authorized(
            &denied_key,
            &actor,
            PlatformAccessMutation::Grant,
            &PlatformAccessAuthorization {
                statically_authorized: false,
                dynamic_key: PlatformAccessGrantKey {
                    platform: "onebot".to_string(),
                    account_scope: "10000".to_string(),
                    permission: "administrator".to_string(),
                    subject_kind: "user".to_string(),
                    subject_id: "42".to_string(),
                },
            },
        )
        .unwrap();
    assert_eq!(denied, PlatformAccessMutationResult::Unauthorized);
    assert!(!store.has_platform_access_grant(
        "onebot",
        "10000",
        "private_whitelist",
        "user",
        "99"
    ));

    let conn = rusqlite::Connection::open(paths.state_dir.join("conversation.db")).unwrap();
    let audit_count: i64 = conn
        .query_row("SELECT count(*) FROM platform_access_audit", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(audit_count, 2);
}


#[test]
fn platform_bindings_survive_rename_and_isolate_personas() {
    let (_temp, store) = test_store();
    let miyu_session = store
        .create_session("miyu", "old display name", "user", None)
        .unwrap();
    let other_session = store
        .create_session("other", "another display name", "user", None)
        .unwrap();
    let miyu_key = platform_binding_key("20000", None, "miyu");
    let other_key = platform_binding_key("20000", None, "other");

    store
        .bind_platform_session(&miyu_key, &miyu_session.session_id)
        .unwrap();
    store
        .bind_platform_session(&other_key, &other_session.session_id)
        .unwrap();
    store
        .rename_session(&miyu_session.session_id, "new display name")
        .unwrap();

    assert_eq!(
        store.find_platform_session_binding(&miyu_key).unwrap(),
        Some(miyu_session.session_id.clone())
    );
    // `None` and an empty participant are the same database identity.
    let empty_participant_key = platform_binding_key("20000", Some(""), "miyu");
    assert_eq!(
        store
            .find_platform_session_binding(&empty_participant_key)
            .unwrap(),
        Some(miyu_session.session_id.clone())
    );
    assert_eq!(
        store.find_platform_session_binding(&other_key).unwrap(),
        Some(other_session.session_id)
    );

    store.delete_session(&miyu_session.session_id).unwrap();
    assert_eq!(
        store.find_platform_session_binding(&miyu_key).unwrap(),
        None
    );
}

#[test]
fn platform_binding_overwrite_and_conflict_are_atomic() {
    let (_temp, store) = test_store();
    let session_a = store.create_session("miyu", "a", "user", None).unwrap();
    let session_b = store.create_session("miyu", "b", "user", None).unwrap();
    let session_c = store.create_session("miyu", "c", "user", None).unwrap();
    let key_a = platform_binding_key("group-a", None, "miyu");
    let key_b = platform_binding_key("group-b", None, "miyu");

    store
        .bind_platform_session(&key_a, &session_a.session_id)
        .unwrap();
    store
        .bind_platform_session(&key_b, &session_b.session_id)
        .unwrap();

    let error = store
        .bind_platform_session(&key_a, &session_b.session_id)
        .unwrap_err();
    assert!(error.to_string().contains("already bound"));
    assert_eq!(
        store.find_platform_session_binding(&key_a).unwrap(),
        Some(session_a.session_id)
    );
    assert_eq!(
        store.find_platform_session_binding(&key_b).unwrap(),
        Some(session_b.session_id)
    );

    store
        .bind_platform_session(&key_a, &session_c.session_id)
        .unwrap();
    assert_eq!(
        store.find_platform_session_binding(&key_a).unwrap(),
        Some(session_c.session_id)
    );
    assert!(store.unbind_platform_session(&key_a).unwrap());
    assert!(!store.unbind_platform_session(&key_a).unwrap());
}

#[test]
fn concurrent_platform_bind_rejects_session_sharing() {
    let (temp, store) = test_store();
    let second_store = StateStore::new(&test_paths(temp.path())).unwrap();
    let session = store
        .create_session("miyu", "shared target", "user", None)
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let handles = [store.clone(), second_store]
        .into_iter()
        .zip(["group-a", "group-b"])
        .map(|(store, conversation_id)| {
            let barrier = barrier.clone();
            let session_id = session.session_id.clone();
            let key = platform_binding_key(conversation_id, None, "miyu");
            std::thread::spawn(move || {
                barrier.wait();
                let result = store.bind_platform_session(&key, &session_id);
                (key, result)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_err()).count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|(key, _)| store.find_platform_session_binding(key).unwrap().is_some())
            .count(),
        1
    );
}

#[test]
fn concurrent_platform_claim_converges_on_one_session() {
    let (temp, store) = test_store();
    let second_store = StateStore::new(&test_paths(temp.path())).unwrap();
    let session_a = store.create_session("miyu", "a", "user", None).unwrap();
    let session_b = store.create_session("miyu", "b", "user", None).unwrap();
    let key = platform_binding_key("same-group", None, "miyu");
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let handles = [
        (store.clone(), session_a.session_id.clone()),
        (second_store, session_b.session_id.clone()),
    ]
    .into_iter()
    .map(|(store, candidate)| {
        let barrier = barrier.clone();
        let key = key.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.claim_platform_session(&key, &candidate).unwrap()
        })
    })
    .collect::<Vec<_>>();
    barrier.wait();
    let winners = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(winners[0], winners[1]);
    assert_eq!(
        store.find_platform_session_binding(&key).unwrap(),
        Some(winners[0].clone())
    );
    assert!(winners[0] == session_a.session_id || winners[0] == session_b.session_id);
}

#[test]
fn platform_session_creation_is_bound_atomically() {
    let (_temp, store) = test_store();
    let key = platform_binding_key("atomic-group", None, "miyu");
    let (platform, created) = store
        .create_or_get_platform_session(&key, "platform")
        .unwrap();
    assert!(created);
    assert_eq!(
        store.find_platform_session_binding(&key).unwrap(),
        Some(platform.session_id.clone())
    );
    assert!(!store
        .list_local_sessions("miyu")
        .unwrap()
        .iter()
        .any(|entry| entry.record.session_id == platform.session_id));

    let (same, created) = store
        .create_or_get_platform_session(&key, "ignored")
        .unwrap();
    assert!(!created);
    assert_eq!(same.session_id, platform.session_id);
}

#[test]
fn platform_plugin_json_is_shared_across_personas_and_supports_deletion() {
    let (_temp, store) = test_store();
    let scope = plugin_scope("20000");
    let value = vec!["image-a".to_string(), "image-b".to_string()];
    store
        .plugin_put_json(&scope, "recent_images", &value)
        .unwrap();
    let replacement = vec!["image-c".to_string()];
    store
        .plugin_put_json(&scope, "recent_images", &replacement)
        .unwrap();

    // Pinned stores represent independent persona sessions but share the
    // external-conversation plugin scope.
    let miyu_session = store.create_session("miyu", "miyu", "user", None).unwrap();
    let other_session = store
        .create_session("other", "other", "user", None)
        .unwrap();
    let miyu_store = store.pinned(&miyu_session.session_id);
    let other_store = store.pinned(&other_session.session_id);
    let from_miyu: Option<Vec<String>> =
        miyu_store.plugin_get_json(&scope, "recent_images").unwrap();
    let from_other: Option<Vec<String>> = other_store
        .plugin_get_json(&scope, "recent_images")
        .unwrap();
    assert_eq!(from_miyu, Some(replacement.clone()));
    assert_eq!(from_other, Some(replacement));

    store.plugin_put_json(&scope, "mode", &"image").unwrap();
    assert!(store.plugin_delete_key(&scope, "recent_images").unwrap());
    let deleted: Option<Vec<String>> = store.plugin_get_json(&scope, "recent_images").unwrap();
    assert_eq!(deleted, None);
    assert_eq!(store.plugin_delete_scope(&scope).unwrap(), 1);
    assert!(!store.plugin_delete_key(&scope, "mode").unwrap());
}

#[test]
fn concurrent_platform_plugin_updates_do_not_lose_values() {
    let (temp, first) = test_store();
    let second = StateStore::new(&test_paths(temp.path())).unwrap();
    let scope = plugin_scope("atomic-group");
    let barrier = Arc::new(std::sync::Barrier::new(9));
    let handles = (0..8)
        .map(|value| {
            let store = if value % 2 == 0 {
                first.clone()
            } else {
                second.clone()
            };
            let scope = scope.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .plugin_update_json(&scope, "values", |current: Option<Vec<usize>>| {
                        let mut values = current.unwrap_or_default();
                        values.push(value);
                        Ok(Some(values))
                    })
                    .unwrap();
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for handle in handles {
        handle.join().unwrap();
    }

    let mut values: Vec<usize> = first.plugin_get_json(&scope, "values").unwrap().unwrap();
    values.sort_unstable();
    assert_eq!(values, (0..8).collect::<Vec<_>>());
}


#[test]
fn platform_meme_refs_are_ordered_isolated_upserted_and_cleaned_by_ref() {
    let (_temp, store) = test_store();
    let later = platform_meme_ref(
        "group-a",
        "message-1",
        "secondary",
        "meme-b",
        "outbound",
        "2026-01-02T00:00:00Z",
    );
    let earlier = platform_meme_ref(
        "group-a",
        "message-1",
        "default",
        "meme-a",
        "inbound",
        "2026-01-01T00:00:00Z",
    );
    let other_conversation = platform_meme_ref(
        "group-b",
        "message-1",
        "default",
        "meme-a",
        "inbound",
        "2026-01-03T00:00:00Z",
    );
    store.put_platform_meme_ref(&later).unwrap();
    store.put_platform_meme_ref(&earlier).unwrap();
    store.put_platform_meme_ref(&other_conversation).unwrap();

    assert_eq!(
        store
            .platform_meme_refs_for_message("onebot", "10000", "group", "group-a", "message-1")
            .unwrap(),
        vec![earlier.clone(), later]
    );

    let mut updated = earlier;
    updated.direction = "outbound".to_string();
    updated.created_at = "2026-01-04T00:00:00Z".to_string();
    store.put_platform_meme_ref(&updated).unwrap();
    let records = store
        .platform_meme_refs_for_message("onebot", "10000", "group", "group-a", "message-1")
        .unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1], updated);

    assert_eq!(
        store.delete_platform_meme_ref("default", "meme-a").unwrap(),
        2
    );
    assert!(store
        .platform_meme_refs_for_message("onebot", "10000", "group", "group-b", "message-1")
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .platform_meme_refs_for_message("onebot", "10000", "group", "group-a", "message-1")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn platform_meme_ref_rejects_invalid_direction() {
    let (_temp, store) = test_store();
    let record = platform_meme_ref(
        "group-a",
        "message-1",
        "default",
        "meme-a",
        "sideways",
        "2026-01-01T00:00:00Z",
    );
    assert!(store.put_platform_meme_ref(&record).is_err());
    assert!(store
        .platform_meme_refs_for_message("onebot", "10000", "group", "group-a", "message-1")
        .unwrap()
        .is_empty());
}
