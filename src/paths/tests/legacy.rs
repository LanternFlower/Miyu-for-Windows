//! 旧布局迁移与符号链接处置。

use crate::paths::*;
use super::shared::*;

#[test]
fn migration_moves_and_merges_without_overwriting() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("legacy");
    let destination = temp.path().join("next");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::create_dir_all(&destination).unwrap();
    fs::write(source.join("nested/value.txt"), "old").unwrap();
    fs::write(destination.join("kept.txt"), "new").unwrap();

    migrate_entry(&source, &destination).unwrap();

    assert!(!source.exists());
    assert_eq!(
        fs::read_to_string(destination.join("nested/value.txt")).unwrap(),
        "old"
    );
    assert_eq!(
        fs::read_to_string(destination.join("kept.txt")).unwrap(),
        "new"
    );
}

#[test]
fn migration_preflights_conflicts_then_removes_identical_duplicates() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("legacy");
    let destination = temp.path().join("next");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&destination).unwrap();
    fs::write(source.join("same.txt"), "same").unwrap();
    fs::write(destination.join("same.txt"), "same").unwrap();
    fs::write(source.join("conflict.txt"), "legacy").unwrap();
    fs::write(destination.join("conflict.txt"), "next").unwrap();

    let error = migrate_entry(&source, &destination).unwrap_err();

    assert!(source.join("same.txt").exists());
    assert!(source.join("conflict.txt").exists());
    assert_eq!(
        fs::read_to_string(destination.join("conflict.txt")).unwrap(),
        "next"
    );
    assert!(error.to_string().contains("conflicting entries"));

    fs::remove_file(source.join("conflict.txt")).unwrap();
    migrate_entry(&source, &destination).unwrap();
    assert!(!source.exists());
    assert_eq!(
        fs::read_to_string(destination.join("same.txt")).unwrap(),
        "same"
    );
}

