//! 命令执行的实时显示与输出预览。
//!
//! 命令可能输出几万行，终端里只该看到头尾。`CommandOutputTail` 是个环形缓冲，
//! 中间部分直接丢掉——保留全部再截断意味着一个 `yes` 就能把内存吃光。
//!
//! `sanitize_terminal_text` 是安全边界而非美化：命令输出里的 ANSI 转义序列如果
//! 原样打出去，可以移动光标、改标题、甚至清屏。这里只放行已知安全的一小撮。
//!
//! `decode_utf8_prefix` 处理的是流式解码——一个字符可能被切在两次读取之间。

use crate::render::*;

#[derive(Clone)]
pub(crate) struct CommandLogLine {
    pub(crate) stream: CommandOutputStream,
    pub(crate) text: String,
    pub(crate) sequence: u64,
}

#[derive(Default)]
pub(crate) struct CommandStreamState {
    pub(crate) utf8_pending: Vec<u8>,
    pub(crate) current: String,
    pub(crate) control: TerminalControlState,
    pub(crate) last_update: u64,
    pub(crate) current_sequence: Option<u64>,
    pub(crate) pending_cr: bool,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct CommandOutputPreviewLine {
    pub(crate) stream: &'static str,
    pub(crate) text: String,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct CommandOutputPreview {
    pub(crate) lines: Vec<CommandOutputPreviewLine>,
    pub(crate) omitted: bool,
}

pub(crate) struct CommandOutputTail {
    pub(crate) max_output_rows: usize,
    pub(crate) stdout: CommandStreamState,
    pub(crate) stderr: CommandStreamState,
    pub(crate) completed: VecDeque<CommandLogLine>,
    pub(crate) omitted_lines: bool,
    pub(crate) sequence: u64,
}

impl CommandOutputTail {
    pub(crate) fn new(max_output_rows: usize) -> Self {
        Self {
            max_output_rows,
            stdout: CommandStreamState::default(),
            stderr: CommandStreamState::default(),
            completed: VecDeque::new(),
            omitted_lines: false,
            sequence: 0,
        }
    }

    pub(crate) fn push(&mut self, stream: CommandOutputStream, chunk: &[u8]) {
        self.sequence = self.sequence.wrapping_add(1);
        let completed = match stream {
            CommandOutputStream::Stdout => self.stdout.push(chunk, self.sequence),
            CommandOutputStream::Stderr => self.stderr.push(chunk, self.sequence),
        };
        self.completed.extend(completed.into_iter().map(|mut line| {
            line.stream = stream;
            line
        }));
        let keep = self.max_output_rows.saturating_mul(4).max(100);
        while self.completed.len() > keep {
            self.completed.pop_front();
            self.omitted_lines = true;
        }
    }

    pub(crate) fn finalize(&mut self) {
        self.stdout.finalize_pending(self.sequence);
        self.stderr.finalize_pending(self.sequence);
    }

    pub(crate) fn preview(&self) -> CommandOutputPreview {
        if self.max_output_rows == 0 {
            return CommandOutputPreview {
                lines: Vec::new(),
                omitted: false,
            };
        }
        let logical = self.logical_lines();
        let omitted = self.omitted_lines || logical.len() > self.max_output_rows;
        let start = logical.len().saturating_sub(self.max_output_rows);
        let lines = logical[start..]
            .iter()
            .map(|line| CommandOutputPreviewLine {
                stream: match line.stream {
                    CommandOutputStream::Stdout => "stdout",
                    CommandOutputStream::Stderr => "stderr",
                },
                text: line.text.clone(),
            })
            .collect();
        CommandOutputPreview { lines, omitted }
    }

    pub(crate) fn logical_lines(&self) -> Vec<CommandLogLine> {
        let mut logical = self.completed.iter().cloned().collect::<Vec<_>>();
        let mut pending = [
            (CommandOutputStream::Stdout, &self.stdout),
            (CommandOutputStream::Stderr, &self.stderr),
        ];
        pending.sort_by_key(|(_, state)| state.last_update);
        for (stream, state) in pending {
            if !state.current.is_empty() {
                logical.push(CommandLogLine {
                    stream,
                    text: state.current.clone(),
                    sequence: state.current_sequence.unwrap_or(state.last_update),
                });
            }
        }
        logical.sort_by_key(|line| line.sequence);
        logical
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) enum TerminalControlState {
    #[default]
    Text,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
}

impl CommandStreamState {
    pub(crate) fn push(&mut self, chunk: &[u8], sequence: u64) -> Vec<CommandLogLine> {
        self.last_update = sequence;
        let decoded = decode_utf8_chunk(&mut self.utf8_pending, chunk);
        let mut completed = Vec::new();
        for ch in decoded.chars() {
            let Some(ch) = sanitize_terminal_char(&mut self.control, ch) else {
                continue;
            };
            if self.pending_cr {
                self.pending_cr = false;
                if ch == '\n' {
                    completed.push(CommandLogLine {
                        stream: CommandOutputStream::Stdout,
                        text: std::mem::take(&mut self.current),
                        sequence: self.current_sequence.take().unwrap_or(sequence),
                    });
                    continue;
                }
                self.current.clear();
                self.current_sequence = None;
            }
            match ch {
                '\n' => completed.push(CommandLogLine {
                    stream: CommandOutputStream::Stdout,
                    text: std::mem::take(&mut self.current),
                    sequence: self.current_sequence.take().unwrap_or(sequence),
                }),
                '\r' => self.pending_cr = true,
                '\t' => {
                    self.current_sequence.get_or_insert(sequence);
                    self.current.push_str("    ");
                }
                _ => {
                    self.current_sequence.get_or_insert(sequence);
                    self.current.push(ch);
                }
            }
        }
        const MAX_LIVE_LINE_CHARS: usize = 20_000;
        if self.current.chars().count() > MAX_LIVE_LINE_CHARS {
            self.current = self
                .current
                .chars()
                .rev()
                .take(MAX_LIVE_LINE_CHARS)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
        }
        completed
    }

    pub(crate) fn finalize_pending(&mut self, sequence: u64) {
        if !self.utf8_pending.is_empty() {
            self.utf8_pending.clear();
            self.current_sequence.get_or_insert(sequence);
            self.current.push('\u{fffd}');
        }
        self.pending_cr = false;
        self.control = TerminalControlState::Text;
    }
}

pub(crate) struct CommandLiveDisplay {
    pub(crate) command: String,
    pub(crate) status: CommandStatus,
    pub(crate) max_output_rows: usize,
    pub(crate) show_output: bool,
    pub(crate) show_full_command: bool,
    pub(crate) output: CommandOutputTail,
    pub(crate) frame: usize,
    pub(crate) rendered_line_widths: Vec<usize>,
}

impl CommandLiveDisplay {
    pub(crate) fn new(
        arguments: &str,
        max_output_rows: usize,
        show_output: bool,
        show_full_command: bool,
    ) -> Self {
        Self {
            command: command_from_arguments(arguments),
            status: CommandStatus::Running,
            max_output_rows,
            show_output,
            show_full_command,
            output: CommandOutputTail::new(max_output_rows),
            frame: 0,
            rendered_line_widths: Vec::new(),
        }
    }

    pub(crate) fn set_result(&mut self, ok: bool) {
        self.status = if ok {
            CommandStatus::Ok
        } else {
            CommandStatus::Error
        };
    }

    pub(crate) fn push(&mut self, stream: CommandOutputStream, chunk: &[u8]) {
        self.output.push(stream, chunk);
    }

    pub(crate) fn tick(&mut self, writer: &mut impl Write) -> Result<()> {
        self.redraw(writer, true)?;
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn tick_changes_layout_at_width(&self, width: usize) -> bool {
        let next_widths = self
            .rendered_lines(width, true)
            .iter()
            .map(|line| command_ansi_width(line))
            .collect::<Vec<_>>();
        rendered_physical_rows(&self.rendered_line_widths, width)
            != rendered_physical_rows(&next_widths, width)
    }

    pub(crate) fn redraw(&mut self, writer: &mut impl Write, spinning: bool) -> Result<()> {
        let width = command_terminal_width();
        let lines = self.rendered_lines(width, spinning);
        self.clear(writer)?;
        for (index, line) in lines.iter().enumerate() {
            execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            write!(writer, "{line}")?;
            if index + 1 < lines.len() {
                writeln!(writer)?;
            }
        }
        writer.flush()?;
        self.rendered_line_widths = lines.iter().map(|line| command_ansi_width(line)).collect();
        Ok(())
    }

    pub(crate) fn commit(&mut self, writer: &mut impl Write, include_output: bool) -> Result<()> {
        self.output.finalize();
        let show_output = self.show_output;
        self.show_output = include_output && show_output;
        self.redraw(writer, false)?;
        self.show_output = show_output;
        if !self.rendered_line_widths.is_empty() {
            write_command_block_gap(writer, false)?;
            writer.flush()?;
            self.rendered_line_widths.clear();
        }
        Ok(())
    }

    pub(crate) fn write_static(&mut self, writer: &mut impl Write, include_output: bool) -> Result<()> {
        self.output.finalize();
        let show_output = self.show_output;
        self.show_output = include_output && show_output;
        let lines = self.rendered_lines(command_terminal_width(), false);
        self.show_output = show_output;
        for line in lines {
            writeln!(writer, "{line}")?;
        }
        write_command_block_gap(writer, true)?;
        writer.flush()?;
        Ok(())
    }

    pub(crate) fn clear(&mut self, writer: &mut impl Write) -> Result<()> {
        if self.rendered_line_widths.is_empty() {
            return Ok(());
        }
        let rendered_rows =
            rendered_physical_rows(&self.rendered_line_widths, command_terminal_width());
        if rendered_rows > 1 {
            execute!(writer, MoveUp(rendered_rows - 1))?;
        }
        for index in 0..rendered_rows {
            execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            if index + 1 < rendered_rows {
                writeln!(writer)?;
            }
        }
        if rendered_rows > 1 {
            execute!(writer, MoveUp(rendered_rows - 1))?;
        }
        execute!(writer, MoveToColumn(0))?;
        writer.flush()?;
        self.rendered_line_widths.clear();
        Ok(())
    }

    pub(crate) fn rendered_lines(&self, width: usize, spinning: bool) -> Vec<String> {
        let usable = width.saturating_sub(1).max(5);
        let body_width = usable.saturating_sub(4).max(1);
        let command_lines = render_command_preview(
            &self.command,
            usable,
            self.show_full_command,
            spinning,
            self.frame,
        );
        let mut output = Vec::with_capacity(command_lines.len() + self.max_output_rows + 1);
        output.push(command_heading_line(self.status));
        output.extend(command_lines);
        if self.show_output && self.max_output_rows > 0 {
            output.extend(self.rendered_log_lines(body_width));
        }
        output
    }

    pub(crate) fn rendered_log_lines(&self, body_width: usize) -> Vec<String> {
        let logical = self.output.logical_lines();
        let mut rows = Vec::new();
        for line in logical {
            for text in wrap_plain_text(&line.text, body_width) {
                rows.push(CommandLogLine {
                    stream: line.stream,
                    text,
                    sequence: line.sequence,
                });
            }
        }
        let omitted = self.output.omitted_lines || rows.len() > self.max_output_rows;
        let keep = if omitted && self.max_output_rows > 1 {
            self.max_output_rows - 1
        } else {
            self.max_output_rows
        };
        let start = rows.len().saturating_sub(keep);
        let mut output = Vec::with_capacity(self.max_output_rows);
        if omitted && self.max_output_rows > 1 {
            output.push(format!(
                "\x1b[2m  ⋮ {}\x1b[0m",
                t("earlier output omitted", "已省略较早输出")
            ));
        }
        output.extend(rows[start..].iter().map(|line| {
            let style = match line.stream {
                CommandOutputStream::Stdout => "\x1b[2m",
                CommandOutputStream::Stderr => "\x1b[2m\x1b[31m",
            };
            format!("\x1b[2m  │\x1b[0m {style}{}\x1b[0m", line.text)
        }));
        output
    }
}

pub(crate) fn write_command_block_gap(writer: &mut impl Write, line_terminated: bool) -> Result<()> {
    if !line_terminated {
        writeln!(writer)?;
    }
    writeln!(writer)?;
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) enum CommandStatus {
    Running,
    Ok,
    Error,
}

pub(crate) fn command_heading_line(status: CommandStatus) -> String {
    let status = match status {
        CommandStatus::Running => t("running", "运行中"),
        CommandStatus::Ok => "ok",
        CommandStatus::Error => "err",
    };
    format!(
        "\x1b[2m$ {}×1 {status}\x1b[0m",
        t("run command", "运行命令")
    )
}

pub(crate) fn command_terminal_width() -> usize {
    terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(120)
}

/// 命令家族:走 `CommandLiveDisplay`(带色命令行+输出尾巴)的工具。
/// claude-code 原生 Bash 的入参同样是 `command` 键,与 run_command 同构。
pub(crate) fn is_command_tool(name: &str) -> bool {
    matches!(name, "run_command" | "Bash")
}

pub(crate) fn command_from_arguments(arguments: &str) -> String {
    let parsed = serde_json::from_str::<Value>(arguments).ok();
    let command = parsed
        .as_ref()
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .unwrap_or(arguments);
    sanitize_terminal_text(command).trim().to_string()
}

pub(crate) const COMMAND_PREVIEW_HEAD_LINES: usize = 2;

pub(crate) const COMMAND_PREVIEW_TAIL_LINES: usize = 4;

#[derive(Clone, Copy)]
pub(crate) enum CommandPreviewPrefix {
    First,
    Middle,
    Last,
    SoftWrap,
    LastSoftWrap,
}

pub(crate) fn render_command_preview(
    command: &str,
    width: usize,
    full: bool,
    spinning: bool,
    frame: usize,
) -> Vec<String> {
    let total_lines = command.split('\n').count();
    let compact_lines = COMMAND_PREVIEW_HEAD_LINES + COMMAND_PREVIEW_TAIL_LINES;
    let omitted_lines = if !full && total_lines > compact_lines {
        Some(total_lines - compact_lines)
    } else {
        None
    };
    let logical_lines = if omitted_lines.is_some() {
        command
            .split('\n')
            .take(COMMAND_PREVIEW_HEAD_LINES)
            .chain(
                command
                    .split('\n')
                    .skip(total_lines - COMMAND_PREVIEW_TAIL_LINES),
            )
            .collect::<Vec<_>>()
    } else {
        command.split('\n').collect::<Vec<_>>()
    };
    // Soft-wrap rows have two extra indentation columns after the tree marker.
    let content_width = width.saturating_sub(6).max(1);
    let mut rows = Vec::new();
    for (index, logical_line) in logical_lines.iter().enumerate() {
        if index == COMMAND_PREVIEW_HEAD_LINES {
            if let Some(omitted) = omitted_lines {
                let message = format!(
                    "{} {omitted} {}",
                    t("omitted", "已省略中间"),
                    t("middle lines", "行")
                );
                rows.extend(
                    wrap_plain_text(&message, content_width)
                        .into_iter()
                        .enumerate()
                        .map(|(wrapped_index, text)| {
                            let prefix = if wrapped_index == 0 {
                                "  ⋮ "
                            } else {
                                "  │   "
                            };
                            format!("\x1b[2m{prefix}{text}\x1b[0m")
                        }),
                );
            }
        }
        let wrapped = wrap_plain_text(logical_line, content_width);
        for (wrapped_index, text) in wrapped.iter().enumerate() {
            let first_logical_line = index == 0;
            let last_logical_line = index + 1 == logical_lines.len();
            let last_wrapped_row = wrapped_index + 1 == wrapped.len();
            let prefix = if first_logical_line && wrapped_index == 0 {
                CommandPreviewPrefix::First
            } else if last_logical_line && last_wrapped_row {
                if wrapped_index == 0 {
                    CommandPreviewPrefix::Last
                } else {
                    CommandPreviewPrefix::LastSoftWrap
                }
            } else if wrapped_index > 0 {
                CommandPreviewPrefix::SoftWrap
            } else {
                CommandPreviewPrefix::Middle
            };
            rows.push(format_command_preview_line(prefix, text, spinning, frame));
        }
    }
    rows
}

pub(crate) fn format_command_preview_line(
    prefix: CommandPreviewPrefix,
    text: &str,
    spinning: bool,
    frame: usize,
) -> String {
    let prefix = match prefix {
        CommandPreviewPrefix::First if spinning => format!(
            "\x1b[2m\x1b[36m{}\x1b[0m \x1b[2m↳\x1b[0m ",
            braille_frame(frame)
        ),
        CommandPreviewPrefix::First => "  \x1b[2m↳\x1b[0m ".to_string(),
        CommandPreviewPrefix::Middle => "  \x1b[2m│\x1b[0m ".to_string(),
        CommandPreviewPrefix::Last => "  \x1b[2m└\x1b[0m ".to_string(),
        CommandPreviewPrefix::SoftWrap => "  \x1b[2m│\x1b[0m   ".to_string(),
        CommandPreviewPrefix::LastSoftWrap => "  \x1b[2m└\x1b[0m   ".to_string(),
    };
    format!("{prefix}\x1b[33m{text}\x1b[0m")
}

pub(crate) fn wrap_plain_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if current_width > 0 && current_width + grapheme_width > width {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
    }
    lines.push(current);
    lines
}

pub(crate) fn clip_to_display_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    let ellipsis = "…";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    if max_width <= ellipsis_width {
        return ellipsis.to_string();
    }
    let content_width = max_width - ellipsis_width;
    let mut output = String::new();
    let mut width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > content_width {
            break;
        }
        output.push_str(grapheme);
        width += grapheme_width;
    }
    output.push_str(ellipsis);
    output
}

pub(crate) fn transient_summary_lines(text: &str, terminal_width: usize) -> Vec<String> {
    let max_width = terminal_width.saturating_sub(1).max(1);
    let mut lines = text
        .lines()
        .map(|line| clip_to_display_width(line, max_width))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(crate) fn command_ansi_width(text: &str) -> usize {
    let mut plain = String::new();
    let mut state = TerminalControlState::Text;
    for ch in text.chars() {
        if let Some(ch) = sanitize_terminal_char(&mut state, ch) {
            plain.push(ch);
        }
    }
    UnicodeWidthStr::width(plain.as_str())
}

pub(crate) fn sanitize_terminal_text(text: &str) -> String {
    let mut state = CommandStreamState::default();
    let completed = state.push(text.as_bytes(), 0);
    state.finalize_pending(0);
    let mut lines = completed
        .into_iter()
        .map(|line| line.text)
        .collect::<Vec<_>>();
    if !state.current.is_empty() {
        lines.push(state.current);
    }
    lines.join("\n")
}

pub(crate) fn decode_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8]) -> String {
    pending.extend_from_slice(chunk);
    let bytes = std::mem::take(pending);
    let mut output = String::new();
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(text) => {
                output.push_str(text);
                break;
            }
            Err(error) => {
                let valid_end = offset + error.valid_up_to();
                output.push_str(std::str::from_utf8(&bytes[offset..valid_end]).unwrap_or_default());
                match error.error_len() {
                    Some(length) => {
                        output.push('\u{fffd}');
                        offset = valid_end + length;
                    }
                    None => {
                        pending.extend_from_slice(&bytes[valid_end..]);
                        break;
                    }
                }
            }
        }
    }
    output
}

