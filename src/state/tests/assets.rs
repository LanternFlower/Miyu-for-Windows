//! 附件与产物资产。

use crate::state::*;
use super::shared::*;

#[test]
fn user_attachment_moves_from_staged_to_turn_and_cascades() {
    let (_temp, store) = test_store();
    let attachment = UserAttachment {
        attachment_id: "att_test".to_string(),
        file_name: "notes.md".to_string(),
        mime: "text/markdown".to_string(),
        kind: "text".to_string(),
        size_bytes: 7,
        width: 0,
        height: 0,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    store.save_user_attachment(&attachment, b"content").unwrap();
    assert_eq!(
        store
            .load_staged_user_attachments(&[attachment.attachment_id.clone()])
            .unwrap()[0]
            .bytes,
        b"content"
    );

    store
        .reserve_user_attachments(&[attachment.attachment_id.clone()], "run_test")
        .unwrap();
    store
        .start_turn_with_display(
            "turn_test",
            "visible\n\n<user-attachment>content</user-attachment>",
            "visible",
            std::process::id(),
            Some("run_test"),
        )
        .unwrap();
    let turns = store.load_turns().unwrap();
    assert_eq!(turns[0].display_content, "visible");
    assert_eq!(turns[0].attachments, vec![attachment.clone()]);
    assert!(store
        .load_staged_user_attachments(&[attachment.attachment_id.clone()])
        .is_err());

    store.reset_conversation().unwrap();
    assert!(store
        .load_user_attachment_by_id(&attachment.attachment_id)
        .unwrap()
        .is_none());
}

#[test]
fn image_assets_persist_with_metadata_and_are_removed_with_history() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    store.start_turn("turn_image", "show it", 999999).unwrap();
    let path = temp.path().join("sample.png");
    image::RgbaImage::from_pixel(3, 2, image::Rgba([30, 120, 210, 255]))
        .save(&path)
        .unwrap();

    let saved = store
        .save_image_asset("turn_image", Some("tool_1"), &path, "sample image")
        .unwrap();
    assert_eq!(saved.mime, "image/png");
    assert_eq!((saved.width, saved.height), (3, 2));
    assert_eq!(store.load_image_assets().unwrap(), vec![saved.clone()]);
    let loaded = store.load_image_asset(&saved.asset_id).unwrap().unwrap();
    assert_eq!(loaded.asset, saved);
    assert!(!loaded.bytes.is_empty());

    store.reset_conversation().unwrap();
    assert!(store.load_image_assets().unwrap().is_empty());
    assert!(store
        .load_image_asset(&loaded.asset.asset_id)
        .unwrap()
        .is_none());
}

#[test]
fn artifact_assets_update_in_place_and_are_removed_with_history() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    store
        .start_turn("turn_artifact", "build it", 999999)
        .unwrap();
    let path = temp.path().join("report.md");
    std::fs::write(&path, "# First\n").unwrap();
    let managed_dir = temp
        .path()
        .join("data/artifacts")
        .join(store.session_id().as_ref());
    std::fs::create_dir_all(&managed_dir).unwrap();
    std::fs::write(managed_dir.join("managed.md"), "# Managed\n").unwrap();

    let first = store
        .save_artifact_asset("turn_artifact", Some("tool_1"), &path, "Report")
        .unwrap();
    assert_eq!(first.kind, "markdown");
    assert_eq!(first.file_name, "Report");

    std::fs::write(&path, "# Updated\n").unwrap();
    let updated = store
        .save_artifact_asset("turn_artifact", Some("tool_2"), &path, "Updated report")
        .unwrap();
    assert_eq!(updated.asset_id, first.asset_id);
    assert_eq!(store.load_artifact_assets().unwrap(), vec![updated.clone()]);
    let loaded = store
        .load_artifact_asset(&updated.asset_id)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.bytes, b"# Updated\n");

    store.reset_conversation().unwrap();
    assert!(!managed_dir.exists());
    assert!(store.load_artifact_assets().unwrap().is_empty());
    assert!(store
        .load_artifact_asset(&updated.asset_id)
        .unwrap()
        .is_none());
}

#[test]
fn managed_artifact_keeps_its_identity_across_turns() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    let managed_dir = temp
        .path()
        .join("data/artifacts")
        .join(store.session_id().as_ref());
    std::fs::create_dir_all(&managed_dir).unwrap();
    let path = managed_dir.join("report.md");

    store.start_turn("turn_one", "first", 999999).unwrap();
    std::fs::write(&path, "# First\n").unwrap();
    let first = store
        .save_artifact_asset("turn_one", Some("tool_one"), &path, "Report")
        .unwrap();
    store.complete_turn("turn_one", "done", None).unwrap();

    store.start_turn("turn_two", "update", 999999).unwrap();
    std::fs::write(&path, "# Updated\n").unwrap();
    let updated = store
        .save_artifact_asset("turn_two", Some("tool_two"), &path, "Report")
        .unwrap();

    assert_eq!(updated.asset_id, first.asset_id);
    assert_eq!(updated.turn_id, "turn_two");
    assert_eq!(store.load_artifact_assets().unwrap(), vec![updated.clone()]);
    assert_eq!(
        store
            .load_artifact_asset(&updated.asset_id)
            .unwrap()
            .unwrap()
            .bytes,
        b"# Updated\n"
    );
}