#[test]
fn unified_layout_merges_xdg_documents_and_both_picture_directories() {
    let temp = tempfile::tempdir().unwrap();
    let (legacy, next) = test_layouts(temp.path());
    for directory in [
        &legacy.config_dir,
        &legacy.data_dir,
        &legacy.cache_dir,
        &legacy.state_dir,
        &legacy.documents_dir,
        &legacy.pictures_dirs[0],
        &legacy.pictures_dirs[1],
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::create_dir_all(legacy.data_dir.join("documents")).unwrap();
    fs::create_dir_all(legacy.data_dir.join("pictures")).unwrap();
    fs::create_dir_all(legacy.config_dir.join("shell")).unwrap();
    fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
    fs::write(legacy.config_dir.join("shell/bash-hook.sh"), "bash hook").unwrap();
    fs::write(legacy.config_dir.join("shell/zsh-hook.zsh"), "zsh hook").unwrap();
    fs::write(
        temp.path().join(".bashrc"),
        "before\n# >>> miyu bash hook >>>\nsource '/legacy/bash-hook.sh'\n# <<< miyu bash hook <<<\nafter\n",
    )
    .unwrap();
    fs::write(
        temp.path().join(".zshrc"),
        "before\n# >>> miyu zsh hook >>>\nsource '/legacy/zsh-hook.zsh'\n# <<< miyu zsh hook <<<\nafter\n",
    )
    .unwrap();
    fs::write(legacy.data_dir.join("data.bin"), "data").unwrap();
    fs::write(legacy.cache_dir.join("cache.bin"), "cache").unwrap();
    fs::write(legacy.state_dir.join("state.db"), "state").unwrap();
    fs::write(legacy.documents_dir.join("report.md"), "report").unwrap();
    fs::write(
        legacy.data_dir.join("documents/shared.txt"),
        "same document",
    )
    .unwrap();
    fs::write(legacy.documents_dir.join("shared.txt"), "same document").unwrap();
    fs::write(legacy.data_dir.join("pictures/shared.png"), "same picture").unwrap();
    fs::write(legacy.pictures_dirs[0].join("shared.png"), "same picture").unwrap();
    fs::write(legacy.pictures_dirs[0].join("lower.png"), "lower").unwrap();
    fs::write(legacy.pictures_dirs[1].join("shared.png"), "same picture").unwrap();
    fs::write(legacy.pictures_dirs[1].join("upper.png"), "upper").unwrap();

    migrate_legacy_layout(&legacy, &next).unwrap();

    for source in [
        &legacy.config_dir,
        &legacy.data_dir,
        &legacy.cache_dir,
        &legacy.state_dir,
        &legacy.documents_dir,
        &legacy.pictures_dirs[0],
        &legacy.pictures_dirs[1],
    ] {
        assert!(
            !source.exists(),
            "legacy source remains: {}",
            source.display()
        );
    }
    assert_eq!(
        fs::read_to_string(next.config_dir.join("config.jsonc")).unwrap(),
        "config"
    );
    assert_eq!(
        fs::read_to_string(next.config_dir.join("shell/bash-hook.sh")).unwrap(),
        "bash hook"
    );
    assert_eq!(
        fs::read_to_string(next.config_dir.join("shell/zsh-hook.zsh")).unwrap(),
        "zsh hook"
    );
    let bashrc = fs::read_to_string(temp.path().join(".bashrc")).unwrap();
    assert!(bashrc.contains(
        next.config_dir
            .join("shell/bash-hook.sh")
            .to_string_lossy()
            .as_ref()
    ));
    assert!(!bashrc.contains("/legacy/bash-hook.sh"));
    let zshrc = fs::read_to_string(temp.path().join(".zshrc")).unwrap();
    assert!(zshrc.contains(
        next.config_dir
            .join("shell/zsh-hook.zsh")
            .to_string_lossy()
            .as_ref()
    ));
    assert!(!zshrc.contains("/legacy/zsh-hook.zsh"));
    assert_eq!(
        fs::read_to_string(next.data_dir.join("data.bin")).unwrap(),
        "data"
    );
    assert_eq!(
        fs::read_to_string(next.cache_dir.join("cache.bin")).unwrap(),
        "cache"
    );
    assert_eq!(
        fs::read_to_string(next.state_dir.join("state.db")).unwrap(),
        "state"
    );
    assert_eq!(
        fs::read_to_string(next.data_dir.join("documents/report.md")).unwrap(),
        "report"
    );
    assert_eq!(
        fs::read_to_string(next.data_dir.join("documents/shared.txt")).unwrap(),
        "same document"
    );
    assert_eq!(
        fs::read_to_string(next.data_dir.join("pictures/shared.png")).unwrap(),
        "same picture"
    );
    assert_eq!(
        fs::read_to_string(next.data_dir.join("pictures/lower.png")).unwrap(),
        "lower"
    );
    assert_eq!(
        fs::read_to_string(next.data_dir.join("pictures/upper.png")).unwrap(),
        "upper"
    );
    assert!(next.marker().exists());
}

#[cfg(unix)]
#[test]
fn unified_layout_discards_cache_with_relative_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let (legacy, next) = test_layouts(temp.path());
    fs::create_dir_all(&legacy.config_dir).unwrap();
    fs::create_dir_all(legacy.cache_dir.join("blobs")).unwrap();
    fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
    fs::write(legacy.cache_dir.join("blobs/blob"), "blob").unwrap();
    symlink("blobs/blob", legacy.cache_dir.join("snapshot")).unwrap();

    migrate_legacy_layout(&legacy, &next).unwrap();

    assert!(!legacy.cache_dir.exists());
    assert!(!next.cache_dir.join("snapshot").exists());
    assert_eq!(
        fs::read_to_string(next.config_dir.join("config.jsonc")).unwrap(),
        "config"
    );
    assert!(next.marker().exists());
}

#[cfg(unix)]
#[test]
fn unified_layout_discards_symlinked_cache_without_touching_its_target() {
    let temp = tempfile::tempdir().unwrap();
    let (legacy, next) = test_layouts(temp.path());
    fs::create_dir_all(&legacy.config_dir).unwrap();
    fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
    let target = temp.path().join("real-cache");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("cache.bin"), "keep me").unwrap();
    symlink("../real-cache", &legacy.cache_dir).unwrap();

    migrate_legacy_layout(&legacy, &next).unwrap();

    assert!(fs::symlink_metadata(&legacy.cache_dir).is_err());
    assert_eq!(
        fs::read_to_string(target.join("cache.bin")).unwrap(),
        "keep me"
    );
    assert!(next.marker().exists());
}

