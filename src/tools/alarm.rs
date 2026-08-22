use super::{ToolRegistry, ToolSpec};
use crate::alarm::{self, AlarmRecord, AlarmStatus};
use crate::paths::MiyuPaths;
use anyhow::{bail, Result};
use chrono::Local;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// 三件闹钟工具合并成一件 `alarm`(08-17):set/list/cancel 是同一个对象的
/// 三种操作,拆开只是让 tools 数组多背两份外壳。
pub fn register(registry: &mut ToolRegistry, paths: MiyuPaths) {
    registry.register(ToolSpec::new(
        "alarm",
        "Manage local alarms. action=set schedules one (time accepts 30s, 10m, 1h 30m, or 14:30); action=list shows scheduled and ringing alarms; action=cancel removes one by id. Alarms run in a background Miyu process with Miyu's embedded sound.",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["set", "list", "cancel"],
                    "description": "set schedules, list shows, cancel removes."
                },
                "time": { "type": "string", "description": "Required for set: duration or clock time." },
                "label": { "type": "string", "description": "Optional alarm label for set." },
                "audio_file": { "type": "string", "description": "Optional local .wav or .mp3 for set, replacing Miyu's built-in sound." },
                "id": { "type": "string", "description": "Required for cancel: alarm id from set or list." }
            },
            "required": ["action"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths.clone();
            async move {
                match args.get("action").and_then(Value::as_str).unwrap_or_default() {
                    "set" => set_alarm(args, paths).await,
                    "list" => list_alarms(paths).await,
                    "cancel" => cancel_alarm(args, paths).await,
                    other => bail!("unknown action: {other}; expected set, list or cancel"),
                }
            }
        },
    ).writes());
}
async fn set_alarm(args: Value, paths: MiyuPaths) -> Result<String> {
    let time = args
        .get("time")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if time.is_empty() {
        bail!("time is required")
    }
    let label = args
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("Miyu alarm")
        .trim();
    let audio_file = args
        .get("audio_file")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(resolve_audio_file)
        .transpose()?;
    let due_at = alarm::due_at_from_time(time)?;
    let id = format!(
        "alarm-{}-{}",
        Local::now().timestamp_millis(),
        std::process::id()
    );
    let exe = crate::paths::miyu_executable()?;
    let mut command = Command::new(exe);
    command
        .arg("__alarm-worker")
        .arg("--id")
        .arg(&id)
        .arg("--time")
        .arg(time)
        .arg("--label")
        .arg(label)
        .arg("--state-dir")
        .arg(&paths.state_dir)
        .arg("--cache-dir")
        .arg(&paths.cache_dir);
    if let Some(path) = &audio_file {
        command.arg("--audio-file").arg(path);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let pid = child.id();
    alarm::upsert(
        &paths,
        AlarmRecord {
            id: id.clone(),
            label: label.to_string(),
            time: time.to_string(),
            audio_file: audio_file.clone(),
            due_at,
            pid,
            status: AlarmStatus::Scheduled,
        },
    )?;
    Ok(json!({
        "ok": true,
        "id": id,
        "time": time,
        "label": label,
        "audio_file": audio_file,
        "due_at": due_at,
        "due_at_local": alarm::format_due_at(due_at),
        "pid": pid,
    })
    .to_string())
}

async fn list_alarms(paths: MiyuPaths) -> Result<String> {
    let records = alarm::cleanup_dead(&paths)?;
    let alarms = records
        .into_iter()
        .map(|record| {
            json!({
                "id": record.id,
                "label": record.label,
                "time": record.time,
                "audio_file": record.audio_file,
                "due_at": record.due_at,
                "due_at_local": alarm::format_due_at(record.due_at),
                "pid": record.pid,
                "status": record.status,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"ok": true, "alarms": alarms}).to_string())
}

async fn cancel_alarm(args: Value, paths: MiyuPaths) -> Result<String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if id.is_empty() {
        bail!("id is required")
    }
    let removed = alarm::remove(&paths, id)?;
    if let Some(record) = &removed {
        if let Some(pid) = record.pid {
            if alarm::process_exists(pid) {
                alarm::stop_process(pid)?;
            }
        }
    }
    Ok(json!({"ok": removed.is_some(), "id": id, "removed": removed.is_some()}).to_string())
}

fn resolve_audio_file(value: &str) -> Result<PathBuf> {
    let path = expand_path(value.trim());
    let canonical = path.canonicalize()?;
    if !canonical.is_file() {
        bail!("audio_file is not a regular file: {}", path.display())
    }
    let extension = canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "wav" | "mp3") {
        bail!("audio_file must be a .wav or .mp3 file")
    }
    Ok(canonical)
}

fn expand_path(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = crate::platform_dirs::PlatformDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
            return home.join(rest);
        }
    }
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        super::workspace::effective_workdir().join(path)
    }
}
