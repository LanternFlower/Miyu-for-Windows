use crate::i18n::text as t;
use crate::paths::MiyuPaths;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

const BEGIN_MARKER: &str = "# >>> miyu powershell hook >>>";
const END_MARKER: &str = "# <<< miyu powershell hook <<<";

pub fn hook() -> String {
    // PowerShell has no `command_not_found_handle` like bash/fish, but since
    // 7.2 `PreCommandLookupAction` fires before every command lookup and can
    // substitute a script block when the command does not exist. We use it to
    // forward unrecognized natural-language input to `miyu`.
    //
    // A re-entrancy guard is essential: the `Get-Command` existence probe
    // below performs its own lookups, which would otherwise recurse forever.
    r#"# Miyu PowerShell integration.
# Forwards unrecognized natural-language commands to the assistant instead of
# failing with `CommandNotFoundException`.

$script:__miyu_lookup_guard = $false

$ExecutionContext.InvokeCommand.PreCommandLookupAction = {
    param($sender, $eventArgs)

    if ($script:__miyu_lookup_guard) { return }
    if ($eventArgs.CommandOrigin -ne [System.Management.Automation.CommandOrigin]::Runspace) { return }

    $name = $eventArgs.CommandName
    if ([string]::IsNullOrWhiteSpace($name)) { return }

    $script:__miyu_lookup_guard = $true
    try {
        $isCommand = $null -ne (Get-Command $name -ErrorAction SilentlyContinue)
    } finally {
        $script:__miyu_lookup_guard = $false
    }
    if ($isCommand) { return }

    # Not a real command: rebuild the typed text and hand it to miyu. The
    # replacement script block receives the trailing arguments in `$args`.
    $script:__miyu_intercept_name = $name
    $eventArgs.CommandScriptBlock = {
        $text = $script:__miyu_intercept_name
        if ($args.Count -gt 0) {
            $text = "$text $($args -join ' ')"
        }
        $text | miyu --shell-intercept --shell powershell --stdin
    }
}
"#
    .to_string()
}

pub fn install(paths: &MiyuPaths) -> Result<()> {
    let hook_file = paths.powershell_hook_file();
    if let Some(parent) = hook_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&hook_file, hook())?;
    println!(
        "{}: {}",
        t("installed powershell hook", "已安装 PowerShell hook"),
        hook_file.display()
    );

    let profile = profile_path();
    upsert_profile_block(&profile, &hook_file)?;
    println!("{}: {}", t("updated", "已更新"), profile.display());
    super::print_reload_hint("powershell", &hook_file);
    Ok(())
}

pub fn uninstall(paths: &MiyuPaths) -> Result<bool> {
    let hook_file = paths.powershell_hook_file();
    let removed_file = match std::fs::remove_file(&hook_file) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => return Err(err.into()),
    };
    let removed_block = remove_profile_block(&profile_path())?;
    let removed = removed_file || removed_block;
    if removed {
        println!(
            "{}: powershell",
            t("removed Miyu shell hook", "已移除 Miyu shell hook")
        );
    }
    Ok(removed)
}

/// The current-user/current-host profile for PowerShell 7 (`$PROFILE`).
/// `PreCommandLookupAction` only exists on PS 7.2+, so Windows PowerShell 5.1
/// is intentionally not targeted.
fn profile_path() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.document_dir().map(Path::to_path_buf))
        .or_else(|| {
            directories::BaseDirs::new().map(|base| base.home_dir().join("Documents"))
        })
        .unwrap_or_else(|| PathBuf::from("Documents"))
        .join("PowerShell")
        .join("Microsoft.PowerShell_profile.ps1")
}

fn ps_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

fn source_block(hook_file: &Path) -> String {
    format!("{BEGIN_MARKER}\n. {}\n{END_MARKER}\n", ps_quote(hook_file))
}

