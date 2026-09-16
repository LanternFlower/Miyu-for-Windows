use anyhow::Result;
use miyu_base::paths::MiyuPaths;
use miyu_core::args::WebArgs;

/// Unified background host for IPC, WebUI and configured platform transports.
/// Transport-specific HTTP handlers remain in `web`; lifecycle ownership lives
/// here so future entrypoints do not acquire a second process model.
pub async fn run(paths: MiyuPaths, web: WebArgs) -> Result<()> {
    crate::web::run(paths, web).await
}
