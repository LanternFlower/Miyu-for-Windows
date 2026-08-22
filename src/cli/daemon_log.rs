//! daemon 日志的读取、格式化与跟随。
//!
//! `miyu daemon logs` 要处理几件不太直观的事：日志会轮转（跟随时得跨文件接
//! 上）、可能被截断（要认出来重新定位）、行可能跨多行（结构化日志的多行正文
//! 不能被拆散）、还有 daemon 自己 stdout 的非结构化输出要按时间穿插进去。
//!
//! 格式化那半边负责上色与对齐——`miyu daemon logs` 是排查问题时最常用的入口，
//! 所以「活跃回复决策」这类关键行整行上色，一眼能扫到。

use crate::cli::*;

pub(in crate::cli) async fn run_daemon_logs(paths: &MiyuPaths, args: DaemonLogsArgs) -> Result<()> {
    match args.topic.as_deref().map(str::trim) {
        None => {}
        Some("request" | "requests") => return run_request_monitor(paths).await,
        Some(other) => bail!(
            "{}: {other}",
            t(
                "unknown logs topic (try: request)",
                "未知日志主题(可用: request)"
            )
        ),
    }
    let ansi = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    if let Some(lines) = args.lines {
        if !(1..=100_000).contains(&lines) {
            bail!(
                "{}",
                t(
                    "--lines must be between 1 and 100000",
                    "--lines 必须在 1 到 100000 之间"
                )
            );
        }
        let snapshot = recent_daemon_log_snapshot(paths, lines)?;
        write_daemon_log_lines(&snapshot.lines, ansi)?;
        return Ok(());
    }

    let snapshot = recent_daemon_log_snapshot(paths, 50)?;
    write_daemon_log_lines(&snapshot.lines, ansi)?;
    // The cursor is tied to the exact EOF captured by the snapshot reader.
    // Bytes appended while the snapshot is printed or while the status probe
    // runs are therefore consumed by follow instead of being skipped.
    let cursor = snapshot.cursor;
    let Some(daemon) = ipc::daemon_info(paths).await else {
        bail!("{}", t("Miyu daemon is not running", "Miyu daemon 未运行"));
    };
    follow_daemon_log(paths, ansi, cursor, daemon.pid).await
}

#[derive(Debug, Eq, PartialEq)]
pub(in crate::cli) struct ParsedDaemonLogLine<'a> {
    pub(in crate::cli) timestamp: &'a str,
    pub(in crate::cli) level: &'a str,
    pub(in crate::cli) module: &'a str,
    pub(in crate::cli) message: &'a str,
}

pub(in crate::cli) fn parse_daemon_log_line(line: &str) -> Option<ParsedDaemonLogLine<'_>> {
    let timestamp_end = line.find(char::is_whitespace)?;
    let timestamp = &line[..timestamp_end];
    DateTime::parse_from_rfc3339(timestamp).ok()?;

    let remainder = line[timestamp_end..].trim_start();
    let level_end = remainder.find(char::is_whitespace)?;
    let level = &remainder[..level_end];
    if !matches!(level, "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR") {
        return None;
    }

    let remainder = remainder[level_end..].trim_start();
    let (module, message) = remainder
        .split_once(": ")
        .filter(|(candidate, _)| is_miyu_log_target(candidate))
        .unwrap_or(("miyu", remainder));
    Some(ParsedDaemonLogLine {
        timestamp,
        level,
        module,
        message,
    })
}

pub(in crate::cli) fn is_miyu_log_target(value: &str) -> bool {
    value == "miyu"
        || value
            .strip_prefix("miyu::")
            .is_some_and(|suffix| !suffix.is_empty())
}

pub(in crate::cli) fn format_daemon_log_line(line: &str, ansi: bool) -> String {
    let mut decision_color = None;
    format_daemon_log_line_with_state(line, ansi, &mut decision_color)
}

pub(in crate::cli) fn format_daemon_log_line_with_state(
    line: &str,
    ansi: bool,
    decision_color: &mut Option<Color>,
) -> String {
    let Some(parsed) = parse_daemon_log_line(line) else {
        if let Some(color) = active_reply_log_color(line) {
            *decision_color = Some(color);
        }
        return decision_color.map_or_else(
            || line.to_string(),
            |color| color_log_part(line.to_string(), color, ansi),
        );
    };
    *decision_color = active_reply_log_color(parsed.message);
    let timestamp = DateTime::parse_from_rfc3339(parsed.timestamp)
        .map(|value| {
            value
                .with_timezone(&Local)
                .format("%H:%M:%S%.3f")
                .to_string()
        })
        .unwrap_or_else(|_| parsed.timestamp.to_string());
    let timestamp = color_log_part(format!("[{timestamp}]"), Color::DarkGreen, ansi);
    let level = color_log_part(
        format!("[{}]", parsed.level),
        log_level_color(parsed.level),
        ansi,
    );
    let module = color_log_part(format!("[{}]", parsed.module), Color::DarkCyan, ansi);
    if parsed.message.is_empty() {
        format!("{timestamp} {level} {module}")
    } else {
        format!(
            "{timestamp} {level} {module} {}",
            decision_color.map_or_else(
                || parsed.message.to_string(),
                |color| color_log_part(parsed.message.to_string(), color, ansi),
            )
        )
    }
}

