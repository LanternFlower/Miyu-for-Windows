use super::*;

/// kb 补丁三操作全链路:写入/更新/删除都必须落到 KnowledgeBase
/// (kb_meta.db 有行、文件在 kb 根下),而不是裸 fs 写。
#[tokio::test]
async fn kb_patch_routes_writes_through_the_knowledge_base() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::tools::tests::test_paths(temp.path());
    let config = miyu_base::config::AppConfig::default();
    let kb =
        crate::tools::knowledge_base::KnowledgeBase::new(config.clone(), paths.clone()).unwrap();
    kb.init().unwrap();

    let add = "*** Begin Patch\n*** Add File: notes/demo.md\n+# demo\n+hello kb\n*** End Patch";
    let output = apply_kb_patch(
        json!({ "patchText": add }),
        ToolProgress::default(),
        &config,
        &paths,
    )
    .unwrap();
    assert!(output.contains("kb:notes/demo.md"), "{output}");
    let stored = kb.safe_file_path("notes/demo.md").unwrap();
    assert_eq!(
        std::fs::read_to_string(&stored).unwrap(),
        "# demo\nhello kb\n"
    );

    let update = "*** Begin Patch\n*** Update File: notes/demo.md\n@@ # demo\n-hello kb\n+hello again\n*** End Patch";
    apply_kb_patch(
        json!({ "patchText": update }),
        ToolProgress::default(),
        &config,
        &paths,
    )
    .unwrap();
    assert!(std::fs::read_to_string(&stored)
        .unwrap()
        .contains("hello again"));

    let delete = "*** Begin Patch\n*** Delete File: notes/demo.md\n*** End Patch";
    apply_kb_patch(
        json!({ "patchText": delete }),
        ToolProgress::default(),
        &config,
        &paths,
    )
    .unwrap();
    assert!(!stored.exists());
}

/// edit 收到带域前缀的补丁必须指路而不是走错域。
#[test]
fn edit_rejects_prefixed_patches_with_a_pointer() {
    let patch = "*** Begin Patch\n*** Add File: kb:notes/x.md\n+x\n*** End Patch";
    let error = edit_filesystem(json!({ "patchText": patch }), ToolProgress::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("`kb` tool"), "{error}");
    let patch = "*** Begin Patch\n*** Add File: artifact:r.md\n+x\n*** End Patch";
    let error = edit_filesystem(json!({ "patchText": patch }), ToolProgress::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("`artifact` tool"), "{error}");
}

#[test]
fn parses_add_update_delete_patch() {
    let patch = "*** Begin Patch\n*** Add File: a.txt\n+hello\n*** Update File: b.txt\n@@ marker\n-old\n+new\n*** Delete File: c.txt\n*** End Patch";
    let operations = parse_patch_with(patch, &path_arg).unwrap();
    assert_eq!(operations.len(), 3);
}

#[test]
fn update_hunk_replaces_exact_text() {
    let path = PathBuf::from("demo.txt");
    let hunk = Hunk {
        context: None,
        end_of_file: false,
        lines: vec![
            HunkLine::Context("one".to_string()),
            HunkLine::Delete("two".to_string()),
            HunkLine::Insert("TWO".to_string()),
            HunkLine::Context("three".to_string()),
        ],
    };
    let result = apply_hunk(&path, "one\ntwo\nthree\n", &hunk).unwrap();
    assert_eq!(result, "one\nTWO\nthree\n");
}

#[test]
fn update_hunk_fails_when_stale() {
    let path = PathBuf::from("demo.txt");
    let hunk = Hunk {
        context: None,
        end_of_file: false,
        lines: vec![
            HunkLine::Delete("missing".to_string()),
            HunkLine::Insert("new".to_string()),
        ],
    };
    assert!(apply_hunk(&path, "current\n", &hunk).is_err());
}

#[test]
fn parses_fenced_patch_with_no_space_after_header_colon() {
    let patch = "```\n*** Begin Patch\n*** Add File:a.txt\n+hello\n*** End Patch\n```";
    let operations = parse_patch_with(patch, &path_arg).unwrap();
    assert_eq!(operations.len(), 1);
}

#[test]
fn insertion_hunk_uses_context_header() {
    let path = PathBuf::from("demo.txt");
    let hunk = Hunk {
        context: Some("one".to_string()),
        end_of_file: false,
        lines: vec![HunkLine::Insert("inserted".to_string())],
    };
    let result = apply_hunk(&path, "one\ntwo\n", &hunk).unwrap();
    assert_eq!(result, "one\ninserted\ntwo\n");
}