fn read_optional_text(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(value) => Ok(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn upsert_profile_block(profile: &Path, hook_file: &Path) -> Result<()> {
    let existing = read_optional_text(profile)?;
    let block = source_block(hook_file);
    if let Some(updated) = replace_marked_block(&existing, &block)? {
        if updated != existing {
            write_profile(profile, &updated)?;
        }
        return Ok(());
    }
    if let Some(parent) = profile.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(profile)?;
    use std::io::Write as _;
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(file)?;
    }
    file.write_all(block.as_bytes())?;
    Ok(())
}

fn remove_profile_block(profile: &Path) -> Result<bool> {
    let Ok(existing) = std::fs::read_to_string(profile) else {
        return Ok(false);
    };
    let Some(begin_index) = existing.find(BEGIN_MARKER) else {
        return Ok(false);
    };
    let Some(end_relative) = existing[begin_index..].find(END_MARKER) else {
        return Ok(false);
    };
    let mut end_index = begin_index + end_relative + END_MARKER.len();
    if existing.as_bytes().get(end_index) == Some(&b'\r') {
        end_index += 1;
    }
    if existing.as_bytes().get(end_index) == Some(&b'\n') {
        end_index += 1;
    }
    let mut updated = String::new();
    updated.push_str(&existing[..begin_index]);
    updated.push_str(&existing[end_index..]);
    write_profile(profile, &updated)?;
    Ok(true)
}

fn replace_marked_block(existing: &str, replacement: &str) -> Result<Option<String>> {
    let Some(begin_index) = existing.find(BEGIN_MARKER) else {
        if existing.contains(END_MARKER) {
            bail!("PowerShell profile contains a Miyu end marker without its begin marker");
        }
        return Ok(None);
    };
    let Some(end_relative) = existing[begin_index..].find(END_MARKER) else {
        bail!("PowerShell profile contains an incomplete Miyu hook block");
    };
    let mut end_index = begin_index + end_relative + END_MARKER.len();
    if existing.as_bytes().get(end_index) == Some(&b'\r') {
        end_index += 1;
    }
    if existing.as_bytes().get(end_index) == Some(&b'\n') {
        end_index += 1;
    }
    let mut updated = String::with_capacity(existing.len() + replacement.len());
    updated.push_str(&existing[..begin_index]);
    updated.push_str(replacement);
    updated.push_str(&existing[end_index..]);
    Ok(Some(updated))
}

fn write_profile(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("PowerShell profile has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    std::fs::write(path, contents)
        .with_context(|| format!("updating PowerShell profile {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_subscribes_to_command_lookup_and_forwards_to_miyu() {
        let hook = hook();
        assert!(hook.contains("PreCommandLookupAction"));
        assert!(hook.contains("CommandOrigin"));
        assert!(hook.contains("--shell powershell"));
        assert!(hook.contains("--shell-intercept"));
        assert!(hook.contains("__miyu_lookup_guard"));
        assert!(hook.contains("Get-Command"));
    }

    #[test]
    fn source_block_quotes_the_hook_path() {
        let block = source_block(Path::new("C:/Users/x/.miyu/config/shell/miyu.ps1"));
        assert!(block.contains(". 'C:/Users/x/.miyu/config/shell/miyu.ps1'"));
        assert!(block.starts_with(BEGIN_MARKER));
        assert!(block.trim_end().ends_with(END_MARKER));
    }

    #[test]
    fn replace_marked_block_updates_an_existing_block() {
        let existing = format!("before\n{BEGIN_MARKER}\n. 'old.ps1'\n{END_MARKER}\nafter\n");
        let updated = replace_marked_block(&existing, &source_block(Path::new("new.ps1")))
            .unwrap()
            .unwrap();
        assert!(updated.contains(". 'new.ps1'"));
        assert!(!updated.contains("old.ps1"));
        assert_eq!(updated.matches(BEGIN_MARKER).count(), 1);
        assert!(updated.starts_with("before\n"));
        assert!(updated.ends_with("after\n"));
    }

    #[test]
    fn remove_profile_block_removes_only_the_marked_block() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("Microsoft.PowerShell_profile.ps1");
        std::fs::write(
            &profile,
            format!("before\n{BEGIN_MARKER}\n. 'hook.ps1'\n{END_MARKER}\nafter\n"),
        )
        .unwrap();
        assert!(remove_profile_block(&profile).unwrap());
        assert_eq!(
            std::fs::read_to_string(&profile).unwrap(),
            "before\nafter\n"
        );
        assert!(!remove_profile_block(&profile).unwrap());
    }
}