pub(in crate::cli) fn active_reply_log_color(value: &str) -> Option<Color> {
    match value.trim_start().lines().next().unwrap_or_default() {
        "【续聊窗口判断：回复】"
        | "【主动回复判断：回复】"
        | "[Continuation decision: reply]"
        | "[Active reply decision: reply]" => Some(Color::Green),
        "【续聊窗口判断：不回复】"
        | "【主动回复判断：不回复】"
        | "[Continuation decision: no reply]"
        | "[Active reply decision: no reply]" => Some(Color::DarkGrey),
        _ => None,
    }
}

pub(in crate::cli) fn log_level_color(level: &str) -> Color {
    match level {
        "ERROR" => Color::Red,
        "WARN" => Color::Yellow,
        "INFO" => Color::Green,
        "DEBUG" => Color::Cyan,
        _ => Color::DarkGrey,
    }
}

pub(in crate::cli) fn color_log_part(value: String, color: Color, ansi: bool) -> String {
    if ansi {
        format!("{}", value.with(color))
    } else {
        value
    }
}

pub(in crate::cli) fn write_daemon_log_lines(lines: &[String], ansi: bool) -> Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut decision_color = None;
    for line in lines {
        writeln!(
            output,
            "{}",
            format_daemon_log_line_with_state(line, ansi, &mut decision_color)
        )?;
    }
    output.flush()?;
    Ok(())
}

#[derive(Default)]
pub(in crate::cli) struct DaemonLogStreamFormatter {
    pub(in crate::cli) pending: Vec<u8>,
    pub(in crate::cli) decision_color: Option<Color>,
}

impl DaemonLogStreamFormatter {
    pub(in crate::cli) fn push(
        &mut self,
        bytes: &[u8],
        ansi: bool,
        output: &mut impl Write,
    ) -> io::Result<()> {
        self.pending.extend_from_slice(bytes);
        let Some(last_newline) = self.pending.iter().rposition(|byte| *byte == b'\n') else {
            return Ok(());
        };
        let remainder = self.pending.split_off(last_newline + 1);
        let complete = std::mem::replace(&mut self.pending, remainder);
        for mut line in complete[..last_newline].split(|byte| *byte == b'\n') {
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            write_daemon_log_line_bytes(line, ansi, &mut self.decision_color, output)?;
        }
        Ok(())
    }

    pub(in crate::cli) fn finish(&mut self, ansi: bool, output: &mut impl Write) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut line = std::mem::take(&mut self.pending);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        write_daemon_log_line_bytes(&line, ansi, &mut self.decision_color, output)
    }
}

pub(in crate::cli) fn write_daemon_log_line_bytes(
    line: &[u8],
    ansi: bool,
    decision_color: &mut Option<Color>,
    output: &mut impl Write,
) -> io::Result<()> {
    writeln!(
        output,
        "{}",
        format_daemon_log_line_with_state(&String::from_utf8_lossy(line), ansi, decision_color,)
    )
}

pub(in crate::cli) fn daemon_log_files(paths: &MiyuPaths) -> Result<Vec<PathBuf>> {
    let mut files = match std::fs::read_dir(paths.logs_dir()) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("miyu.") && name.ends_with(".log"))
            })
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    files.sort();
    // The daemon's inherited stdout/stderr goes to daemon.log. Keep it in
    // the history even when tracing has already created rolling log files;
    // startup failures and panics can happen before the tracing layer writes
    // anything useful.
    let fallback = paths.logs_dir().join("daemon.log");
    if fallback.is_file() && !files.iter().any(|path| path == &fallback) {
        // Treat the unstructured process stream as the oldest source for the
        // bounded recent view. The newest rolling file remains last.
        files.insert(0, fallback);
    }
    Ok(files)
}