pub(crate) fn sanitize_terminal_char(state: &mut TerminalControlState, ch: char) -> Option<char> {
    match *state {
        TerminalControlState::Text => {
            if ch == '\x1b' {
                *state = TerminalControlState::Escape;
                None
            } else if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t') {
                None
            } else {
                Some(ch)
            }
        }
        TerminalControlState::Escape => {
            *state = match ch {
                '[' => TerminalControlState::Csi,
                ']' | 'P' | 'X' | '^' | '_' => TerminalControlState::Osc,
                ' '..='/' => TerminalControlState::EscapeIntermediate,
                _ => TerminalControlState::Text,
            };
            None
        }
        TerminalControlState::EscapeIntermediate => {
            if ('0'..='~').contains(&ch) {
                *state = TerminalControlState::Text;
            }
            None
        }
        TerminalControlState::Csi => {
            if ('@'..='~').contains(&ch) {
                *state = TerminalControlState::Text;
            }
            None
        }
        TerminalControlState::Osc => {
            if ch == '\x07' {
                *state = TerminalControlState::Text;
            } else if ch == '\x1b' {
                *state = TerminalControlState::OscEscape;
            }
            None
        }
        TerminalControlState::OscEscape => {
            *state = if ch == '\\' {
                TerminalControlState::Text
            } else {
                TerminalControlState::Osc
            };
            None
        }
    }
}

