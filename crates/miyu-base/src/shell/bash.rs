use crate::i18n::text as t;
use crate::paths::MiyuPaths;
use anyhow::Result;
use std::path::Path;

const BEGIN_MARKER: &str = "# >>> miyu bash hook >>>";
const END_MARKER: &str = "# <<< miyu bash hook <<<";

pub fn hook() -> &'static str {
    r#"command_not_found_handle() {
    [[ $- == *i* ]] || return 127

    local text="$*"
    [[ -n "$text" ]] || return 127
    [[ "$text" != *$'\n'* && "$text" != *$'\r'* ]] || return 127

    miyu --shell-intercept --shell bash -- "$@" 2>/dev/null
    return 127
}
"#
}

pub fn install(paths: &MiyuPaths) -> Result<()> {
    if let Some(parent) = paths.bash_hook_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&paths.bash_hook_file, hook())?;
    let rc_path = home_file(".bashrc");
    super::upsert_source_block(&rc_path, BEGIN_MARKER, END_MARKER, &paths.bash_hook_file)?;
    println!(
        "{}: {}",
        t("installed bash hook", "已安装 bash hook"),
        paths.bash_hook_file.display()
    );
    println!("{}: {}", t("updated", "已更新"), rc_path.display());
    super::print_reload_hint("bash", &paths.bash_hook_file);
    Ok(())
}

pub fn uninstall(paths: &MiyuPaths) -> Result<bool> {
    let removed_file = remove_file_if_exists(&paths.bash_hook_file)?;
    let rc_path = home_file(".bashrc");
    let removed_block = remove_source_block(&rc_path, BEGIN_MARKER, END_MARKER)?;
    let removed = removed_file || removed_block;
    if removed {
        println!(
            "{}: bash",
            t("removed Miyu shell hook", "已移除 Miyu shell hook")
        );
    }
    Ok(removed)
}

fn home_file(name: &str) -> std::path::PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(name))
        .unwrap_or_else(|| std::path::PathBuf::from(name))
}

fn remove_source_block(rc_path: &Path, begin: &str, end: &str) -> Result<bool> {
    let Ok(existing) = std::fs::read_to_string(rc_path) else {
        return Ok(false);
    };
    let Some(begin_index) = existing.find(begin) else {
        return Ok(false);
    };
    let Some(end_relative) = existing[begin_index..].find(end) else {
        return Ok(false);
    };
    let mut end_index = begin_index + end_relative + end.len();
    if existing.as_bytes().get(end_index) == Some(&b'\r') {
        end_index += 1;
    }
    if existing.as_bytes().get(end_index) == Some(&b'\n') {
        end_index += 1;
    }
    let mut updated = String::new();
    updated.push_str(&existing[..begin_index]);
    updated.push_str(&existing[end_index..]);
    super::write_rc_atomic(rc_path, &updated)?;
    Ok(true)
}

fn remove_file_if_exists(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_hook_defines_command_not_found_handler() {
        let hook = hook();
        assert!(hook.contains("command_not_found_handle"));
        assert!(hook.contains("--shell bash"));
        assert!(hook.contains("return 127"));
    }

    #[test]
    fn bash_hook_does_not_filter_natural_language_symbols() {
        let hook = hook();
        assert!(!hook.contains("${#text} <= 120"));
        assert!(!hook.contains("miyu_shell_syntax_pattern"));
        assert!(!hook.contains("miyu_leading_pattern"));
    }

    #[test]
    fn remove_file_if_exists_reports_whether_file_was_removed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hook.sh");

        assert!(!remove_file_if_exists(&path).unwrap());
        std::fs::write(&path, hook()).unwrap();
        assert!(remove_file_if_exists(&path).unwrap());
        assert!(!remove_file_if_exists(&path).unwrap());
    }

    #[test]
    fn remove_source_block_reports_whether_block_was_removed() {
        let temp = tempfile::tempdir().unwrap();
        let rc_path = temp.path().join(".bashrc");
        std::fs::write(
            &rc_path,
            format!("before\n{BEGIN_MARKER}\nsource hook\n{END_MARKER}\nafter\n"),
        )
        .unwrap();

        assert!(remove_source_block(&rc_path, BEGIN_MARKER, END_MARKER).unwrap());
        assert_eq!(
            std::fs::read_to_string(&rc_path).unwrap(),
            "before\nafter\n"
        );
        assert!(!remove_source_block(&rc_path, BEGIN_MARKER, END_MARKER).unwrap());
    }

    #[test]
    fn installing_again_refreshes_an_existing_hook_path() {
        let temp = tempfile::tempdir().unwrap();
        let rc_path = temp.path().join(".bashrc");
        std::fs::write(
            &rc_path,
            format!("before\n{BEGIN_MARKER}\nsource '/old/miyu-hook.sh'\n{END_MARKER}\nafter\n"),
        )
        .unwrap();
        let hook = temp.path().join("new miyu-hook.sh");

        crate::shell::upsert_source_block(&rc_path, BEGIN_MARKER, END_MARKER, &hook).unwrap();

        let updated = std::fs::read_to_string(rc_path).unwrap();
        assert!(updated.contains("new miyu-hook.sh"));
        assert!(!updated.contains("/old/miyu-hook.sh"));
        assert_eq!(updated.matches(BEGIN_MARKER).count(), 1);
        assert!(updated.starts_with("before\n"));
        assert!(updated.ends_with("after\n"));
    }
}
