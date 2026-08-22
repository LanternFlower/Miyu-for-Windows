//! 后台任务的状态订阅与展示。
//!
//! 轮询线程（`spawn_jobs_poll_thread`）把 daemon 那边的任务状态拉过来，REPL 只
//! 读快照。`JOBS_FEED_MARK_LIMIT` 限制「已读」标记的数量——它只用于去重通知，
//! 无限增长毫无意义。

use crate::cli::*;

pub(in crate::cli) const JOB_SPINNER_FRAMES: [char; 10] =
    ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub(in crate::cli) fn format_job_duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

/// Status strip under the footer: a leading blank line, then one line per
/// background command with a blank line between entries. Timers are
/// right-aligned to the terminal width.
pub(in crate::cli) fn background_job_lines(
    jobs: &[crate::tools::jobs::JobOverview],
    spinner_phase: usize,
    cols: usize,
) -> Vec<String> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let kind_label = |job: &crate::tools::jobs::JobOverview| {
        if job.kind == "subagent" {
            crate::i18n::text("agent", "子代理")
        } else {
            crate::i18n::text("cmd", "命令")
        }
    };
    // Pad kinds to one column so mixed command/subagent rows keep their ids
    // and titles vertically aligned.
    let kind_col = jobs
        .iter()
        .map(|job| visible_width(kind_label(job)))
        .max()
        .unwrap_or(0);
    let mut lines = vec![String::new()];
    for job in jobs.iter() {
        let marker = JOB_SPINNER_FRAMES[spinner_phase % JOB_SPINNER_FRAMES.len()];
        let kind_word = kind_label(job);
        let kind_pad = " ".repeat(kind_col.saturating_sub(visible_width(kind_word)));
        let mut left = format!(
            "{marker} {kind_word}{kind_pad} {} · {}",
            job.job_id, job.title
        );
        let timer = format_job_duration(job.runtime_seconds);
        let timer_width = visible_width(&timer);
        // Never exceed the terminal width: a wrapped strip line would shift
        // the whole tail and flicker.
        let max_left = cols.saturating_sub(timer_width).saturating_sub(2);
        while visible_width(&left) > max_left && !left.is_empty() {
            left.pop();
        }
        let left_width = visible_width(&left);
        let pad = cols
            .saturating_sub(left_width)
            .saturating_sub(timer_width)
            .max(1);
        lines.push(format!("\x1b[2m{left}{}{timer}\x1b[0m", " ".repeat(pad)));
    }
    lines
}

/// Strips the bracketed prefix off a background-job wake headline, leaving
/// `子代理完成 82bea3 · 标题`. The older `[后台命令完成] ` spelling still shows
/// up in sessions recorded before the rename.
pub(in crate::cli) fn job_wake_headline(headline: &str) -> String {
    headline
        .strip_prefix("[后台任务完成] ")
        .or_else(|| headline.strip_prefix("[后台命令完成] "))
        .map(str::to_string)
        .unwrap_or_else(|| headline.to_string())
}

/// Fires a desktop notification unless the REPL window has focus.
///
/// `focused` is `None` when there is no live tail — a one-shot `miyu ask` has
/// no window to be away from, so it stays quiet.
pub(in crate::cli) fn notify_if_unfocused(
    config: &AppConfig,
    focused: Option<bool>,
    title: &str,
    body: &str,
) {
    if !config.notifications.enabled || focused != Some(false) {
        return;
    }
    crate::notify::notify(title, &crate::notify::clip_body(body, 120));
}