pub(crate) fn write_command_block(stdout: &mut impl Write, arguments: &str) -> Result<()> {
    write_command_block_with_status(stdout, arguments, CommandStatus::Running)
}

pub(crate) fn write_command_block_with_status(
    stdout: &mut impl Write,
    arguments: &str,
    status: CommandStatus,
) -> Result<()> {
    let command = command_from_arguments(arguments);
    writeln!(stdout, "{}", command_heading_line(status))?;
    let terminal_width = terminal::size().map(|(w, _)| usize::from(w)).unwrap_or(120);
    let usable = terminal_width.saturating_sub(1).max(5);
    for line in render_command_preview(&command, usable, true, false, 0) {
        writeln!(stdout, "{line}")?;
    }
    Ok(())
}

pub(crate) fn write_command_result_blocks(stdout: &mut impl Write, output: &str) -> Result<()> {
    let Some(result) = parse_command_result(output) else {
        return write_tool_payload(stdout, t("output", "输出"), &sanitize_terminal_text(output));
    };
    if !result.stdout.trim().is_empty() {
        write_fenced_block(stdout, t("output", "输出"), &result.stdout)?;
    }
    if !result.stderr.trim().is_empty() {
        let label = result
            .exit_code
            .map(|code| format!("err exit {code}"))
            .unwrap_or_else(|| "err".to_string());
        write_fenced_block(stdout, &label, &result.stderr)?;
    } else if !result.success {
        let label = result
            .exit_code
            .map(|code| format!("err exit {code}"))
            .unwrap_or_else(|| "err".to_string());
        write_fenced_block(
            stdout,
            &label,
            t(
                "command failed without stderr",
                "命令失败，但没有 stderr 输出",
            ),
        )?;
    }
    Ok(())
}

