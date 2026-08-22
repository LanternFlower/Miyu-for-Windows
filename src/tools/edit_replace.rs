use super::{ToolProgress, ToolRegistry, ToolSpec};
use crate::tools::patch_preview::write_with_patch_preview;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new_with_progress(
        "edit_string",
        "Edit a file by replacing an exact string match. Fails if oldString is not found or found multiple times (unless replaceAll). Must differ from newString.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path. Supports absolute, workspace-relative, and ~/ paths."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to replace. Must match exactly including whitespace and indentation."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text. Must differ from old_string."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences of old_string (default false).",
                    "default": false
                }
            },
            "required": ["path", "old_string", "new_string"],
            "additionalProperties": false
        }),
        |args, progress| async move { edit_string(args, progress) },
    ).writes());
}

fn edit_string(args: Value, progress: ToolProgress) -> Result<String> {
    let path = path_arg(&args, "path")?;
    let old_string = args
        .get("old_string")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("old_string is required"))?;
    let new_string = args
        .get("new_string")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("new_string is required"))?;
    let replace_all = args
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if old_string == new_string {
        bail!("old_string and new_string must differ");
    }
    if old_string.is_empty() {
        bail!("old_string must not be empty; use write_file to create or overwrite a file");
    }

    let original = std::fs::read_to_string(&path)?;
    let count = count_occurrences(&original, old_string);

    if count == 0 {
        bail!(
            "Could not find old_string in the file. It must match exactly, including whitespace and indentation."
        );
    }
    if count > 1 && !replace_all {
        bail!(
            "Found {} exact matches for old_string. Provide more surrounding context or set replace_all to true.",
            count
        );
    }

    let updated = if replace_all {
        original.replace(old_string, new_string)
    } else {
        original.replacen(old_string, new_string, 1)
    };

    write_with_patch_preview(
        &path,
        &original,
        &updated,
        &progress,
        serde_json::Map::from_iter([(
            "replacements".to_string(),
            json!(if replace_all { count } else { 1 }),
        )]),
    )
}

fn count_occurrences(content: &str, search: &str) -> usize {
    if search.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut offset = 0;
    while let Some(pos) = content[offset..].find(search) {
        count += 1;
        offset += pos + search.len();
    }
    count
}

fn path_arg(args: &Value, key: &str) -> Result<PathBuf> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        anyhow::bail!("{} is required", key);
    }
    Ok(expand_path(value))
}

fn expand_path(value: &str) -> PathBuf {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = crate::platform_dirs::PlatformDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
            return home.join(rest);
        }
    }
    let path = std::path::Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        super::workspace::effective_workdir().join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_single_match() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.txt");
        std::fs::write(&path, "foo bar baz\n").unwrap();
        let result = edit_string(
            json!({
                "path": path.display().to_string(),
                "old_string": "bar",
                "new_string": "BAR"
            }),
            ToolProgress::default(),
        )
        .unwrap();
        let data: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(data["replacements"], 1);
        assert!(data.get("diff").is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo BAR baz\n");
    }

    #[test]
    fn replace_all_matches() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.txt");
        std::fs::write(&path, "foo foo foo\n").unwrap();
        let result = edit_string(
            json!({
                "path": path.display().to_string(),
                "old_string": "foo",
                "new_string": "qux",
                "replace_all": true
            }),
            ToolProgress::default(),
        )
        .unwrap();
        let data: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(data["replacements"], 3);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "qux qux qux\n");
    }

    #[test]
    fn not_found_errors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.txt");
        std::fs::write(&path, "hello world\n").unwrap();
        let result = edit_string(
            json!({
                "path": path.display().to_string(),
                "old_string": "nonexistent",
                "new_string": "found"
            }),
            ToolProgress::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn multiple_matches_without_replace_all_errors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.txt");
        std::fs::write(&path, "a a a\n").unwrap();
        let result = edit_string(
            json!({
                "path": path.display().to_string(),
                "old_string": "a",
                "new_string": "b"
            }),
            ToolProgress::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn identical_strings_error() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.txt");
        std::fs::write(&path, "same\n").unwrap();
        let result = edit_string(
            json!({
                "path": path.display().to_string(),
                "old_string": "same",
                "new_string": "same"
            }),
            ToolProgress::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn multiline_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.txt");
        std::fs::write(&path, "line1\nline2\nline3\n").unwrap();
        let _result = edit_string(
            json!({
                "path": path.display().to_string(),
                "old_string": "line2",
                "new_string": "LINE TWO\nEXTRA"
            }),
            ToolProgress::default(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line1\nLINE TWO\nEXTRA\nline3\n"
        );
    }
}