/// Shared feed state between the remote REPL and its IPC poll thread.
#[derive(Default)]
pub(in crate::cli) struct SharedJobsFeed {
    /// The owning REPL's current session — strip snapshots are filtered to
    /// it (daemon "current session" can drift from the REPL's after /new).
    pub(in crate::cli) repl_session: std::sync::Mutex<Option<String>>,
    pub(in crate::cli) jobs: std::sync::Mutex<Vec<crate::tools::jobs::JobOverview>>,
    /// Rendered wake-turn reports waiting to be printed into the scrollback.
    pub(in crate::cli) reports: std::sync::Mutex<Vec<BackgroundReport>>,
    /// Latest session Σ read straight from the store. Background subagents
    /// bill to the session that launched them, but they finish long after the
    /// turn that spawned them published its totals — without this the footer
    /// sat on a stale Σ until the user happened to send another prompt.
    pub(in crate::cli) cumulative: std::sync::Mutex<Option<TurnTokens>>,
    /// Active daemon-initiated wake runs: (run_id, session_id, label).
    pub(in crate::cli) wake_runs: std::sync::Mutex<Vec<(String, String, String)>>,
    /// Wake runs already attached to (never re-follow), and turn ids that
    /// were rendered live (their DB report must not print again).
    pub(in crate::cli) followed_runs: std::sync::Mutex<std::collections::HashSet<String>>,
    pub(in crate::cli) rendered_turns: std::sync::Mutex<std::collections::HashSet<String>>,
}

/// 两个去重集合的容量兜底。常开 REPL 的后台唤醒一直发生,集合只增不减;
/// 死掉的 id 不会再被查到(run 不再出现在 wake_runs、turn 已过水位线),
/// 超限时清掉无副作用。
pub(in crate::cli) const JOBS_FEED_MARK_LIMIT: usize = 4_096;

#[derive(Clone)]
pub(in crate::cli) struct BackgroundReport {
    pub(in crate::cli) turn_id: String,
    pub(in crate::cli) headline: String,
    pub(in crate::cli) reply: String,
}

/// Session isolation for the strip: keep only `session`'s jobs (sessionless
/// jobs stay visible as a legacy fallback; `None` session shows everything).
pub(in crate::cli) fn retain_session_jobs(
    jobs: &mut Vec<crate::tools::jobs::JobOverview>,
    session: Option<&str>,
) {
    if let Some(session) = session {
        jobs.retain(|job| job.session_id.is_none() || job.session_id.as_deref() == Some(session));
    }
}

/// Source of background-command snapshots for the idle status strip.
pub(in crate::cli) enum JobsFeed {
    /// Remote REPL: snapshots pushed by the IPC poll thread.
    Shared(std::sync::Arc<SharedJobsFeed>),
    /// Direct REPL: read the in-process registry, scoped to this REPL's
    /// session. 直连道以前直接读整张表不过滤——远端道在 poll 线程里过滤了，
    /// 两条路语义不一致，直连 REPL 会看到别的会话的后台命令。
    Local(Option<String>),
}

impl JobsFeed {
    pub(in crate::cli) fn current(&self) -> Vec<crate::tools::jobs::JobOverview> {
        match self {
            JobsFeed::Shared(shared) => shared.jobs.lock().unwrap().clone(),
            JobsFeed::Local(session) => {
                let mut jobs = crate::tools::jobs::overview();
                retain_session_jobs(&mut jobs, session.as_deref());
                jobs
            }
        }
    }

    /// The store's current Σ for the REPL's session, or `None` when this feed
    /// has no store behind it.
    pub(in crate::cli) fn cumulative(&self) -> Option<TurnTokens> {
        match self {
            JobsFeed::Shared(shared) => *shared.cumulative.lock().unwrap(),
            JobsFeed::Local(_) => None,
        }
    }

    pub(in crate::cli) fn take_reports(&self) -> Vec<BackgroundReport> {
        match self {
            JobsFeed::Shared(shared) => {
                let mut reports = shared.reports.lock().unwrap();
                let rendered = shared.rendered_turns.lock().unwrap();
                let taken = reports
                    .drain(..)
                    .filter(|report| !rendered.contains(&report.turn_id))
                    .collect();
                taken
            }
            JobsFeed::Local(_) => Vec::new(),
        }
    }

