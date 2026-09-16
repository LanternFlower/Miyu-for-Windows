//! 运行时目录与 home 归一化。

use crate::paths::*;

#[test]
fn default_runtime_directory_keeps_the_legacy_name() {
    let runtime_root = Path::new("/run/user/1000");
    assert_eq!(
        runtime_dir_for(runtime_root, None),
        runtime_root.join("miyu")
    );
}

#[test]
fn explicit_homes_get_stable_isolated_runtime_directories() {
    let temp = tempfile::tempdir().unwrap();
    let runtime_root = temp.path().join("runtime");
    let first = temp.path().join("homes/first");
    let second = temp.path().join("homes/second");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();

    let first_runtime = runtime_dir_for(&runtime_root, Some(&first));
    assert_eq!(first_runtime, runtime_dir_for(&runtime_root, Some(&first)));
    assert_ne!(first_runtime, runtime_dir_for(&runtime_root, Some(&second)));
    assert!(first_runtime
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("miyu-"));
}

#[test]
fn runtime_hash_uses_a_normalized_home_path() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(home.join("child")).unwrap();
    let equivalent = home.join("child/..");

    assert_eq!(
        runtime_dir_for(temp.path(), Some(&home)),
        runtime_dir_for(temp.path(), Some(&equivalent))
    );
}