pub(crate) fn write_fenced_block(stdout: &mut impl Write, label: &str, text: &str) -> Result<()> {
    writeln!(stdout, "\x1b[2m,-- {label}\x1b[0m")?;
    let sanitized = sanitize_terminal_text(text);
    let style = if label.starts_with("err") {
        "\x1b[2m\x1b[31m"
    } else {
        "\x1b[2m"
    };
    for line in truncate_chars(sanitized.trim(), 2400).lines() {
        writeln!(stdout, "{style}{line}\x1b[0m")?;
    }
    writeln!(stdout, "\x1b[2m`--\x1b[0m")?;
    Ok(())
}

pub(crate) struct CommandResult {
    pub(crate) success: bool,
    pub(crate) exit_code: Option<i64>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// 解析 dsh 式纯文本命令结果(08-17 起 run_command/grep/glob 的形态):
/// 正文是 stdout,可选 `[stderr]` 段,末尾可选 `[exit code: N]` /
/// `[killed by signal]` 标记。老的 JSON 形态仍然认——历史回合里还躺着
/// 一批,渲染层不能因为换了形态就把它们变成裸 JSON。
pub(crate) fn parse_command_result(output: &str) -> Option<CommandResult> {
    let text = output.trim();
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(success) = value.get("success").and_then(Value::as_bool) {
            return Some(CommandResult {
                success,
                exit_code: value.get("exit_code").and_then(Value::as_i64),
                stdout: value
                    .get("stdout")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                stderr: value
                    .get("stderr")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        return None;
    }

    let mut body = text;
    let mut exit_code = Some(0);
    let mut success = true;
    if let Some(rest) = body.strip_suffix("]") {
        if let Some((head, marker)) = rest.rsplit_once("\n[") {
            if let Some(code) = marker.strip_prefix("exit code: ") {
                if let Ok(code) = code.trim().parse::<i64>() {
                    body = head;
                    exit_code = Some(code);
                    success = code == 0;
                }
            } else if marker == "killed by signal" {
                body = head;
                exit_code = None;
                success = false;
            }
        }
    }
    let (stdout, stderr) = match body.split_once("[stderr]\n") {
        Some((out, err)) => (out.trim_end(), err),
        None => (body, ""),
    };
    let stdout = if stdout.trim() == "(no output)" {
        ""
    } else {
        stdout
    };
    Some(CommandResult {
        success,
        exit_code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    })
}