#[cfg(unix)]
#[test]
fn unified_layout_discards_cache_whose_absolute_symlink_target_moves() {
    let temp = tempfile::tempdir().unwrap();
    let (legacy, next) = test_layouts(temp.path());
    fs::create_dir_all(&legacy.config_dir).unwrap();
    fs::create_dir_all(&legacy.data_dir).unwrap();
    fs::create_dir_all(&legacy.cache_dir).unwrap();
    fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
    fs::write(legacy.data_dir.join("data.bin"), "data").unwrap();
    symlink(
        legacy.data_dir.join("data.bin"),
        legacy.cache_dir.join("link"),
    )
    .unwrap();

    migrate_legacy_layout(&legacy, &next).unwrap();

    assert!(!legacy.cache_dir.exists());
    assert_eq!(
        fs::read_to_string(next.data_dir.join("data.bin")).unwrap(),
        "data"
    );
    assert!(next.marker().exists());
}

#[cfg(unix)]
#[test]
fn unified_layout_still_refuses_relative_symlink_outside_cache() {
    let temp = tempfile::tempdir().unwrap();
    let (legacy, next) = test_layouts(temp.path());
    fs::create_dir_all(&legacy.config_dir).unwrap();
    fs::create_dir_all(&legacy.cache_dir).unwrap();
    fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
    fs::write(legacy.cache_dir.join("cache.bin"), "cache").unwrap();
    symlink("config.jsonc", legacy.config_dir.join("alias")).unwrap();

    let error = migrate_legacy_layout(&legacy, &next).unwrap_err();

    assert!(error.to_string().contains("relative symbolic link"));
    // A healthy cache must survive an aborted migration untouched.
    assert_eq!(
        fs::read_to_string(legacy.cache_dir.join("cache.bin")).unwrap(),
        "cache"
    );
    assert!(!next.root_dir.exists());
}

#[cfg(unix)]
#[test]
fn unified_layout_cross_source_conflict_has_zero_migration_writes() {
    let temp = tempfile::tempdir().unwrap();
    let (legacy, next) = test_layouts(temp.path());
    fs::create_dir_all(&legacy.config_dir).unwrap();
    fs::create_dir_all(&legacy.pictures_dirs[0]).unwrap();
    fs::create_dir_all(&legacy.pictures_dirs[1]).unwrap();
    fs::write(legacy.config_dir.join("config.jsonc"), "must stay").unwrap();
    fs::write(legacy.pictures_dirs[0].join("conflict.png"), "lower").unwrap();
    fs::write(legacy.pictures_dirs[1].join("conflict.png"), "upper").unwrap();

    let error = migrate_legacy_layout(&legacy, &next).unwrap_err();

    assert!(error.to_string().contains("conflicting legacy entries"));
    assert_eq!(
        fs::read_to_string(legacy.config_dir.join("config.jsonc")).unwrap(),
        "must stay"
    );
    assert_eq!(
        fs::read_to_string(legacy.pictures_dirs[0].join("conflict.png")).unwrap(),
        "lower"
    );
    assert_eq!(
        fs::read_to_string(legacy.pictures_dirs[1].join("conflict.png")).unwrap(),
        "upper"
    );
    assert!(!next.root_dir.exists());
}

#[test]
fn unified_layout_late_destination_conflict_does_not_move_earlier_sources() {
    let temp = tempfile::tempdir().unwrap();
    let (legacy, next) = test_layouts(temp.path());
    fs::create_dir_all(&legacy.config_dir).unwrap();
    fs::create_dir_all(&legacy.documents_dir).unwrap();
    fs::create_dir_all(next.data_dir.join("documents")).unwrap();
    fs::write(legacy.config_dir.join("config.jsonc"), "must stay").unwrap();
    fs::write(legacy.documents_dir.join("conflict.md"), "legacy").unwrap();
    fs::write(next.data_dir.join("documents/conflict.md"), "current").unwrap();

    let error = migrate_legacy_layout(&legacy, &next).unwrap_err();

    assert!(error.to_string().contains("conflicting entries"));
    assert!(legacy.config_dir.join("config.jsonc").exists());
    assert!(!next.config_dir.exists());
    assert_eq!(
        fs::read_to_string(legacy.documents_dir.join("conflict.md")).unwrap(),
        "legacy"
    );
    assert_eq!(
        fs::read_to_string(next.data_dir.join("documents/conflict.md")).unwrap(),
        "current"
    );
    assert!(!next.marker().exists());
}

