//! claude_code 执行器:spawn 本机 `claude` CLI、限时、限量、审计。
//!
//! prompt 走 stdin 管道而不是位置参数——绕开 ARG_MAX 与引号转义两类坑。
//! 超时必须显式杀整个进程组(`CommandProcessGroup`):drop future 只是弃
//! promise,`kill_on_drop` 也只杀直接子进程,claude 拉起的孙子进程会活下来。

use anyhow::{bail, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use super::super::default_tools::CommandProcessGroup;
use crate::config::ClaudeCodePluginConfig;
use crate::paths::MiyuPaths;

pub(super) struct ClaudeCodeRequest {
    pub prompt: String,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub append_system_prompt: Option<String>,
    pub resume: Option<String>,
}

/// stderr 只为错误报文服务,不进模型上下文正文,64 KiB 足够。
const STDERR_CAP: usize = 64 * 1024;
/// JSON 信封(usage/modelUsage 等元数据)在 result 之外的开销预算。
/// 原始 stdout 上限 = max_output_bytes + 这份余量;超过它连信封都收不完,
/// 只能按超限报错,收得完则事后只截 result 字段。
const ENVELOPE_SLACK: usize = 64 * 1024;
/// 错误报文里带的 stdout/stderr 尾巴长度(字符)。
const ERROR_TAIL_CHARS: usize = 2000;

/// 同会话并发上限 1:进程内一张运行名单,第二个调用直接拒绝。
/// 防多个订阅会话并发抢额度,不做排队——排队会让回合无限等。
static RUNNING_SESSIONS: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

struct RunningSlot(String);

impl Drop for RunningSlot {
    fn drop(&mut self) {
        let mut running = RUNNING_SESSIONS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        running.remove(&self.0);
    }
}

fn claim_session_slot() -> Result<RunningSlot> {
    let key = crate::tools::workspace::try_session()
        .map(|session| session.to_string())
        .unwrap_or_else(|| "local".to_string());
    let mut running = RUNNING_SESSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !running.insert(key.clone()) {
        bail!("another claude_code run is already in progress for this session; wait for it to finish before starting a new one");
    }
    Ok(RunningSlot(key))
}

pub(super) async fn run(
    request: ClaudeCodeRequest,
    plugin: &ClaudeCodePluginConfig,
    paths: &MiyuPaths,
) -> Result<String> {
    let _slot = claim_session_slot()?;
    let binary = plugin.binary.trim();
    let binary = if binary.is_empty() { "claude" } else { binary };
    let permission_mode = plugin.permission_mode.trim();
    let permission_mode = if permission_mode.is_empty() {
        "bypassPermissions"
    } else {
        permission_mode
    };
    let cwd = request
        .cwd
        .clone()
        .unwrap_or_else(crate::tools::workspace::effective_workdir);
    // 提前查目录:current_dir 不存在时 spawn 也报 NotFound,不查就会
    // 误报成「CLI 未安装」。
    if !cwd.is_dir() {
        bail!("working directory does not exist: {}", cwd.display());
    }

    let mut command = Command::new(binary);
    command
        .arg("-p")
        .arg("--output-format")
        .arg("json")
        .arg("--permission-mode")
        .arg(permission_mode);
    if let Some(model) = &request.model {
        command.arg("--model").arg(model);
    }
    if let Some(append) = &request.append_system_prompt {
        command.arg("--append-system-prompt").arg(append);
    }
    if let Some(resume) = &request.resume {
        command.arg("--resume").arg(resume);
    }
    command.current_dir(&cwd);
    if plugin.prefer_subscription {
        // 强制订阅登录态:环境里挂着 API key 时 claude 会优先按量计费。
        command.env_remove("ANTHROPIC_API_KEY");
        command.env_remove("ANTHROPIC_AUTH_TOKEN");
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("Claude Code CLI not found: `{binary}` is not installed or not on PATH; install Claude Code or set plugins.claude_code.binary");
        }
        Err(error) => return Err(error.into()),
    };
    let mut process_group = CommandProcessGroup::new(child.id());
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to open Claude Code stdin"))?;
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture Claude Code stdout"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture Claude Code stderr"))?;

    let max_output = usize::try_from(plugin.max_output_bytes)
        .ok()
        .filter(|limit| *limit > 0)
        .unwrap_or(512 * 1024);
    let prompt_bytes = request.prompt.as_bytes().len();
    let started = std::time::Instant::now();
    let timeout_seconds = plugin.timeout_seconds.max(1);
    let execution = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_seconds),
        async {
            // 写 prompt 与读输出必须并发:先写完再读的话,大输出会把管道
            // 填满,claude 卡在写、这边卡在写,互相等死。写失败(EPIPE,
            // 子进程早退)不当错——退出码和 stderr 会说明真相。
            let write_prompt = async {
                let _ = stdin.write_all(request.prompt.as_bytes()).await;
                drop(stdin);
            };
            let (_, status, stdout, stderr) = tokio::join!(
                write_prompt,
                child.wait(),
                read_capped(stdout_pipe, max_output.saturating_add(ENVELOPE_SLACK)),
                read_capped(stderr_pipe, STDERR_CAP),
            );
            (status, stdout, stderr)
        },
    )
    .await;

    let (status, stdout, stderr) = match execution {
        Ok(results) => {
            process_group.disarm();
            results
        }
        Err(_) => {
            process_group.terminate();
            let _ = child.start_kill();
            let _ = child.wait().await;
            process_group.disarm();
            bail!("claude_code timed out after {timeout_seconds}s; the Claude Code process group was killed. Raise plugins.claude_code.timeout_seconds for longer tasks.");
        }
    };
    let status = status?;
    let (stdout, stdout_overflow) = stdout?;
    let (stderr, _) = stderr?;
    let stdout_text = String::from_utf8_lossy(&stdout).into_owned();
    let stderr_text = String::from_utf8_lossy(&stderr).into_owned();

    let envelope = match serde_json::from_str::<Value>(stdout_text.trim()) {
        Ok(Value::Object(envelope)) => Some(Value::Object(envelope)),
        Ok(_) | Err(_) => None,
    };
    let Some(envelope) = envelope else {
        if !status.success() {
            bail!(
                "Claude Code exited with {}. stderr tail: {} | stdout tail: {}",
                describe_status(status),
                tail(&stderr_text),
                tail(&stdout_text),
            );
        }
        if stdout_overflow {
            bail!(
                "Claude Code stdout exceeded plugins.claude_code.max_output_bytes ({max_output} bytes) before the JSON envelope completed; raise the limit. stdout tail: {}",
                tail(&stdout_text),
            );
        }
        bail!(
            "failed to parse Claude Code JSON output. stdout tail: {} | stderr tail: {}",
            tail(&stdout_text),
            tail(&stderr_text),
        );
    };

    let result_text = envelope
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let session_id = envelope.get("session_id").and_then(Value::as_str);
    let cost_usd = envelope.get("total_cost_usd").and_then(Value::as_f64);
    let duration_ms = envelope
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| started.elapsed().as_millis() as u64);
    let num_turns = envelope.get("num_turns").and_then(Value::as_u64);
    let is_error = envelope
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || !status.success();
    let (result_text, truncated) = truncate_utf8(result_text, max_output);
    // 请求没点名模型时从 modelUsage 的键里拿实际用的那个。
    let model = request.model.clone().or_else(|| {
        envelope
            .get("modelUsage")
            .and_then(Value::as_object)
            .and_then(|usage| usage.keys().next().cloned())
    });
    append_audit(
        paths,
        serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "model": model,
            "claude_session_id": session_id,
            "cost_usd": cost_usd,
            "duration_ms": duration_ms,
            "prompt_bytes": prompt_bytes,
            "truncated": truncated,
            "is_error": is_error,
        }),
    );

    if is_error {
        let subtype = envelope
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("error");
        bail!(
            "Claude Code reported an error (subtype: {subtype}, {}): {} | stderr tail: {}",
            describe_status(status),
            tail(&result_text),
            tail(&stderr_text),
        );
    }

    let body = serde_json::json!({
        "ok": true,
        "result": result_text,
        "session_id": session_id,
        "cost_usd": cost_usd,
        "duration_ms": duration_ms,
        "num_turns": num_turns,
        "truncated": truncated,
    });
    Ok(serde_json::to_string_pretty(&body)?)
}

