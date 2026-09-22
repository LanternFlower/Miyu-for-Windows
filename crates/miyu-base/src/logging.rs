use crate::i18n::text as t;
use crate::paths::MiyuPaths;
use anyhow::{Context, Result};
use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

const LOG_ENV: &str = "MIYU_LOG";
const LOG_FILE_LIMIT: usize = 8;
const LOG_BUFFERED_LINES_LIMIT: usize = 1_024;

pub struct LoggingGuard {
    _worker: Option<WorkerGuard>,
}

pub fn init(paths: &MiyuPaths, cli_debug: bool) -> Result<LoggingGuard> {
    let env_value = std::env::var(LOG_ENV).ok();
    let (level, invalid_env) = selected_level(cli_debug, env_value.as_deref());
    if level == LevelFilter::OFF {
        return Ok(LoggingGuard { _worker: None });
    }

    let logs_dir = paths.logs_dir();
    std::fs::create_dir_all(&logs_dir)
        .with_context(|| format!("creating log directory {}", logs_dir.display()))?;
    secure_log_directory(&logs_dir)?;

    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("miyu")
        .filename_suffix("log")
        .max_log_files(LOG_FILE_LIMIT)
        .build(&logs_dir)
        .with_context(|| format!("opening log file in {}", logs_dir.display()))?;
    let (writer, worker) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .buffered_lines_limit(LOG_BUFFERED_LINES_LIMIT)
        .lossy(false)
        .finish(appender);
    let targets = Targets::new()
        .with_default(LevelFilter::OFF)
        .with_target("miyu", level)
        .with_target("miyu::qq", qq_level(level, cli_debug, env_value.is_some()));
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(writer)
        .with_filter(targets);
    tracing_subscriber::registry()
        .with(fmt_layer)
        .try_init()
        .context("initializing debug logging")?;

    if invalid_env {
        tracing::error!(
            variable = LOG_ENV,
            "{}",
            t(
                "invalid log level ignored; expected off, error, warn, info, debug, or trace",
                "已忽略无效日志级别；应为 off、error、warn、info、debug 或 trace"
            )
        );
    }
    tracing::debug!(
        level = %level,
        log_dir = %logs_dir.display(),
        "{}",
        t("debug logging initialized", "调试日志已初始化")
    );
    Ok(LoggingGuard {
        _worker: Some(worker),
    })
}

fn qq_level(level: LevelFilter, cli_debug: bool, explicit_env: bool) -> LevelFilter {
    if level == LevelFilter::OFF || explicit_env || cli_debug {
        level
    } else {
        LevelFilter::INFO
    }
}

fn selected_level(cli_debug: bool, env_value: Option<&str>) -> (LevelFilter, bool) {
    let fallback = if cli_debug {
        LevelFilter::DEBUG
    } else {
        LevelFilter::ERROR
    };
    let Some(value) = env_value else {
        return (fallback, false);
    };
    let level = match value.trim().to_ascii_lowercase().as_str() {
        "off" => LevelFilter::OFF,
        "error" => LevelFilter::ERROR,
        "warn" => LevelFilter::WARN,
        "info" => LevelFilter::INFO,
        "debug" => LevelFilter::DEBUG,
        "trace" => LevelFilter::TRACE,
        _ => return (fallback, true),
    };
    (level, false)
}

#[cfg(unix)]
fn secure_log_directory(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("securing log directory {}", path.display()))
}

#[cfg(not(unix))]
fn secure_log_directory(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_level_only_records_errors() {
        assert_eq!(selected_level(false, None), (LevelFilter::ERROR, false));
        assert_eq!(
            qq_level(LevelFilter::ERROR, false, false),
            LevelFilter::INFO
        );
    }

    #[test]
    fn debug_flag_enables_debug_level() {
        assert_eq!(selected_level(true, None), (LevelFilter::DEBUG, false));
    }

    #[test]
    fn environment_overrides_debug_flag() {
        assert_eq!(
            selected_level(true, Some("info")),
            (LevelFilter::INFO, false)
        );
        assert_eq!(
            qq_level(LevelFilter::ERROR, false, true),
            LevelFilter::ERROR
        );
    }

    #[test]
    fn environment_can_disable_logging() {
        assert_eq!(selected_level(true, Some("off")), (LevelFilter::OFF, false));
    }

    #[test]
    fn invalid_environment_uses_flag_fallback() {
        assert_eq!(
            selected_level(true, Some("everything")),
            (LevelFilter::DEBUG, true)
        );
    }
}
