//! 出网请求录制(验收:要能亲眼确认发给模型的完整请求体,例如 dev
//! 请求有没有被注入不必要的东西)。
//!
//! 抓在三个序列化后的出网口(chat/anthropic/responses),记录的就是
//! `.json(request)` 将要发出的同一结构——所有 extra_body、思考档、消息
//! 变换都已生效。默认关闭;`miyu daemon logs request` 在监控期间开启,
//! 环境变量 `MIYU_LOG_REQUESTS=1` 可常开(直连 REPL 也生效)。
//! 完整体积大(长上下文可达数百 KB/请求),全量落 JSONL 文件,不进
//! 常规日志。开关是进程级内存位,daemon 重启自动回到关闭。

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static ENABLED: AtomicBool = AtomicBool::new(false);
static ENV_INIT: OnceLock<()> = OnceLock::new();
static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 客户端构造时安装日志目录(幂等)。
pub fn install_dir(dir: PathBuf) {
    let _ = LOG_DIR.set(dir);
}

fn env_init() {
    ENV_INIT.get_or_init(|| {
        if std::env::var_os("MIYU_LOG_REQUESTS").is_some_and(|value| value != "0") {
            ENABLED.store(true, Ordering::Relaxed);
        }
    });
}

pub fn set_enabled(enabled: bool) {
    env_init();
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    env_init();
    ENABLED.load(Ordering::Relaxed)
}

/// 当天的录制文件路径(目录未安装时为 None)。
pub fn current_file() -> Option<PathBuf> {
    let date = chrono::Utc::now().format("%Y-%m-%d");
    Some(LOG_DIR.get()?.join(format!("requests-{date}.jsonl")))
}

/// 追加一条完整请求记录。录制关闭时零开销;写盘失败只警告,绝不影响
/// 请求本身。
pub fn record<T: serde::Serialize>(
    provider: &str,
    model: &str,
    kind: &str,
    scope: &str,
    url: &str,
    request: &T,
) {
    if !enabled() {
        return;
    }
    let Some(path) = current_file() else {
        return;
    };
    let body = match serde_json::to_value(request) {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, "request log: serialize failed");
            return;
        }
    };
    let line = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "provider": provider,
        "model": model,
        "kind": kind,
        "scope": scope,
        "url": url,
        "body": body,
    });
    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let mut serialized = line.to_string();
        serialized.push('\n');
        file.write_all(serialized.as_bytes())
    };
    if let Err(error) = write() {
        tracing::warn!(%error, path = %path.display(), "request log: append failed");
    }
}