/// 边读边截:超限后继续排空管道(不排空子进程会卡在写),但不再缓存。
async fn read_capped(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    cap: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut overflow = false;
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = cap.saturating_sub(output.len());
        if remaining == 0 {
            overflow = true;
            continue;
        }
        let take = read.min(remaining);
        if take < read {
            overflow = true;
        }
        output.extend_from_slice(&buffer[..take]);
    }
    Ok((output, overflow))
}

/// 按字节上限截到最近的字符边界。
fn truncate_utf8(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

fn tail(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return "(empty)".to_string();
    }
    let chars = text.chars().count();
    if chars <= ERROR_TAIL_CHARS {
        return text.to_string();
    }
    let skipped = chars - ERROR_TAIL_CHARS;
    let tail: String = text.chars().skip(skipped).collect();
    format!("…{tail}")
}

fn describe_status(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "killed by signal".to_string(),
    }
}

/// 追加写 JSONL 审计(§3.4 的施工取向:不进 DB,不碰 Usage 口径)。
/// 尽力而为:审计写失败只记日志,不吞掉一次成功的委托结果。
fn append_audit(paths: &MiyuPaths, record: Value) {
    let logs_dir = paths.logs_dir();
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&logs_dir)?;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs_dir.join("claude-code-usage.jsonl"))?;
        writeln!(file, "{record}")
    };
    if let Err(error) = write() {
        tracing::warn!(error = %error, "failed to append the claude_code usage audit record");
    }
}
