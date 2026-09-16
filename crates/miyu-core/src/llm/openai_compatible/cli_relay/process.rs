//! CLI 中转子进程的生命周期:拉起、喂 stdin、按行读 stdout(带空闲看门狗)、
//! 收 stderr 尾巴、收尾等待/击杀。三条线的事件语法各不相同,但进程这一层
//! 完全一样——尤其是「超时/出错必须显式杀进程组:drop future 只是弃 promise,
//! 不杀子进程」这条,三处各抄一遍就会有一处漏。

use crate::llm::openai_compatible::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout};

pub(in crate::llm::openai_compatible) fn kill_process_group(pid: u32) {
    signal_process_group(pid, libc::SIGKILL);
}

fn signal_process_group(pid: u32, signal: libc::c_int) {
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

/// SIGTERM 之后留给 CLI 收尾的宽限期。agy 正常收尾(落最后一步、关语言服务器)
/// 实测在 5 秒内;超时的走 SIGKILL 兜底。
const TERMINATE_GRACE: Duration = Duration::from_secs(5);

pub(in crate::llm::openai_compatible) struct RelayProcess {
    child: Child,
    pid: u32,
    lines: Lines<BufReader<ChildStdout>>,
    stderr_tail: Arc<Mutex<String>>,
    stderr_task: tokio::task::JoinHandle<()>,
    /// stdin 写入在独立任务里进行:先读 stdout 再等写完。CLI 在读 stdin 之前
    /// 就退出(续传目标丢失、登录失败)时,大于管道缓冲的载荷会让同步 write_all
    /// 永远等不到人读,或者拿到一个没有 stderr 尾巴的 EPIPE——两种都盖住了
    /// 真正的报错措辞(评审 09-03)。
    stdin_task: tokio::task::JoinHandle<()>,
    idle_timeout: Duration,
    /// 看门狗报错里的阶段名(`claude-code.stream` 这种)。
    stage: &'static str,
    label: &'static str,
}

/// 三条中转线各自的配置/登录态目录:claude(`~/.claude`、`~/.claude.json`)、
/// codex(`~/.codex`)、agy(`~/.gemini`,或 `MIYU_AGY_CONFIG_DIR`)。不存在的不给。
fn relay_config_grants() -> Vec<std::path::PathBuf> {
    let mut grants = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        for name in [".claude", ".claude.json", ".codex", ".gemini"] {
            grants.push(home.join(name));
        }
    }
    if let Some(dir) = std::env::var_os("MIYU_AGY_CONFIG_DIR") {
        grants.push(std::path::PathBuf::from(dir));
    }
    grants
}

