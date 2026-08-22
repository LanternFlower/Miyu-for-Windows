//! 从管道读输入。
//!
//! `miyu < file` 或 `cmd | miyu` 时把 stdin 并进提示词。有字符上限与超时
//! （`STDIN_MAX_CHARS` / `STDIN_TIMEOUT_SECS`）——管道可能永远不关，也可能吐出
//! 几个 G。

use crate::cli::*;

pub(in crate::cli) fn drain_stdin() {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        let stdin = io::stdin();
        if !stdin.is_terminal() {
            return;
        }
        let fd = stdin.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return;
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return;
        }

        let mut handle = stdin.lock();
        let mut buffer = [0_u8; 4096];
        loop {
            match handle.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    }
    #[cfg(not(unix))]
    if io::stdin().is_terminal() {
        crate::sys::flush_stdin();
    }
}

pub(in crate::cli) const STDIN_MAX_CHARS: usize = 50_000;

pub(in crate::cli) const STDIN_TIMEOUT_SECS: u64 = 5;

pub(in crate::cli) async fn append_stdin_if_piped(message: String) -> String {
    if io::stdin().is_terminal() {
        return message;
    }
    // The reader thread bounds itself with poll() deadlines instead of being
    // abandoned by an outer timeout: a thread stuck in a blocking read(0)
    // would make the tokio runtime hang forever on shutdown (the process
    // then never exits when stdin is a never-closing pipe).
    let read_result = tokio::task::spawn_blocking(move || {
        crate::sys::read_stdin_with_timeout(
            STDIN_MAX_CHARS,
            Duration::from_secs(STDIN_TIMEOUT_SECS),
        )
    })
    .await;

    let stdin_content = match read_result {
        Ok(Ok(content)) if !content.is_empty() => {
            String::from_utf8_lossy(&content).trim().to_string()
        }
        _ => return message,
    };

    if message.is_empty() {
        stdin_content
    } else {
        format!("{message}\n\n---\n(stdin)\n{stdin_content}")
    }
}

/// Expands a leading `~` or `~/…` to the user's home directory.
pub(in crate::cli) fn expand_tilde(path: &str) -> PathBuf {
    if let Some(home) = crate::platform_dirs::PlatformDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
        if path == "~" {
            return home;
        }
        if let Some(rest) = path.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}