#[test]
fn interrupted_shell_hook_refresh_is_retried_before_marking_layout_complete() {
    let temp = tempfile::tempdir().unwrap();
    let (legacy, next) = test_layouts(temp.path());
    fs::create_dir_all(legacy.config_dir.join("shell")).unwrap();
    fs::write(legacy.config_dir.join("shell/bash-hook.sh"), "bash hook").unwrap();
    fs::write(
        temp.path().join(".bashrc"),
        "# >>> miyu bash hook >>>\nsource '/legacy/bash-hook.sh'\n",
    )
    .unwrap();

    let error = migrate_legacy_layout(&legacy, &next).unwrap_err();
    assert!(error.to_string().contains("refreshing shell hook paths"));
    assert!(!next.marker().exists());
    assert!(next.config_dir.join("shell/bash-hook.sh").exists());

    fs::write(
        temp.path().join(".bashrc"),
        "# >>> miyu bash hook >>>\nsource '/legacy/bash-hook.sh'\n# <<< miyu bash hook <<<\n",
    )
    .unwrap();
    migrate_legacy_layout(&legacy, &next).unwrap();

    let bashrc = fs::read_to_string(temp.path().join(".bashrc")).unwrap();
    assert!(bashrc.contains(
        next.config_dir
            .join("shell/bash-hook.sh")
            .to_string_lossy()
            .as_ref()
    ));
    assert!(!bashrc.contains("/legacy/bash-hook.sh"));
    assert!(next.marker().exists());
}

#[cfg(unix)]
#[test]
fn migration_rejects_relative_symbolic_links() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("legacy");
    let destination = temp.path().join("next");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(temp.path().join("outside")).unwrap();
    fs::write(temp.path().join("outside/value"), "value").unwrap();
    symlink("../outside/value", source.join("link")).unwrap();

    let error = migrate_entry(&source, &destination).unwrap_err();

    assert!(error.to_string().contains("relative symbolic link"));
    assert!(source.join("link").exists());
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn migration_rejects_symlinked_destination_root_and_marker() {
    let temp = tempfile::tempdir().unwrap();
    let (legacy, next) = test_layouts(temp.path());
    fs::create_dir_all(&legacy.config_dir).unwrap();
    fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();

    symlink(&outside, &next.root_dir).unwrap();
    let root_error = migrate_legacy_layout(&legacy, &next).unwrap_err();
    assert!(root_error.to_string().contains("symbolic-link directory"));
    fs::remove_file(&next.root_dir).unwrap();

    fs::create_dir_all(&next.root_dir).unwrap();
    let marker_target = temp.path().join("marker-target");
    fs::write(&marker_target, "1\n").unwrap();
    symlink(&marker_target, next.marker()).unwrap();
    let marker_error = migrate_legacy_layout(&legacy, &next).unwrap_err();
    assert!(marker_error.to_string().contains("layout marker"));
}

#[test]
fn legacy_data_and_state_alias_is_migrated_once() {
    let temp = tempfile::tempdir().unwrap();
    let (mut legacy, next) = test_layouts(temp.path());
    legacy.state_dir = legacy.data_dir.clone();
    fs::create_dir_all(&legacy.data_dir).unwrap();
    fs::write(legacy.data_dir.join("shared.db"), "state and data").unwrap();

    migrate_legacy_layout(&legacy, &next).unwrap();

    assert_eq!(
        fs::read_to_string(next.data_dir.join("shared.db")).unwrap(),
        "state and data"
    );
    assert!(!legacy.data_dir.exists());
}

#[test]
fn legacy_layout_stays_put_while_the_core_lock_is_held() {
    let temp = tempfile::tempdir().unwrap();
    let (legacy, _) = test_layouts(temp.path());
    let runtime_dir = legacy.state_dir.join("miyu");
    fs::create_dir_all(&runtime_dir).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(runtime_dir.join("core.lock"))
        .unwrap();
    assert!(crate::sys::try_lock_exclusive(&lock).unwrap());

    assert!(legacy_daemon_is_running_at(&legacy, None));
    crate::sys::unlock(&lock);
    assert!(!legacy_daemon_is_running_at(&legacy, None));
}

#[test]
fn legacy_layout_stays_put_while_the_starter_lock_is_held() {
    let temp = tempfile::tempdir().unwrap();
    let (legacy, _) = test_layouts(temp.path());
    let runtime_dir = legacy.state_dir.join("miyu");
    fs::create_dir_all(&runtime_dir).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(runtime_dir.join("starter.lock"))
        .unwrap();
    assert!(crate::sys::try_lock_exclusive(&lock).unwrap());

    assert!(legacy_daemon_is_running_at(&legacy, None));
    crate::sys::unlock(&lock);
    assert!(!legacy_daemon_is_running_at(&legacy, None));
}