impl RelayProcess {
    /// 拉起子进程并把整段 stdin 载荷写完、关写端(本轮输入结束)。
    /// `env` 里 `None` 表示从子进程环境里抹掉该变量。
    #[allow(clippy::too_many_arguments)]
    pub(in crate::llm::openai_compatible) async fn spawn(
        binary: &std::path::Path,
        args: &[String],
        workdir: &std::path::Path,
        env: &[(String, Option<String>)],
        stdin_payload: &str,
        idle_timeout: Duration,
        stage: &'static str,
        label: &'static str,
        not_found: impl FnOnce() -> String,
    ) -> Result<Self> {
        let mut command = tokio::process::Command::new(binary);
        command
            .args(args)
            .current_dir(workdir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        for (key, value) in env {
            match value {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        // 沙盒回合(成员):CLI 进程整个关进 Landlock,它自带的 Bash/Edit 子进程一并
        // 继承;CLI 自己的配置目录(登录态、会话文件)放行读写。
        miyu_base::sandbox::confine_relay(&mut command, &relay_config_grants());
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("{}: {}", not_found(), binary.display())
            } else {
                anyhow::Error::from(error).context(format!("failed to spawn {label}"))
            }
        })?;
        let pid = child.id().unwrap_or_default();
        let mut stdin = child
            .stdin
            .take()
            .with_context(|| format!("{label} stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .with_context(|| format!("{label} stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .with_context(|| format!("{label} stderr unavailable"))?;
        // stderr 尾巴单独收,失败时并进报错(限流/登录错误常常只在 stderr)。
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let stderr_task = {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(mut tail) = tail.lock() {
                        tail.push_str(&line);
                        tail.push('\n');
                        let overflow = tail.len().saturating_sub(8192);
                        if overflow > 0 {
                            let cut = tail
                                .char_indices()
                                .map(|(index, _)| index)
                                .find(|index| *index >= overflow)
                                .unwrap_or(0);
                            tail.drain(..cut);
                        }
                    }
                }
            })
        };
        let stdin_task = {
            let payload = stdin_payload.as_bytes().to_vec();
            tokio::spawn(async move {
                if let Err(error) = stdin.write_all(&payload).await {
                    // 子进程先退出(EPIPE)属正常:真正的原因在 stdout/stderr 里。
                    tracing::debug!(%error, "{label} closed stdin before the payload was written");
                }
                drop(stdin);
            })
        };
        Ok(Self {
            child,
            pid,
            lines: BufReader::new(stdout).lines(),
            stderr_tail,
            stderr_task,
            stdin_task,
            idle_timeout,
            stage,
            label,
        })
    }

    /// 下一行 stdout;空闲超过看门狗就收掉进程组并报 Timeout 类传输失败。
    /// `Ok(None)` = stdout 关闭(进程退出)。
    pub(in crate::llm::openai_compatible) async fn next_line(&mut self) -> Result<Option<String>> {
        match tokio::time::timeout(self.idle_timeout, self.lines.next_line()).await {
            Err(_) => {
                self.terminate().await;
                Err(anyhow::anyhow!(
                    "{} produced no output for {}s; the process was killed",
                    self.label,
                    self.idle_timeout.as_secs()
                )
                .context(TransportFailure {
                    stage: self.stage,
                    kind: TransportFailureKind::Timeout,
                }))
            }
            Ok(read) => read.with_context(|| format!("failed to read {} stdout", self.label)),
        }
    }

    pub(in crate::llm::openai_compatible) fn kill(&self) {
        kill_process_group(self.pid);
    }

    /// 先 SIGTERM 再兜底 SIGKILL:硬杀会让 CLI 正在跑的那一步停在取消态留在
    /// 会话转录里,agy 的日志里能看到序列化器后来一直撞它
    /// (`serializer encountered non-tool step N ... CORTEX_STEP_STATUS_CANCELED`)。
    ///
    /// 老实说:09-16 的对照实验里,单次硬杀(生成中途 SIGKILL)**没能**复现出
    /// 一条坏掉的会话——两组 resume 回来都是 SUCCESS,所以「硬杀必然毒化会话」
    /// 并没有被证实,这里是防御性的,不是已证实的修复。留着的理由很朴素:
    /// SIGTERM 实测让 agy 在 1 秒内自己收尾退出(退出码 1),代价几乎为零,
    /// 而硬杀留下的取消态步确实会出现在序列化报错里。
    pub(in crate::llm::openai_compatible) async fn terminate(&mut self) {
        signal_process_group(self.pid, libc::SIGTERM);
        if tokio::time::timeout(TERMINATE_GRACE, self.child.wait())
            .await
            .is_err()
        {
            tracing::warn!(
                label = self.label,
                grace_seconds = TERMINATE_GRACE.as_secs(),
                "relay process ignored SIGTERM; killing the process group"
            );
            self.kill();
        }
    }

    pub(in crate::llm::openai_compatible) fn stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .map(|tail| tail.trim().to_string())
            .unwrap_or_default()
    }

    /// 终态帧到手后进程应当自然退出;不给它耗着的机会。返回退出码文本。
    pub(in crate::llm::openai_compatible) async fn finish(mut self) -> (String, String) {
        let exit = match tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await {
            Ok(status) => status.ok(),
            Err(_) => {
                // 终态帧已到手,但进程还赖着:同样走 SIGTERM→SIGKILL,别在
                // 收尾这一步把转录打成取消态(理由见 `terminate`)。
                self.terminate().await;
                None
            }
        };
        self.stderr_task.abort();
        self.stdin_task.abort();
        let code = exit
            .and_then(|status| status.code())
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        (code, self.stderr_tail())
    }
}
