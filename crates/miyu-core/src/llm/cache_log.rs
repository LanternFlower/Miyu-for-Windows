//! Per-request cache accounting JSONL (v7 Release 1.5 "JSONL 结构化日志先行").
//!
//! One line of absolute token numbers per LLM request — provider, model,
//! scope, prompt/cache_read/completion — and never any prompt text. The
//! in-process `tracing` line in `finalize_stream_result` is debug-level and
//! ephemeral; this file is what makes cache regressions diagnosable after the
//! fact (the 2026-08-10 "12% full-miss turns" hunt had to be reconstructed
//! from the turns table because nothing durable recorded per-request hits).
//!
//! Files rotate daily (`cache-usage.<YYYY-MM-DD>.jsonl`), are created 0600,
//! and files older than the configured retention are pruned on rotation.
//! Recording must never fail a request: all errors degrade to a debug log.

use crate::llm::Usage;
use miyu_base::config::CacheConfig;
use miyu_base::paths::MiyuPaths;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const FILE_PREFIX: &str = "cache-usage.";
const FILE_SUFFIX: &str = ".jsonl";

struct Sink {
    dir: PathBuf,
    enabled: bool,
    retention_days: u64,
    current: Option<(String, fs::File)>,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// Installs (or updates) the process-wide sink. Called from client
/// construction so every path that can issue LLM requests configures the
/// sink before its first request; later calls just refresh the settings.
pub(crate) fn configure(paths: &MiyuPaths, config: &CacheConfig) {
    let dir = paths.cache_dir.join("logs");
    let mutex = SINK.get_or_init(|| {
        Mutex::new(Sink {
            dir: dir.clone(),
            enabled: config.request_log,
            retention_days: config.request_log_retention_days,
            current: None,
        })
    });
    let mut sink = mutex.lock().unwrap();
    if sink.dir != dir {
        sink.current = None;
        sink.dir = dir;
    }
    sink.enabled = config.request_log;
    sink.retention_days = config.request_log_retention_days;
}

/// Appends one accounting line. `usage` may be `None` (provider reported no
/// usage); the request is still counted so per-scope request totals stay
/// honest.
pub(crate) fn record(
    scope: &str,
    provider: &str,
    model: &str,
    key_index: usize,
    request_id: &str,
    usage: Option<&Usage>,
) {
    let Some(mutex) = SINK.get() else {
        return;
    };
    let Ok(mut sink) = mutex.lock() else {
        return;
    };
    if !sink.enabled {
        return;
    }
    let now = chrono::Local::now();
    let date = now.format("%Y-%m-%d").to_string();
    let line = format_line(
        &now.to_rfc3339(),
        scope,
        provider,
        model,
        key_index,
        request_id,
        usage,
    );
    if let Err(error) = sink.write_line(&date, &line) {
        tracing::debug!(error = %error, "cache usage log write failed");
        sink.current = None;
    }
}

impl Sink {
    fn write_line(&mut self, date: &str, line: &str) -> std::io::Result<()> {
        let rotated = match &self.current {
            Some((current_date, _)) => current_date != date,
            None => true,
        };
        if rotated {
            fs::create_dir_all(&self.dir)?;
            let path = self.dir.join(format!("{FILE_PREFIX}{date}{FILE_SUFFIX}"));
            let mut options = fs::OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(path)?;
            self.current = Some((date.to_string(), file));
            prune_old_files(&self.dir, date, self.retention_days);
        }
        let (_, file) = self.current.as_mut().expect("rotation just set current");
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")
    }
}

fn format_line(
    ts: &str,
    scope: &str,
    provider: &str,
    model: &str,
    key_index: usize,
    request_id: &str,
    usage: Option<&Usage>,
) -> String {
    let (prompt, cache_read, cache_write, completion, reasoning, reported) = match usage {
        Some(usage) => (
            usage.prompt_tokens,
            usage.cache_read_tokens,
            usage.cache_write_tokens,
            usage.completion_tokens,
            usage.reasoning_tokens,
            usage.cache_reported,
        ),
        None => (0, 0, 0, 0, 0, false),
    };
    serde_json::json!({
        "ts": ts,
        "scope": scope,
        "provider": provider,
        "model": model,
        "key": key_index + 1,
        "req": request_id,
        "prompt": prompt,
        "cache_read": cache_read,
        "cache_write": cache_write,
        "completion": completion,
        "reasoning": reasoning,
        "reported": reported,
    })
    .to_string()
}

/// Deletes cache-usage files whose date suffix is more than `retention_days`
/// before `today`. Unparseable file names are left alone.
fn prune_old_files(dir: &Path, today: &str, retention_days: u64) {
    let Ok(today) = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d") else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !stale_file_date(name, today, retention_days) {
            continue;
        }
        if let Err(error) = fs::remove_file(entry.path()) {
            tracing::debug!(file = name, error = %error, "cache usage log prune failed");
        }
    }
}