pub(in crate::cli) fn is_daemon_fallback_log(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("daemon.log")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::cli) struct DaemonLogFollowCursor {
    pub(in crate::cli) current: Option<PathBuf>,
    pub(in crate::cli) current_offset: u64,
    pub(in crate::cli) fallback: Option<PathBuf>,
    pub(in crate::cli) fallback_offset: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub(in crate::cli) struct DaemonLogSnapshot {
    pub(in crate::cli) lines: Vec<String>,
    pub(in crate::cli) cursor: DaemonLogFollowCursor,
}

pub(in crate::cli) fn daemon_log_follow_cursor_for_files(
    files: &[PathBuf],
    offsets: &[(PathBuf, u64)],
) -> DaemonLogFollowCursor {
    let fallback = files
        .iter()
        .find(|path| is_daemon_fallback_log(path))
        .cloned();
    let current = files
        .iter()
        .rev()
        .find(|path| !is_daemon_fallback_log(path))
        .cloned()
        .or_else(|| fallback.clone());
    let file_offset = |path: Option<&PathBuf>| {
        path.and_then(|path| {
            offsets
                .iter()
                .find(|(candidate, _)| candidate == path)
                .map(|(_, offset)| *offset)
        })
        .unwrap_or_else(|| {
            path.and_then(|path| std::fs::metadata(path).ok())
                .map_or(0, |metadata| metadata.len())
        })
    };
    DaemonLogFollowCursor {
        current_offset: file_offset(current.as_ref()),
        fallback_offset: file_offset(fallback.as_ref()),
        current,
        fallback,
    }
}

pub(in crate::cli) fn recent_daemon_log_snapshot(
    paths: &MiyuPaths,
    limit: usize,
) -> Result<DaemonLogSnapshot> {
    let files = daemon_log_files(paths)?;
    if limit == 0 {
        return Ok(DaemonLogSnapshot {
            lines: Vec::new(),
            cursor: daemon_log_follow_cursor_for_files(&files, &[]),
        });
    }
    // Record the initial EOF of every source before reading any tails. A
    // writer that appends after this point is intentionally left for follow;
    // it can never be lost in the snapshot-to-follow hand-off.
    let mut offsets = files
        .iter()
        .map(|path| {
            let offset = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
            (path.clone(), offset)
        })
        .collect::<Vec<_>>();
    let mut lines = Vec::with_capacity(limit.min(1024));
    for path in files.iter().rev() {
        let remaining = limit.saturating_sub(lines.len());
        if remaining == 0 {
            break;
        }
        let (mut file_lines, end_offset) = tail_file_lines_with_end(path, remaining)?;
        if let Some((_, offset)) = offsets.iter_mut().find(|(candidate, _)| candidate == path) {
            *offset = end_offset;
        }
        file_lines.extend(lines);
        lines = file_lines;
    }
    if lines.len() > limit {
        lines.drain(..lines.len() - limit);
    }
    Ok(DaemonLogSnapshot {
        lines,
        cursor: daemon_log_follow_cursor_for_files(&files, &offsets),
    })
}

pub(in crate::cli) fn recent_daemon_log_lines(
    paths: &MiyuPaths,
    limit: usize,
) -> Result<Vec<String>> {
    Ok(recent_daemon_log_snapshot(paths, limit)?.lines)
}

#[derive(Debug, Eq, PartialEq)]
pub(in crate::cli) struct DaemonLogDelta {
    pub(in crate::cli) bytes: Vec<u8>,
    pub(in crate::cli) next_offset: u64,
    pub(in crate::cli) reset: bool,
}

pub(in crate::cli) fn read_daemon_log_delta(path: &Path, offset: u64) -> Result<DaemonLogDelta> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let reset = len < offset;
    let start = if reset { 0 } else { offset };
    if len == start {
        return Ok(DaemonLogDelta {
            bytes: Vec::new(),
            next_offset: start,
            reset,
        });
    }
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(usize::try_from(len - start).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    Ok(DaemonLogDelta {
        bytes,
        next_offset: file.stream_position()?,
        reset,
    })
}

pub(in crate::cli) fn write_daemon_log_delta(
    path: &Path,
    offset: &mut u64,
    formatter: &mut DaemonLogStreamFormatter,
    ansi: bool,
    output: &mut impl Write,
) -> Result<bool> {
    let delta = read_daemon_log_delta(path, *offset)?;
    if delta.reset {
        formatter.finish(ansi, output)?;
    }
    *offset = delta.next_offset;
    if delta.bytes.is_empty() {
        return Ok(false);
    }
    formatter.push(&delta.bytes, ansi, output)?;
    Ok(true)
}

pub(in crate::cli) fn finish_daemon_log_formatters(
    ansi: bool,
    current: Option<&PathBuf>,
    fallback: Option<&PathBuf>,
    formatter: &mut DaemonLogStreamFormatter,
    fallback_formatter: &mut DaemonLogStreamFormatter,
    output: &mut impl Write,
) -> io::Result<()> {
    if fallback == current {
        fallback_formatter.finish(ansi, output)?;
    } else {
        formatter.finish(ansi, output)?;
        fallback_formatter.finish(ansi, output)?;
    }
    output.flush()
}

pub(in crate::cli) async fn follow_daemon_log(
    paths: &MiyuPaths,
    ansi: bool,
    cursor: DaemonLogFollowCursor,
    initial_pid: u32,
) -> Result<()> {
    let mut current = cursor.current;
    let mut offset = cursor.current_offset;
    let mut fallback = cursor.fallback;
    let mut fallback_offset = cursor.fallback_offset;
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    let mut formatter = DaemonLogStreamFormatter::default();
    let mut fallback_formatter = DaemonLogStreamFormatter::default();
    let mut known_pid = Some(initial_pid);
    let mut daemon_misses = 0_u8;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let mut stdout = io::stdout().lock();
                finish_daemon_log_formatters(
                    ansi,
                    current.as_ref(),
                    fallback.as_ref(),
                    &mut formatter,
                    &mut fallback_formatter,
                    &mut stdout,
                )?;
                return Ok(());
            },
            _ = interval.tick() => {
                let files = daemon_log_files(paths)?;
                let latest = files
                    .iter()
                    .rev()
                    .find(|path| !is_daemon_fallback_log(path))
                    .cloned()
                    .or_else(|| files.iter().find(|path| is_daemon_fallback_log(path)).cloned());
                let latest_fallback = files.iter().find(|path| is_daemon_fallback_log(path)).cloned();
                if latest_fallback != fallback {
                    let mut stdout = io::stdout().lock();
                    if fallback.as_ref().is_some_and(|path| path.is_file()) {
                        write_daemon_log_delta(
                            fallback.as_ref().unwrap(),
                            &mut fallback_offset,
                            &mut fallback_formatter,
                            ansi,
                            &mut stdout,
                        )?;
                    }
                    fallback_formatter.finish(ansi, &mut stdout)?;
                    stdout.flush()?;
                    fallback = latest_fallback;
                    fallback_offset = 0;
                }
                if latest != current {
                    let mut stdout = io::stdout().lock();
                    if let Some(previous) = current
                        .as_ref()
                        .filter(|path| path.is_file() && Some(*path) != fallback.as_ref())
                    {
                        write_daemon_log_delta(
                            previous,
                            &mut offset,
                            &mut formatter,
                            ansi,
                            &mut stdout,
                        )?;
                    }
                    formatter.finish(ansi, &mut stdout)?;
                    stdout.flush()?;
                    current = latest;
                    offset = 0;
                }
                let mut changed = false;
                let mut stdout = io::stdout().lock();
                if let Some(path) = fallback.as_ref().filter(|path| path.is_file()) {
                    changed |= write_daemon_log_delta(
                        path,
                        &mut fallback_offset,
                        &mut fallback_formatter,
                        ansi,
                        &mut stdout,
                    )?;
                }
                if current.as_ref() != fallback.as_ref() {
                    if let Some(path) = current.as_ref().filter(|path| path.is_file()) {
                        changed |= write_daemon_log_delta(
                            path,
                            &mut offset,
                            &mut formatter,
                            ansi,
                            &mut stdout,
                        )?;
                    }
                }
                stdout.flush()?;
                drop(stdout);

                if changed {
                    daemon_misses = 0;
                    continue;
                }

                if let Some(info) = ipc::daemon_info(paths).await {
                    known_pid = Some(info.pid);
                    daemon_misses = 0;
                    continue;
                }

                // `daemon_info` deliberately has a short timeout. A busy
                // daemon can miss one or more probes while still being alive;
                // use the last known PID and a small grace window before
                // treating the stream as finished.
                let alive = known_pid.is_some_and(daemon_process_alive);
                let socket_exists = paths.ipc_socket().exists();
                if socket_exists && (alive || known_pid.is_none()) {
                    daemon_misses = 0;
                    continue;
                }
                daemon_misses = daemon_misses.saturating_add(1);
                if daemon_misses < 3 {
                    continue;
                }

                let mut stdout = io::stdout().lock();
                if let Some(path) = fallback.as_ref().filter(|path| path.is_file()) {
                    write_daemon_log_delta(
                        path,
                        &mut fallback_offset,
                        &mut fallback_formatter,
                        ansi,
                        &mut stdout,
                    )?;
                }
                if current.as_ref() != fallback.as_ref() {
                    if let Some(path) = current.as_ref().filter(|path| path.is_file()) {
                        write_daemon_log_delta(
                            path,
                            &mut offset,
                            &mut formatter,
                            ansi,
                            &mut stdout,
                        )?;
                    }
                }
                finish_daemon_log_formatters(
                    ansi,
                    current.as_ref(),
                    fallback.as_ref(),
                    &mut formatter,
                    &mut fallback_formatter,
                    &mut stdout,
                )?;
                return Ok(());
            }
        }
    }
}
