//! WebUI 文件分享工具（`share_file`）。
//!
//! 与 artifact（演示区）是两个独立概念：分享面向「把文件递给局域网里的人」，
//! 条目进全局分享列表，凭 WebUI 登录态即可下载。默认引用模式零复制；
//! `snapshot=true` 复制进托管区换取长期有效。仅注册在 WebUI 会话。

use super::{ToolRegistry, ToolSpec};
use crate::config::AppConfig;
use crate::state::StateStore;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::sync::OnceLock;

/// daemon 启动时写入 WebUI 的可达地址（`http://ip:port` 列表），
/// 工具用它把相对下载路径拼成局域网完整链接。
static SHARE_URL_BASES: OnceLock<Vec<String>> = OnceLock::new();

pub fn set_share_url_bases(urls: Vec<String>) {
    let _ = SHARE_URL_BASES.set(urls);
}

fn full_urls(path_and_query: &str) -> Vec<String> {
    SHARE_URL_BASES
        .get()
        .map(|bases| {
            bases
                .iter()
                .map(|base| format!("{}{}", base.trim_end_matches('/'), path_and_query))
                .collect()
        })
        .unwrap_or_default()
}

const TOOL_NAME: &str = "share_file";
const DESCRIPTION: &str = "Share a local file through the WebUI so anyone who can open the WebUI (including LAN visitors) can download it. Default mode serves the original file directly with zero copying; the link stops working if the file is moved, modified, or deleted. Pass snapshot=true to copy the file into managed storage for a persistent share. Videos, audio, and images can be previewed inline in the WebUI shared-files panel. Also supports listing existing shares and removing one.";

pub fn register_webui(registry: &mut ToolRegistry, config: &AppConfig, store: StateStore) {
    if !config.plugins.file_sharing.enabled {
        return;
    }
    let max_bytes = config.plugins.file_sharing.max_shared_file_bytes;
    registry.register(
        ToolSpec::new(
            TOOL_NAME,
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path (or ~/ relative to home) of the local file to share. Required for the share action." },
                    "title": { "type": "string", "description": "Optional display title shown in the WebUI shared-files list." },
                    "snapshot": { "type": "boolean", "description": "Copy the file into managed storage so the link keeps working even if the original file changes. Default false: serve the original file directly." },
                    "action": { "type": "string", "enum": ["share", "list", "unshare"], "description": "Defaults to share. list returns all current shares; unshare removes one by share_id." },
                    "share_id": { "type": "string", "description": "The share to remove. Required for the unshare action." }
                },
                "additionalProperties": false
            }),
            move |args| {
                let store = store.clone();
                async move { run(args, store, max_bytes).await }
            },
        )
        .writes(),
    );
}

async fn run(args: Value, store: StateStore, max_bytes: u64) -> Result<String> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("share");
    match action {
        "share" => share(&args, &store, max_bytes),
        "list" => list(&store),
        "unshare" => unshare(&args, &store),
        other => bail!("unknown action: {other}. Use share, list, or unshare."),
    }
}

fn share(args: &Value, store: &StateStore, max_bytes: u64) -> Result<String> {
    let raw_path = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .context("path is required: the absolute path of the local file to share")?;
    let path = if let Some(rest) = raw_path.strip_prefix("~/") {
        let home = std::env::var_os("HOME").context("cannot resolve ~: HOME is not set")?;
        std::path::Path::new(&home).join(rest)
    } else {
        std::path::PathBuf::from(raw_path)
    };
    let title = args.get("title").and_then(Value::as_str).unwrap_or("");
    let snapshot = args
        .get("snapshot")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let record = store.share_file(&path, title, snapshot, max_bytes)?;
    let download_path = format!("/api/shared/{}?download=1", record.share_id);
    Ok(json!({
        "status": "ok",
        "share_id": record.share_id,
        "file_name": record.file_name,
        "mode": record.mode,
        "kind": record.kind,
        "size_bytes": record.size_bytes,
        "download_urls": full_urls(&download_path),
        "download_path": download_path,
        "assistant_instruction": "Give the user a download_urls entry that matches their network (LAN visitors need a LAN address, not 127.0.0.1). Downloading requires WebUI access: if the WebUI has a password, visitors must log in first. For reference-mode shares, warn that moving or editing the original file breaks the link."
    })
    .to_string())
}

fn list(store: &StateStore) -> Result<String> {
    let shares = store
        .list_shared_files()?
        .into_iter()
        .map(|record| {
            json!({
                "share_id": record.share_id,
                "file_name": record.file_name,
                "title": record.title,
                "mode": record.mode,
                "kind": record.kind,
                "size_bytes": record.size_bytes,
                "created_at": record.created_at,
                "download_path": format!("/api/shared/{}?download=1", record.share_id),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "status": "ok", "shares": shares }).to_string())
}

fn unshare(args: &Value, store: &StateStore) -> Result<String> {
    let share_id = args
        .get("share_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .context("share_id is required for unshare; call with action list to look it up")?;
    if !store.delete_shared_file(share_id)? {
        bail!("no share found with id {share_id}");
    }
    Ok(json!({ "status": "ok", "removed": share_id }).to_string())
}