    /// Next wake run in `session` that has not been followed yet; marks it
    /// followed so the caller attaches exactly once.
    pub(in crate::cli) fn claim_wake_run(&self, session: &str) -> Option<(String, String)> {
        let JobsFeed::Shared(shared) = self else {
            return None;
        };
        let wake_runs = shared.wake_runs.lock().unwrap();
        let mut followed = shared.followed_runs.lock().unwrap();
        for (run_id, run_session, label) in wake_runs.iter() {
            if run_session == session && !followed.contains(run_id) {
                if followed.len() >= JOBS_FEED_MARK_LIMIT {
                    followed.retain(|id| wake_runs.iter().any(|(r, _, _)| r == id));
                }
                followed.insert(run_id.clone());
                return Some((run_id.clone(), label.clone()));
            }
        }
        None
    }
}

/// Poll the daemon for background commands while the remote REPL idles:
/// 1s when commands are live, 3s when quiet — a unix-socket roundtrip
/// costs microseconds either way.
pub(in crate::cli) fn spawn_jobs_poll_thread(paths: MiyuPaths) -> std::sync::Arc<SharedJobsFeed> {
    let shared = std::sync::Arc::new(SharedJobsFeed::default());
    let feed = shared.clone();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        // Track per-session watermarks so wake replies print exactly once,
        // and never replay history from before this REPL started.
        let mut seen: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        // The store open can lose a race against daemon writes (SQLITE_BUSY);
        // retry every cycle instead of deciding at startup forever.
        let mut store: Option<StateStore> = None;
        loop {
            if store.is_none() {
                store = StateStore::new(&paths).ok();
            }
            let (jobs, session_id, wake_runs) = runtime
                .block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        fetch_jobs_overview(&paths),
                    )
                    .await
                    .unwrap_or_else(|_| Ok((Vec::new(), None, Vec::new())))
                })
                .unwrap_or_default();
            let mut jobs = jobs;
            let repl_session = { feed.repl_session.lock().unwrap().clone() };
            retain_session_jobs(&mut jobs, repl_session.as_deref());
            *feed.jobs.lock().unwrap() = jobs;
            *feed.wake_runs.lock().unwrap() = wake_runs;
            if let (Some(store), Some(session)) = (store.as_ref(), repl_session.as_deref()) {
                if let Ok(totals) = store.pinned(session).session_cumulative_token_totals() {
                    *feed.cumulative.lock().unwrap() = Some(totals);
                }
            }
            if let (Some(store), Some(session_id)) = (store.as_ref(), session_id) {
                let watermark = match seen.entry(session_id.clone()) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let latest = store.latest_turn_seq(&session_id).unwrap_or(0);
                        *entry.insert(latest)
                    }
                };
                if let Ok(rows) = store.background_report_replies_after(&session_id, watermark) {
                    for (seq, turn_id, display, reply) in rows {
                        seen.insert(session_id.clone(), seq);
                        if feed.rendered_turns.lock().unwrap().contains(&turn_id) {
                            continue;
                        }
                        feed.reports.lock().unwrap().push(BackgroundReport {
                            turn_id,
                            headline: display,
                            reply,
                        });
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    shared
}

pub(in crate::cli) type JobsOverviewSnapshot = (
    Vec<crate::tools::jobs::JobOverview>,
    Option<String>,
    Vec<(String, String, String)>,
);

pub(in crate::cli) async fn fetch_jobs_overview(paths: &MiyuPaths) -> Result<JobsOverviewSnapshot> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(&mut stream, &IpcRequest::new(IpcCommand::JobsOverview)).await?;
    match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::AdminResult { state, data }) => {
            let wake_runs = data
                .get("wake_runs")
                .and_then(serde_json::Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            Some((
                                row.get("run_id")?.as_str()?.to_string(),
                                row.get("session_id")?.as_str()?.to_string(),
                                row.get("label")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            ))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok((
                data.get("jobs")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .unwrap_or_default()
                    .unwrap_or_default(),
                Some(state.session_id),
                wake_runs,
            ))
        }
        _ => Ok((Vec::new(), None, Vec::new())),
    }
}
