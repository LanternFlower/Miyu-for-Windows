//! 任务台账与陈旧清理。
//!
//! 台账落盘，进程重启后还能看到上一轮的任务。`sweep_stale_jobs` 处理「进程没
//! 了但台账还写着运行中」——判活靠 `process_alive`，PID 会被复用，所以还要比
//! 对启动时间。

use crate::tools::jobs::*;

pub(crate) const LOG_RETENTION_DAYS: u64 = 7;

#[derive(Serialize, Deserialize)]
pub(crate) struct LedgerEntry {
    pub(crate) owner_pid: u32,
    pub(crate) pid: u32,
    pub(crate) job_id: String,
    pub(crate) started_unix: u64,
}

pub(crate) fn logs_dir(paths: &MiyuPaths) -> PathBuf {
    paths.cache_dir.join("jobs")
}

pub(crate) fn ledger_path(paths: &MiyuPaths) -> PathBuf {
    paths.runtime_dir().join("background-jobs.json")
}

pub(crate) fn next_job_id() -> String {
    // Short hex id for display friendliness; collision-checked against the
    // live registry, so six chars are plenty for a per-process job list.
    loop {
        let id = format!("{:06x}", rand::random::<u32>() & 0xff_ffff);
        if !jobs().lock().unwrap().contains_key(&id) {
            return id;
        }
    }
}

pub(crate) fn signal_process_group(pid: u32, force: bool) {
    crate::sys::terminate_process_tree(pid, force);
}

pub(crate) fn process_alive(pid: u32) -> bool {
    crate::sys::process_alive(pid)
}

/// Kill process groups recorded by predecessors that are no longer alive.
/// Entries owned by other live Miyu processes are left untouched.
pub fn sweep_stale_jobs(paths: &MiyuPaths) {
    let path = ledger_path(paths);
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let Ok(entries) = serde_json::from_slice::<Vec<LedgerEntry>>(&bytes) else {
        let _ = std::fs::remove_file(&path);
        return;
    };
    let mut kept = Vec::new();
    for entry in entries {
        if entry.owner_pid == std::process::id() {
            continue;
        }
        if process_alive(entry.owner_pid) {
            kept.push(entry);
            continue;
        }
        if process_alive(entry.pid) {
            tracing::info!(
                job_id = %entry.job_id,
                pid = entry.pid,
                "{}",
                crate::i18n::text(
                    "killing a background job leaked by a dead Miyu process",
                    "清理已死亡 Miyu 进程遗留的后台任务"
                )
            );
            signal_process_group(entry.pid, true);
        }
    }
    let _ = write_ledger(paths, &kept);
}

pub(crate) fn cleanup_old_logs(paths: &MiyuPaths) {
    let dir = logs_dir(paths);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let cutoff = SystemTime::now() - Duration::from_secs(LOG_RETENTION_DAYS * 24 * 3600);
    for entry in entries.flatten() {
        let keep = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map(|modified| modified >= cutoff)
            .unwrap_or(true);
        if !keep {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub(crate) fn write_ledger(paths: &MiyuPaths, entries: &[LedgerEntry]) -> Result<()> {
    let path = ledger_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec(entries)?)?;
    Ok(())
}

pub(crate) fn sync_ledger(paths: &MiyuPaths) {
    let owner_pid = std::process::id();
    let entries = jobs()
        .lock()
        .unwrap()
        .values()
        .filter(|job| job.state == JobState::Running)
        .filter_map(|job| job.pid().map(|pid| (job, pid)))
        .map(|(job, pid)| LedgerEntry {
            owner_pid,
            pid,
            job_id: job.job_id.clone(),
            started_unix: job
                .started_wall
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
        })
        .collect::<Vec<_>>();
    // Preserve entries owned by other live processes sharing this home.
    let mut merged = entries;
    if let Ok(bytes) = std::fs::read(ledger_path(paths)) {
        if let Ok(existing) = serde_json::from_slice::<Vec<LedgerEntry>>(&bytes) {
            merged.extend(
                existing
                    .into_iter()
                    .filter(|entry| entry.owner_pid != owner_pid),
            );
        }
    }
    if let Err(error) = write_ledger(paths, &merged) {
        tracing::debug!(error = %error, "failed to persist the background job ledger");
    }
}