fn stale_file_date(name: &str, today: chrono::NaiveDate, retention_days: u64) -> bool {
    let Some(date) = name
        .strip_prefix(FILE_PREFIX)
        .and_then(|rest| rest.strip_suffix(FILE_SUFFIX))
    else {
        return false;
    };
    let Ok(date) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
        return false;
    };
    today.signed_duration_since(date).num_days() > retention_days as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(text: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn stale_detection_honours_retention_and_ignores_foreign_files() {
        let today = day("2026-08-10");
        assert!(stale_file_date("cache-usage.2026-07-01.jsonl", today, 14));
        assert!(!stale_file_date("cache-usage.2026-08-01.jsonl", today, 14));
        assert!(!stale_file_date("cache-usage.2026-08-10.jsonl", today, 14));
        // 边界:正好 retention 天不删,多一天才删
        assert!(!stale_file_date("cache-usage.2026-07-27.jsonl", today, 14));
        assert!(stale_file_date("cache-usage.2026-07-26.jsonl", today, 14));
        // 非本日志的文件一律不动
        assert!(!stale_file_date("miyu.2026-07-01.log", today, 14));
        assert!(!stale_file_date("cache-usage.not-a-date.jsonl", today, 14));
    }

    #[test]
    fn line_contains_numbers_only_and_flags_missing_usage() {
        let usage = Usage {
            prompt_tokens: 51910,
            cache_read_tokens: 32384,
            completion_tokens: 996,
            reasoning_tokens: 558,
            cache_reported: true,
            ..Usage::default()
        };
        let line = format_line(
            "2026-08-10T17:00:00+08:00",
            "qq-judge",
            "ririxin",
            "deepseek-v4-flash",
            0,
            "llm_1",
            Some(&usage),
        );
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["prompt"], 51910);
        assert_eq!(value["cache_read"], 32384);
        assert_eq!(value["scope"], "qq-judge");
        assert_eq!(value["key"], 1);
        assert_eq!(value["reported"], true);

        let empty = format_line("ts", "chat", "p", "m", 2, "llm_2", None);
        let value: serde_json::Value = serde_json::from_str(&empty).unwrap();
        assert_eq!(value["prompt"], 0);
        assert_eq!(value["reported"], false);
        assert_eq!(value["key"], 3);
    }

    #[test]
    fn sink_rotates_by_date_and_prunes_stale_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cache-usage.2026-07-01.jsonl"), "old\n").unwrap();
        fs::write(dir.join("miyu.2026-07-01.log"), "keep\n").unwrap();
        let mut sink = Sink {
            dir: dir.clone(),
            enabled: true,
            retention_days: 14,
            current: None,
        };
        sink.write_line("2026-08-10", "{\"a\":1}").unwrap();
        sink.write_line("2026-08-10", "{\"a\":2}").unwrap();
        sink.write_line("2026-08-11", "{\"a\":3}").unwrap();
        let first = fs::read_to_string(dir.join("cache-usage.2026-08-10.jsonl")).unwrap();
        assert_eq!(first.lines().count(), 2);
        let second = fs::read_to_string(dir.join("cache-usage.2026-08-11.jsonl")).unwrap();
        assert_eq!(second.lines().count(), 1);
        assert!(!dir.join("cache-usage.2026-07-01.jsonl").exists());
        assert!(dir.join("miyu.2026-07-01.log").exists());
    }
}
