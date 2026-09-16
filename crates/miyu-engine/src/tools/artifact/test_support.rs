//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/artifact.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

pub fn create_artifact(
    args: Value,
    progress: ToolProgress,
    root: &Path,
    session_id: &str,
) -> Result<String> {
    let filename = args
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    validate_file_name(filename)?;
    validate_session_id(session_id)?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("content is required"))?;
    if content.is_empty() || content.len() > MAX_ARTIFACT_BYTES {
        bail!("artifact content must be between 1 byte and 20 MiB");
    }
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();

    ensure_private_dir(root)?;
    let session_dir = root.join(session_id);
    ensure_private_dir(&session_dir)?;
    let path = session_dir.join(filename);
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!(
                "artifact destination is not a regular file: {}",
                path.display()
            );
        }
    }
    let mut temp = tempfile::NamedTempFile::new_in(&session_dir)?;
    temp.as_file_mut()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    temp.write_all(content.as_bytes())?;
    temp.as_file_mut().sync_all()?;
    temp.persist(&path)?;
    progress.report_artifact(path.clone(), title);
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "path": path,
        "filename": filename,
        "title": title,
        "published": true
    }))?)
}

pub fn read_artifact(args: Value, root: &Path, session_id: &str) -> Result<String> {
    let filename = required_filename(&args)?;
    let path = managed_file_path(root, session_id, filename)?;
    let mut scoped = args;
    scoped["path"] = Value::String(path.to_string_lossy().to_string());
    super::super::default_tools::read_file(scoped)
}

pub fn required_filename(args: &Value) -> Result<&str> {
    let filename = args
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    validate_file_name(filename)?;
    Ok(filename)
}

pub fn validate_file_name(filename: &str) -> Result<()> {
    let path = Path::new(filename);
    let mut components = path.components();
    let valid_component =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !valid_component || filename.chars().count() > 180 || filename.chars().any(char::is_control)
    {
        bail!("filename must be a single safe file name");
    }
    Ok(())
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "Artifact workspace path is not a directory: {}",
            path.display()
        );
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}
