//! 后台任务测试共用的 fixture。

use crate::tools::jobs::*;

pub(super) fn test_progress() -> ToolProgress {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    Box::leak(Box::new(receiver));
    ToolProgress::new(sender)
}

/// `init` is process-global (OnceLock), so every test shares one leaked
/// home; individual tests must tolerate jobs from their siblings.
pub(super) fn shared_init() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let temp = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let root = temp.path().to_path_buf();
        let paths = MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            ..crate::paths::MiyuPaths::new().unwrap()
        };
        init(&paths);
    });
}

/// 测试用:轮询等待任务进入终态(生产路径已无阻塞等待,由完成钩子唤醒)。
pub(super) async fn await_terminal(job_id: &str) {
    for _ in 0..200 {
        if job_snapshot(job_id)
            .map(|job| job.state.is_terminal())
            .unwrap_or(true)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("job {job_id} did not finish in time");
}

/// `stop_session_jobs` filters by session id, which these tests do not
/// have; drive the same concurrent path over explicit ids instead.
pub(super) async fn stop_session_jobs_for_test(ids: &[String]) -> usize {
    futures_util::future::join_all(ids.iter().map(|id| stop_job(id)))
        .await
        .into_iter()
        .filter(Result::is_ok)
        .count()
}