#[test]
fn apply_patch_adds_updates_and_deletes_files() {
    let temp = tempfile::tempdir().unwrap();
    let keep = temp.path().join("keep.txt");
    let remove = temp.path().join("remove.txt");
    std::fs::write(&keep, "one\ntwo\nthree\n").unwrap();
    std::fs::write(&remove, "delete me\n").unwrap();

    let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+new file\n*** Update File: {}\n@@ patch\n one\n-two\n+TWO\n three\n*** Delete File: {}\n*** End Patch",
            temp.path().join("new.txt").display(),
            keep.display(),
            remove.display()
        );
    let result = apply_patch(json!({ "patchText": patch }), ToolProgress::default()).unwrap();
    let data: Value = serde_json::from_str(&result).unwrap();

    assert_eq!(data["ok"], true);
    assert_eq!(data["files_changed"], 3);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("new.txt")).unwrap(),
        "new file\n"
    );
    assert_eq!(std::fs::read_to_string(&keep).unwrap(), "one\nTWO\nthree\n");
    assert!(!remove.exists());
}

#[test]
fn apply_patch_repeated_update_sections_use_staged_content() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("repeated.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

    let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n@@ first\n-one\n+ONE\n*** Update File: {}\n@@ second\n ONE\n-two\n+TWO\n three\n*** End Patch",
            file.display(),
            file.display()
        );
    let result = apply_patch(json!({ "patchText": patch }), ToolProgress::default()).unwrap();
    let data: Value = serde_json::from_str(&result).unwrap();

    assert_eq!(data["ok"], true);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "ONE\nTWO\nthree\n");
}

#[test]
fn apply_patch_rejects_move_to_until_supported() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.txt");
    let target = temp.path().join("target.txt");
    std::fs::write(&source, "old\n").unwrap();
    let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@ patch\n-old\n+new\n*** End Patch",
            source.display(),
            target.display()
        );

    assert!(apply_patch(json!({ "patchText": patch }), ToolProgress::default()).is_err());
    assert!(source.exists());
    assert!(!target.exists());
}

#[test]
fn artifact_patch_adds_and_updates_managed_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("artifacts");
    let session_dir = root.join("sess_test");
    std::fs::create_dir_all(&session_dir).unwrap();
    let report = session_dir.join("report.md");
    std::fs::write(&report, "# Report\n\nOld text.\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: report.md\n@@ report\n # Report\n \n-Old text.\n+Updated text.\n*** Add File: notes.txt\n+Follow up.\n*** End Patch";
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

    let output = apply_artifact_patch(
        json!({"patchText": patch}),
        ToolProgress::new(sender),
        &root,
        "sess_test",
    )
    .unwrap();
    let payload: Value = serde_json::from_str(&output).unwrap();

    assert_eq!(payload["operation"], "apply_artifact_patch");
    assert_eq!(payload["files_changed"], 2);
    assert_eq!(payload["files"][0]["path"], "report.md");
    assert!(!output.contains(temp.path().to_string_lossy().as_ref()));
    assert_eq!(
        std::fs::read_to_string(&report).unwrap(),
        "# Report\n\nUpdated text.\n"
    );
    let notes = session_dir.join("notes.txt");
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "Follow up.\n");
    for path in [&report, &notes] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let artifacts = std::iter::from_fn(|| receiver.try_recv().ok())
        .filter_map(|event| match event {
            super::super::ToolProgressEvent::Artifact { path, .. } => Some(path),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(artifacts, [report, notes]);
}

#[test]
fn artifact_patch_rejects_unsafe_paths_and_symlinks_but_allows_delete() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("artifacts");
    let session_dir = root.join("sess_test");
    std::fs::create_dir_all(&session_dir).unwrap();
    let report = session_dir.join("report.md");
    std::fs::write(&report, "original\n").unwrap();
    let outside = temp.path().join("outside.md");
    std::fs::write(&outside, "outside\n").unwrap();
    symlink(&outside, session_dir.join("link.md")).unwrap();

    for patch in [
        "*** Begin Patch\n*** Add File: ../escape.md\n+bad\n*** End Patch",
        "*** Begin Patch\n*** Add File: nested/file.md\n+bad\n*** End Patch",
        "*** Begin Patch\n*** Update File: link.md\n@@ patch\n-outside\n+changed\n*** End Patch",
        "*** Begin Patch\n*** Delete File: link.md\n*** End Patch",
    ] {
        assert!(apply_artifact_patch(
            json!({"patchText": patch}),
            ToolProgress::default(),
            &root,
            "sess_test",
        )
        .is_err());
    }
    assert_eq!(std::fs::read_to_string(&report).unwrap(), "original\n");
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside\n");
    assert!(!temp.path().join("escape.md").exists());

    // 验收四轮:Artifact 与本地补丁同语义,普通文件的 Delete File 放行。
    let deleted = apply_artifact_patch(
        json!({"patchText": "*** Begin Patch\n*** Delete File: report.md\n*** End Patch"}),
        ToolProgress::default(),
        &root,
        "sess_test",
    );
    assert!(deleted.is_ok(), "{deleted:?}");
    assert!(!report.exists());
}
