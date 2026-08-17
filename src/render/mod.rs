pub(crate) mod math;
pub(crate) mod wait_spinner;

use crate::i18n::text as t;
use crate::llm::{ChatResult, ChatStreamChunk, ChatStreamKind, Usage};
use crate::render::wait_spinner::{braille_frame, SpinnerStyle, WaitSpinner, SPINNER_INTERVAL};
use crate::tools::CommandOutputStream;
use anyhow::Result;
use crossterm::cursor::{Hide, MoveToColumn, MoveUp, Show};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{execute, terminal};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, IsTerminal, Write};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

fn rendered_physical_rows(widths: &[usize], terminal_width: usize) -> u16 {
    let columns = terminal_width.max(1);
    widths
        .iter()
        .map(|width| (*width).max(1).div_ceil(columns))
        .sum::<usize>()
        .min(u16::MAX as usize) as u16
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReasoningDisplayMode {
    Hidden,
    Summary,
    Full,
}

impl ReasoningDisplayMode {
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "hidden" => Self::Hidden,
            "full" => Self::Full,
            _ => Self::Summary,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCallDisplayMode {
    Hidden,
    Summary,
    Full,
}

impl ToolCallDisplayMode {
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "hidden" => Self::Hidden,
            "full" => Self::Full,
            _ => Self::Summary,
        }
    }
}

#[derive(Clone)]
struct CommandLogLine {
    stream: CommandOutputStream,
    text: String,
    sequence: u64,
}

#[derive(Default)]
struct CommandStreamState {
    utf8_pending: Vec<u8>,
    current: String,
    control: TerminalControlState,
    last_update: u64,
    current_sequence: Option<u64>,
    pending_cr: bool,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct CommandOutputPreviewLine {
    stream: &'static str,
    text: String,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct CommandOutputPreview {
    lines: Vec<CommandOutputPreviewLine>,
    omitted: bool,
}

pub(crate) struct CommandOutputTail {
    max_output_rows: usize,
    stdout: CommandStreamState,
    stderr: CommandStreamState,
    completed: VecDeque<CommandLogLine>,
    omitted_lines: bool,
    sequence: u64,
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

    fn logical_lines(&self) -> Vec<CommandLogLine> {
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
enum TerminalControlState {
    #[default]
    Text,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
}

impl CommandStreamState {
    fn push(&mut self, chunk: &[u8], sequence: u64) -> Vec<CommandLogLine> {
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

    fn finalize_pending(&mut self, sequence: u64) {
        if !self.utf8_pending.is_empty() {
            self.utf8_pending.clear();
            self.current_sequence.get_or_insert(sequence);
            self.current.push('\u{fffd}');
        }
        self.pending_cr = false;
        self.control = TerminalControlState::Text;
    }
}

struct CommandLiveDisplay {
    command: String,
    status: CommandStatus,
    max_output_rows: usize,
    show_output: bool,
    show_full_command: bool,
    output: CommandOutputTail,
    frame: usize,
    rendered_line_widths: Vec<usize>,
}

impl CommandLiveDisplay {
    fn new(
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

    fn set_result(&mut self, ok: bool) {
        self.status = if ok {
            CommandStatus::Ok
        } else {
            CommandStatus::Error
        };
    }

    fn push(&mut self, stream: CommandOutputStream, chunk: &[u8]) {
        self.output.push(stream, chunk);
    }

    fn tick(&mut self, writer: &mut impl Write) -> Result<()> {
        self.redraw(writer, true)?;
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }

    #[cfg(test)]
    fn tick_changes_layout_at_width(&self, width: usize) -> bool {
        let next_widths = self
            .rendered_lines(width, true)
            .iter()
            .map(|line| command_ansi_width(line))
            .collect::<Vec<_>>();
        rendered_physical_rows(&self.rendered_line_widths, width)
            != rendered_physical_rows(&next_widths, width)
    }

    fn redraw(&mut self, writer: &mut impl Write, spinning: bool) -> Result<()> {
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

    fn commit(&mut self, writer: &mut impl Write, include_output: bool) -> Result<()> {
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

    fn write_static(&mut self, writer: &mut impl Write, include_output: bool) -> Result<()> {
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

    fn clear(&mut self, writer: &mut impl Write) -> Result<()> {
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

    fn rendered_lines(&self, width: usize, spinning: bool) -> Vec<String> {
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

    fn rendered_log_lines(&self, body_width: usize) -> Vec<String> {
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

fn write_command_block_gap(writer: &mut impl Write, line_terminated: bool) -> Result<()> {
    if !line_terminated {
        writeln!(writer)?;
    }
    writeln!(writer)?;
    Ok(())
}

#[derive(Clone, Copy)]
enum CommandStatus {
    Running,
    Ok,
    Error,
}

fn command_heading_line(status: CommandStatus) -> String {
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

fn command_terminal_width() -> usize {
    terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(120)
}

fn command_from_arguments(arguments: &str) -> String {
    let parsed = serde_json::from_str::<Value>(arguments).ok();
    let command = parsed
        .as_ref()
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .unwrap_or(arguments);
    sanitize_terminal_text(command).trim().to_string()
}

const COMMAND_PREVIEW_HEAD_LINES: usize = 2;
const COMMAND_PREVIEW_TAIL_LINES: usize = 4;

#[derive(Clone, Copy)]
enum CommandPreviewPrefix {
    First,
    Middle,
    Last,
    SoftWrap,
    LastSoftWrap,
}

fn render_command_preview(
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

fn format_command_preview_line(
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

fn wrap_plain_text(text: &str, width: usize) -> Vec<String> {
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

fn clip_to_display_width(text: &str, max_width: usize) -> String {
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

fn transient_summary_lines(text: &str, terminal_width: usize) -> Vec<String> {
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

fn command_ansi_width(text: &str) -> usize {
    let mut plain = String::new();
    let mut state = TerminalControlState::Text;
    for ch in text.chars() {
        if let Some(ch) = sanitize_terminal_char(&mut state, ch) {
            plain.push(ch);
        }
    }
    UnicodeWidthStr::width(plain.as_str())
}

fn sanitize_terminal_text(text: &str) -> String {
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

fn decode_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8]) -> String {
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

fn sanitize_terminal_char(state: &mut TerminalControlState, ch: char) -> Option<char> {
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

pub fn print_assistant_response(response: &ChatResult, show_reasoning: bool) -> Result<()> {
    if show_reasoning {
        if let Some(reasoning) = response
            .reasoning
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            print_reasoning(reasoning)?;
        }
    }
    print_markdown(&response.content);
    Ok(())
}

pub fn print_markdown(markdown: &str) {
    let skin = termimad::MadSkin::default();
    println!("{}", skin.term_text(markdown.trim_end()));
}

/// Everything the token meters show. Grouped into one struct because the two
/// cache rates each need a numerator *and* a denominator, and threading eight
/// loose `u64`s through four call layers was already past readable.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokenMeter {
    pub turn_tokens: u64,
    /// Denominator of the turn cache rate. A cache hit is an input-side
    /// property — output tokens only enter the prompt on the *next* turn — so
    /// the rate is read/prompt, never read/total, which is what every provider
    /// reports too (DeepSeek splits the prompt into hit+miss; OpenAI's
    /// `cached_tokens` is a subset of `prompt_tokens`; Anthropic names all
    /// three fields `*_input_tokens`).
    pub turn_prompt_tokens: u64,
    pub turn_cached_tokens: u64,
    pub session_tokens: u64,
    pub context_window: Option<usize>,
    /// Σ: session-lifetime total. `None` hides it on narrow terminals.
    pub cumulative_tokens: Option<u64>,
    pub cumulative_prompt_tokens: u64,
    pub cumulative_cached_tokens: u64,
}

/// `None` when there is nothing honest to report: a provider that never said
/// anything about caching must not be rendered as a flat 0%.
pub(crate) fn cache_percent(cached: u64, prompt: u64) -> Option<u64> {
    (cached > 0 && prompt > 0)
        .then(|| ((cached as f64 / prompt as f64) * 100.0).round().min(100.0) as u64)
}

fn cache_suffix(cached: u64, prompt: u64) -> String {
    cache_percent(cached, prompt)
        .map(|percent| format!("(C{percent}%)"))
        .unwrap_or_default()
}

pub fn print_token_usage(meter: &TokenMeter, estimated: bool) -> Result<()> {
    let output = token_usage_output(meter, estimated);
    let mut stdout = io::stdout();
    write!(stdout, "{output}")?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn token_usage_output(meter: &TokenMeter, estimated: bool) -> String {
    let prefix = if estimated {
        t("Estimated ", "估算")
    } else {
        ""
    };
    let line = format!("{prefix}Token: {}", format_token_usage_inline(meter));
    format!("\x1b[2m{line}\x1b[0m\n\n")
}

pub(crate) fn format_token_usage_inline(meter: &TokenMeter) -> String {
    format_token_usage_inline_opts(meter, true)
}

pub(crate) fn format_token_usage_inline_opts(meter: &TokenMeter, show_percent: bool) -> String {
    let context_window = meter.context_window.map(|value| value as u64);
    let context = context_window
        .map(format_compact_count)
        .unwrap_or_else(|| "?".to_string());
    let usage_ratio = if let Some(context_window) = context_window.filter(|value| *value > 0) {
        format!(
            "{:.1}%",
            meter.session_tokens as f64 / context_window as f64 * 100.0
        )
    } else {
        "?".to_string()
    };

    let mut session = if show_percent {
        format!(
            "{}/{}({usage_ratio})",
            format_compact_count(meter.session_tokens),
            context,
        )
    } else {
        format!("{}/{}", format_compact_count(meter.session_tokens), context)
    };
    if let Some(cumulative_tokens) = meter.cumulative_tokens {
        session.push_str(&format!(
            " · Σ{}{}",
            format_compact_count(cumulative_tokens),
            cache_suffix(
                meter.cumulative_cached_tokens,
                meter.cumulative_prompt_tokens
            ),
        ));
    }
    if meter.turn_tokens == 0 {
        session
    } else {
        format!(
            "{}{} · {session}",
            format_compact_count(meter.turn_tokens),
            cache_suffix(meter.turn_cached_tokens, meter.turn_prompt_tokens),
        )
    }
}

pub fn usage_total(usage: &Usage) -> u64 {
    usage.effective_total_tokens()
}

pub(crate) fn format_compact_count(value: u64) -> String {
    const K: f64 = 1_000.0;
    const M: f64 = 1_000_000.0;
    if value >= 1_000_000 {
        format_compact_unit(value as f64 / M, "M")
    } else if value >= 1_000 {
        format_compact_unit(value as f64 / K, "k")
    } else {
        value.to_string()
    }
}

fn format_compact_unit(value: f64, suffix: &str) -> String {
    if (value.fract() - 0.0).abs() < f64::EPSILON {
        format!("{value:.0}{suffix}")
    } else {
        format!("{value:.1}{suffix}")
    }
}

enum RenderOutput {
    Terminal,
    Buffered(Vec<u8>),
}

impl Write for RenderOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Terminal => io::stdout().write(bytes),
            Self::Buffered(buffer) => buffer.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Terminal => io::stdout().flush(),
            Self::Buffered(_) => Ok(()),
        }
    }
}

pub struct StreamRenderer {
    reasoning_mode: ReasoningDisplayMode,
    tool_call_mode: ToolCallDisplayMode,
    plain: bool,
    mode: Option<ChatStreamKind>,
    cursor_hidden: bool,
    external_cursor_control: bool,
    output: RenderOutput,
    markdown: MarkdownStreamRenderer,
    reasoning_text: String,
    reasoning_tokens: usize,
    reasoning_title: Option<String>,
    reasoning_started_at: Option<std::time::Instant>,
    reasoning_elapsed: Option<std::time::Duration>,
    tool_stats: BTreeMap<String, ToolStats>,
    tool_seq: usize,
    readable_tool_names: bool,
    command_output_lines: usize,
    command_display: Option<CommandLiveDisplay>,
    summary_line_active: bool,
    summary_lines_active: u16,
    last_tool_summary: String,
    live_summary: bool,
    wait_spinner: Option<WaitSpinner>,
    last_tick: Option<std::time::Instant>,
    preparing_question_started_at: Option<std::time::Instant>,
    /// Phase text and start time for the "still receiving arguments" hint.
    /// Sticky like `preparing_question_started_at` and for the same reason:
    /// `tick_spinner` re-derives the phase from renderer state on every tick,
    /// so a phase merely pushed into the spinner is overwritten before it can
    /// be drawn.
    tool_preparing: Option<(&'static str, std::time::Instant)>,
    subagent_mode: Option<ChatStreamKind>,
    sent_meme_filter: SentMemeStreamFilter,
    /// 模型正文/思维链的流式转义过滤状态:与命令输出同一套状态机,
    /// 拦截 `\x1b[2J`/OSC 等正文里的终端控制序列(清屏/藏光标/伪造 UI)。
    stream_control: TerminalControlState,
}

impl StreamRenderer {
    pub fn new(
        reasoning_mode: ReasoningDisplayMode,
        tool_call_mode: ToolCallDisplayMode,
        plain: bool,
        readable_tool_names: bool,
        command_output_lines: usize,
    ) -> Self {
        Self {
            reasoning_mode,
            tool_call_mode,
            plain,
            mode: None,
            cursor_hidden: false,
            external_cursor_control: false,
            output: RenderOutput::Terminal,
            markdown: MarkdownStreamRenderer::new(),
            reasoning_text: String::new(),
            reasoning_tokens: 0,
            reasoning_title: None,
            reasoning_started_at: None,
            reasoning_elapsed: None,
            tool_stats: BTreeMap::new(),
            tool_seq: 0,
            readable_tool_names,
            command_output_lines,
            command_display: None,
            summary_line_active: false,
            summary_lines_active: 0,
            last_tool_summary: String::new(),
            live_summary: io::stdout().is_terminal(),
            wait_spinner: None,
            last_tick: None,
            preparing_question_started_at: None,
            tool_preparing: None,
            subagent_mode: None,
            sent_meme_filter: SentMemeStreamFilter::default(),
            stream_control: TerminalControlState::default(),
        }
    }

    pub fn use_external_cursor_control(&mut self) {
        self.external_cursor_control = true;
    }

    pub fn use_buffered_output(&mut self) {
        self.output = RenderOutput::Buffered(Vec::new());
    }

    pub fn take_output_frame(&mut self) -> Vec<u8> {
        match &mut self.output {
            RenderOutput::Terminal => Vec::new(),
            RenderOutput::Buffered(buffer) => std::mem::take(buffer),
        }
    }

    pub fn start_waiting(&mut self) -> Result<()> {
        if self.plain
            || self.wait_spinner.is_some()
            || self.command_display.is_some()
            || !WaitSpinner::supported()
        {
            return Ok(());
        }
        self.hide_cursor()?;
        let phase = self.waiting_phase_text();
        self.wait_spinner = Some(WaitSpinner::start(phase, SpinnerStyle::Scanner));
        self.last_tick = None;
        self.tick_spinner()?;
        Ok(())
    }

    pub fn start_reasoning_phase(&mut self, received_at: std::time::Instant) -> Result<()> {
        self.preparing_question_started_at = None;
        self.tool_preparing = None;
        if self.reasoning_mode == ReasoningDisplayMode::Summary {
            self.reasoning_started_at = Some(received_at);
            self.reasoning_elapsed = None;
            self.reasoning_title = None;
            self.reasoning_text.clear();
            self.reasoning_tokens = 0;
        }
        self.start_waiting()?;
        if self.wait_spinner.is_some() {
            self.set_waiting_phase(self.waiting_phase_text());
            self.last_tick = None;
            self.tick_spinner()?;
        }
        Ok(())
    }

    fn waiting_phase_text(&self) -> String {
        if let Some(started_at) = self.preparing_question_started_at {
            return format!(
                "{} · {}",
                t("~ Preparing question", "~ 准备问题"),
                format_reasoning_elapsed(started_at.elapsed())
            );
        }
        if let Some((phase, started_at)) = self.tool_preparing {
            return format!(
                "~ {phase} · {}",
                format_reasoning_elapsed(started_at.elapsed())
            );
        }
        match self.reasoning_mode {
            ReasoningDisplayMode::Summary => {
                if self.reasoning_title.is_some() || !self.reasoning_text.is_empty() {
                    self.reasoning_live_text()
                } else {
                    self.reasoning_elapsed_text()
                }
            }
            ReasoningDisplayMode::Full => String::new(),
            ReasoningDisplayMode::Hidden => t("thinking", "思考").to_string(),
        }
    }

    pub fn write_reasoning_title(&mut self, title: &str) -> Result<()> {
        if self.reasoning_mode != ReasoningDisplayMode::Summary || self.plain {
            return Ok(());
        }
        let title = redact_sensitive_inline(&sanitize_terminal_text(title));
        let title = clip_progress_line(&title, 80);
        if title.is_empty() {
            return Ok(());
        }
        self.reasoning_title = Some(title);
        self.ensure_waiting_phase(self.reasoning_live_text(), SpinnerStyle::Scanner)
    }

    pub fn start_reasoning_part(&mut self, received_at: std::time::Instant) -> Result<()> {
        if self.reasoning_mode != ReasoningDisplayMode::Summary {
            return Ok(());
        }
        self.end_active_stream_line()?;
        if self.reasoning_title.is_some() || !self.reasoning_text.is_empty() {
            self.freeze_reasoning_elapsed_at(received_at);
            self.finalize_reasoning_summary()?;
            self.reasoning_started_at = Some(received_at);
        } else if self.reasoning_started_at.is_none() {
            self.reasoning_started_at = Some(received_at);
        }
        self.reasoning_elapsed = None;
        self.reasoning_title = None;
        self.reasoning_text.clear();
        self.reasoning_tokens = 0;
        self.start_waiting()
    }

    pub fn finish_reasoning_part(&mut self, received_at: std::time::Instant) -> Result<()> {
        if self.reasoning_mode != ReasoningDisplayMode::Summary {
            return Ok(());
        }
        if self.reasoning_title.is_some() || !self.reasoning_text.is_empty() {
            self.freeze_reasoning_elapsed_at(received_at);
            self.finalize_reasoning_summary()?;
            self.reasoning_started_at = Some(received_at);
            self.reasoning_elapsed = None;
        }
        Ok(())
    }

    pub fn reset_reasoning_phase(&mut self, received_at: std::time::Instant) -> Result<()> {
        if self.reasoning_mode != ReasoningDisplayMode::Summary {
            return Ok(());
        }
        self.stop_waiting()?;
        if self.summary_line_active {
            self.clear_summary_lines()?;
        }
        self.reasoning_title = None;
        self.reasoning_text.clear();
        self.reasoning_tokens = 0;
        self.reasoning_started_at = Some(received_at);
        self.reasoning_elapsed = None;
        self.mode = None;
        self.start_waiting()
    }

    pub fn tick_spinner(&mut self) -> Result<()> {
        let now = std::time::Instant::now();
        let should_tick = self
            .last_tick
            .map(|last| now.duration_since(last) >= SPINNER_INTERVAL)
            .unwrap_or(true);
        if should_tick {
            let subagent_timer_active = self.has_running_subagent_timer();
            // Both sticky hints win over the tool/reasoning summaries below:
            // they describe what the turn is blocked on right now, and the
            // summaries would otherwise overwrite them on the very first tick
            // after they are set — before the spinner has drawn once.
            if (self.preparing_question_started_at.is_some() || self.tool_preparing.is_some())
                && self.wait_spinner.is_some()
            {
                self.set_waiting_phase(self.waiting_phase_text());
            } else if self.tool_call_mode == ToolCallDisplayMode::Summary
                && !self.tool_stats.is_empty()
                && self.wait_spinner.is_some()
            {
                let (header, sub) = self.tool_summary_live();
                self.set_tool_waiting_phase(&header, sub.as_deref());
            } else if self.reasoning_mode == ReasoningDisplayMode::Summary
                && self.reasoning_started_at.is_some()
                && self.wait_spinner.is_some()
            {
                self.set_waiting_phase(self.waiting_phase_text());
            }
            if let Some(display) = &mut self.command_display {
                debug_assert!(self.wait_spinner.is_none());
                display.tick(&mut self.output)?;
            } else if let Some(spinner) = &mut self.wait_spinner {
                spinner.tick(&mut self.output)?;
            }
            if self.wait_spinner.is_some()
                || self.command_display.is_some()
                || subagent_timer_active
            {
                self.last_tick = Some(now);
            }
        }
        Ok(())
    }

    pub fn write_chunk(&mut self, chunk: ChatStreamChunk) -> Result<()> {
        if chunk.kind == ChatStreamKind::ToolCall {
            if chunk.text == "ask_question" {
                self.start_preparing_question()?;
            }
            return Ok(());
        }
        if matches!(
            chunk.kind,
            ChatStreamKind::ReasoningPartStart
                | ChatStreamKind::ReasoningPartEnd
                | ChatStreamKind::ReasoningReset
        ) {
            return Ok(());
        }
        if !self.plain {
            self.hide_cursor()?;
        }
        let text = normalize_stream_text(&chunk.text);
        // 正文/思维链与命令输出同权:全部过转义状态机,模型输出里的
        // `\x1b[2J`、OSC 8 等控制序列不能直接打到用户终端上生效。
        // 状态跨 delta 持有,序列被 delta 切断也拦得住。
        let text = sanitize_stream_chunk(&mut self.stream_control, &text);
        let text = if chunk.kind == ChatStreamKind::Content {
            self.sent_meme_filter.push(&text)
        } else {
            text
        };
        if text.is_empty() {
            return Ok(());
        }
        if self.plain && chunk.kind == ChatStreamKind::Reasoning {
            return Ok(());
        }
        if self.reasoning_mode == ReasoningDisplayMode::Hidden
            && chunk.kind == ChatStreamKind::Reasoning
        {
            return Ok(());
        }
        if self.reasoning_mode == ReasoningDisplayMode::Summary
            && chunk.kind == ChatStreamKind::Reasoning
        {
            self.finalize_tools_summary()?;
            self.record_reasoning_text(&text);
            self.mode = Some(ChatStreamKind::Reasoning);
            self.ensure_waiting_phase(self.reasoning_live_text(), SpinnerStyle::Scanner)?;
            return Ok(());
        }
        self.stop_waiting()?;
        if self.mode != Some(chunk.kind) {
            if chunk.kind == ChatStreamKind::Content {
                self.finalize_reasoning_summary()?;
                self.finalize_tools_summary()?;
            } else if chunk.kind == ChatStreamKind::Reasoning {
                self.finalize_tools_summary()?;
            }
            self.switch_mode(chunk.kind)?;
        }
        let stdout = &mut self.output;
        if chunk.kind == ChatStreamKind::Reasoning {
            write_full_reasoning_chunk(stdout, &text)?;
        } else if self.plain {
            write!(stdout, "{text}")?;
        } else {
            write!(stdout, "{}", self.markdown.push(&text))?;
        }
        stdout.flush()?;
        Ok(())
    }

    pub fn write_tool_call(&mut self, name: &str, arguments: &str) -> Result<()> {
        // The arguments finished arriving, so the "still receiving" hint has
        // done its job and hands the spinner back to the tool summary.
        self.tool_preparing = None;
        if self.plain {
            return Ok(());
        }
        if name == "ask_question" {
            return self.start_preparing_question();
        }
        self.release_transient_output()?;
        if is_silent_tool(name) {
            return Ok(());
        }
        if name == "run_command" {
            let mut display = CommandLiveDisplay::new(
                arguments,
                self.command_output_lines,
                self.tool_call_mode != ToolCallDisplayMode::Hidden,
                self.tool_call_mode == ToolCallDisplayMode::Full,
            );
            if self.live_summary {
                display.tick(&mut self.output)?;
                self.last_tick = None;
            }
            self.command_display = Some(display);
            return Ok(());
        }
        if is_subagent_tool(name) && self.tool_call_mode != ToolCallDisplayMode::Hidden {
            let stats = self.tool_stats_entry(name);
            stats.started_at = Some(std::time::Instant::now());
            stats.elapsed = None;
        }
        if self.tool_call_mode == ToolCallDisplayMode::Full {
            let display_name = self.display_tool_name(name);
            let stdout = &mut self.output;
            writeln!(stdout, "{} {}", t("tool", "工具"), display_name)?;
            write_tool_payload(stdout, t("args", "参数"), arguments)?;
            stdout.flush()?;
        } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
            let stats = self.tool_stats_entry(name);
            stats.calls += 1;
            stats.subject = tool_subject(name, arguments);
            self.ensure_tool_waiting_phase()?;
        }
        Ok(())
    }

    pub fn write_tool_preparing(&mut self, name: &str) -> Result<()> {
        if self.plain {
            return Ok(());
        }
        let Some(phase) = crate::tools::preparing_phase(name) else {
            return Ok(());
        };
        self.release_transient_output()?;
        // Set before the spinner exists: `ensure_waiting_phase` ticks
        // immediately, and that tick re-derives the phase from renderer state.
        // Without the sticky field the tool summary or the reasoning timer wins
        // there and the hint never reaches the screen.
        self.tool_preparing = Some((phase, std::time::Instant::now()));
        // Braille + the dim tool palette: this is a tool starting up, not the
        // model thinking, and the scanner/green pair reads as the latter.
        self.ensure_waiting_phase(self.waiting_phase_text(), SpinnerStyle::Braille)
    }

    pub fn write_tool_result(&mut self, name: &str, ok: bool, output: &str) -> Result<()> {
        if self.plain {
            return Ok(());
        }
        if is_silent_tool(name) && ok {
            return Ok(());
        }
        self.stop_waiting()?;
        self.end_subagent_stream_line()?;
        let status = if ok { "ok" } else { "err" };
        let elapsed = self.finish_subagent_timer(name);
        if name == "run_command" {
            if let Some(mut display) = self.command_display.take() {
                display.set_result(ok);
                let include_output = self.tool_call_mode == ToolCallDisplayMode::Summary
                    || (self.tool_call_mode == ToolCallDisplayMode::Full && !ok);
                if self.live_summary {
                    display.commit(&mut self.output, include_output)?;
                } else {
                    display.write_static(&mut self.output, include_output)?;
                }
                self.last_tick = None;
            }
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                let stdout = &mut self.output;
                write_command_result_blocks(stdout, output)?;
                stdout.flush()?;
            }
            return Ok(());
        }
        if matches!(name, "todowrite" | "todoupdate") && ok {
            self.release_transient_output()?;
            let stdout = &mut self.output;
            if write_todo_table(stdout, output)? {
                stdout.flush()?;
                if self.tool_call_mode == ToolCallDisplayMode::Summary {
                    let stats = self.tool_stats_entry(name);
                    stats.ok += 1;
                    stats.progress = None;
                    self.tool_stats.clear();
                    self.last_tool_summary.clear();
                }
                return Ok(());
            }
        }
        if self.tool_call_mode == ToolCallDisplayMode::Full {
            self.release_transient_output()?;
            let display_name = self.display_tool_name(name);
            let stdout = &mut self.output;
            writeln!(
                stdout,
                "{} {} {}",
                t("result", "结果"),
                display_name,
                tool_result_status(status, elapsed)
            )?;
            write_tool_payload(stdout, t("output", "输出"), output)?;
            stdout.flush()?;
            self.tool_stats.remove(name);
        } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
            let stats = self.tool_stats_entry(name);
            if ok {
                stats.ok += 1;
            } else {
                stats.error += 1;
            }
            stats.progress = None;
            if self.tool_stats.values().any(|stats| !stats.settled()) {
                // Siblings still running (parallel subagents): freeze this
                // tool's block in the live area; commit only when the whole
                // batch settles.
                self.update_tool_summary_display()?;
            } else {
                self.finalize_tools_summary()?;
            }
        }
        Ok(())
    }

    pub fn write_command_output(
        &mut self,
        name: &str,
        stream: CommandOutputStream,
        chunk: &[u8],
    ) -> Result<()> {
        if self.plain || name != "run_command" {
            return Ok(());
        }
        if let Some(display) = &mut self.command_display {
            display.push(stream, chunk);
        }
        Ok(())
    }

    pub fn write_tool_progress(&mut self, name: &str, message: &str) -> Result<()> {
        if let Some(phase) = message.strip_prefix("__tool_phase__") {
            if self.plain {
                let stdout = &mut self.output;
                writeln!(stdout, "{phase}")?;
                stdout.flush()?;
            } else if self.wait_spinner.is_some() {
                self.set_waiting_phase(phase.to_string());
                self.tick_spinner()?;
            } else {
                self.render_summary_line(phase, SummaryStyle::Tool)?;
            }
            return Ok(());
        }
        if let Some(json) = message.strip_prefix("__patch_preview__") {
            self.release_transient_output()?;
            let stdout = &mut self.output;
            if write_patch_result(stdout, json)? {
                stdout.flush()?;
            }
            return Ok(());
        }
        if self.plain {
            return Ok(());
        }
        if message == "__external_output__" {
            self.prepare_for_external_output()?;
            return Ok(());
        }
        if let Some(text) = message.strip_prefix("__subagent_detach__") {
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                self.release_transient_output()?;
                let display_name = self.display_tool_name(name);
                let stdout = &mut self.output;
                writeln!(stdout, "{} {}: {text}", t("progress", "进度"), display_name)?;
                stdout.flush()?;
            } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
                // Lands as the block's `↳` subject line, not the final `✓`
                // stats line — detach is a fact about the call, not a result.
                let stats = self.tool_stats_entry(name);
                stats.subject = Some(text.to_string());
                stats.detached = true;
                self.update_tool_summary_display()?;
            }
            return Ok(());
        }
        if let Some(text) = message.strip_prefix(crate::tools::TOOL_SUMMARY_PREFIX) {
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                self.release_transient_output()?;
                let stdout = &mut self.output;
                for line in text.lines() {
                    writeln!(stdout, "{line}")?;
                }
                stdout.flush()?;
            } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
                self.tool_stats_entry(name).final_progress = Some(text.to_string());
                self.update_tool_summary_display()?;
            }
            return Ok(());
        }
        if let Some(text) = message.strip_prefix("__subagent_stats__") {
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                self.release_transient_output()?;
                let display_name = self.display_tool_name(name);
                let stdout = &mut self.output;
                writeln!(stdout, "{} {}: {text}", t("progress", "进度"), display_name)?;
                stdout.flush()?;
            } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
                self.tool_stats_entry(name).final_progress = Some(text.to_string());
                self.update_tool_summary_display()?;
            }
            return Ok(());
        }
        if let Some(text) = message.strip_prefix("__subagent_reasoning__") {
            let text = normalize_stream_text(text);
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                if self.subagent_mode != Some(ChatStreamKind::Reasoning) {
                    self.stop_waiting()?;
                    self.clear_summary_lines()?;
                    self.end_active_stream_line()?;
                    let stdout = &mut self.output;
                    writeln!(stdout)?;
                    stdout.flush()?;
                }
                let stdout = &mut self.output;
                write_full_reasoning_chunk(stdout, &text)?;
                stdout.flush()?;
                self.subagent_mode = Some(ChatStreamKind::Reasoning);
            }
            return Ok(());
        }
        if let Some(json) = message.strip_prefix("__subtool_call__") {
            if let Ok(value) = serde_json::from_str::<Value>(json) {
                let tool_name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                if self.tool_call_mode == ToolCallDisplayMode::Full {
                    let args = value.get("args").and_then(Value::as_str).unwrap_or("");
                    self.release_transient_output()?;
                    let display_name = self.display_tool_name(tool_name);
                    let stdout = &mut self.output;
                    if tool_name == "run_command" {
                        write_command_block(stdout, args)?;
                    } else {
                        writeln!(stdout, "{} {}", t("tool", "工具"), display_name)?;
                        write_tool_payload(stdout, t("args", "参数"), args)?;
                    }
                    stdout.flush()?;
                }
            }
            return Ok(());
        }
        if let Some(json) = message.strip_prefix("__subtool_result__") {
            if let Ok(value) = serde_json::from_str::<Value>(json) {
                let tool_name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
                if self.tool_call_mode == ToolCallDisplayMode::Full {
                    let args = value.get("args").and_then(Value::as_str).unwrap_or("");
                    let output = value.get("output").and_then(Value::as_str).unwrap_or("");
                    let status = if ok { "ok" } else { "err" };
                    self.release_transient_output()?;
                    let display_name = self.display_tool_name(tool_name);
                    let stdout = &mut self.output;
                    if tool_name == "run_command" {
                        write_command_block_with_status(
                            stdout,
                            args,
                            if ok {
                                CommandStatus::Ok
                            } else {
                                CommandStatus::Error
                            },
                        )?;
                        write_command_result_blocks(stdout, output)?;
                        write_command_block_gap(stdout, true)?;
                    } else {
                        writeln!(stdout, "{} {} {status}", t("result", "结果"), display_name)?;
                        write_tool_payload(stdout, t("output", "输出"), output)?;
                    }
                    stdout.flush()?;
                }
            }
            return Ok(());
        }
        if is_silent_tool(name) {
            return Ok(());
        }
        if self.tool_call_mode == ToolCallDisplayMode::Full {
            self.release_transient_output()?;
            let display_name = self.display_tool_name(name);
            let stdout = &mut self.output;
            writeln!(
                stdout,
                "{} {}: {message}",
                t("progress", "进度"),
                display_name
            )?;
            stdout.flush()?;
        } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
            self.tool_stats_entry(name).progress = Some(message.to_string());
            self.update_tool_summary_display()?;
        }
        Ok(())
    }

    fn update_tool_summary_display(&mut self) -> Result<()> {
        self.end_subagent_stream_line()?;
        if self.wait_spinner.is_some() {
            let (header, sub) = self.tool_summary_live();
            self.set_tool_waiting_phase(&header, sub.as_deref());
        } else {
            self.end_active_stream_line()?;
            self.finalize_reasoning_summary()?;
            self.ensure_tool_waiting_phase()?;
        }
        Ok(())
    }

    pub fn prepare_for_external_output(&mut self) -> Result<()> {
        self.preparing_question_started_at = None;
        self.tool_preparing = None;
        self.release_transient_output()?;
        self.finalize_tools_summary()?;
        self.show_cursor()?;
        Ok(())
    }

    pub fn write_system_message(&mut self, message: &str) -> Result<()> {
        self.prepare_for_external_output()?;
        let stdout = &mut self.output;
        execute!(stdout, SetForegroundColor(Color::DarkGrey), MoveToColumn(0))?;
        writeln!(stdout, "{message}")?;
        execute!(stdout, ResetColor)?;
        stdout.flush()?;
        Ok(())
    }

    pub fn write_compact_chunk(&mut self, chunk: &ChatStreamChunk) -> Result<()> {
        if chunk.kind != ChatStreamKind::Content {
            return Ok(());
        }
        self.prepare_for_external_output()?;
        let stdout = &mut self.output;
        execute!(stdout, SetForegroundColor(Color::DarkGrey))?;
        write!(stdout, "{}", chunk.text)?;
        execute!(stdout, ResetColor)?;
        stdout.flush()?;
        Ok(())
    }

    pub fn finish_compact(&mut self) -> Result<()> {
        let stdout = &mut self.output;
        execute!(stdout, ResetColor)?;
        writeln!(stdout)?;
        stdout.flush()?;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        self.preparing_question_started_at = None;
        self.tool_preparing = None;
        self.stop_waiting()?;
        if let Some(mut display) = self.command_display.take() {
            display.commit(
                &mut self.output,
                self.tool_call_mode == ToolCallDisplayMode::Summary,
            )?;
        }
        self.end_subagent_stream_line()?;
        if self.mode == Some(ChatStreamKind::Content) && !self.plain {
            let stdout = &mut self.output;
            let pending = self.sent_meme_filter.finish();
            if !pending.is_empty() {
                write!(stdout, "{}", self.markdown.push(&pending))?;
            }
            write!(stdout, "{}", self.markdown.flush())?;
            stdout.flush()?;
        }
        if self.mode == Some(ChatStreamKind::Reasoning) {
            execute!(self.output, ResetColor)?;
        }
        if stream_needs_terminating_newline(self.mode, self.reasoning_mode) {
            writeln!(self.output)?;
        }
        self.finalize_reasoning_summary()?;
        self.finalize_tools_summary()?;
        if self.summary_line_active {
            self.clear_summary_lines()?;
        }
        self.mode = None;
        self.show_cursor()?;
        Ok(())
    }

    fn switch_mode(&mut self, mode: ChatStreamKind) -> Result<()> {
        let stdout = &mut self.output;
        match mode {
            ChatStreamKind::Reasoning => {
                if self.mode.is_some() {
                    writeln!(stdout)?;
                }
            }
            ChatStreamKind::Content => {
                if self.mode == Some(ChatStreamKind::Reasoning) {
                    execute!(stdout, ResetColor)?;
                    writeln!(stdout)?;
                    writeln!(stdout)?;
                }
            }
            ChatStreamKind::ToolCall => return Ok(()),
            ChatStreamKind::ReasoningPartStart | ChatStreamKind::ReasoningPartEnd => return Ok(()),
            ChatStreamKind::ReasoningReset => return Ok(()),
        }
        stdout.flush()?;
        self.mode = Some(mode);
        Ok(())
    }

    fn end_active_stream_line(&mut self) -> Result<()> {
        if self.reasoning_mode == ReasoningDisplayMode::Summary
            && self.mode == Some(ChatStreamKind::Reasoning)
        {
            self.mode = None;
            return Ok(());
        }
        let was_reasoning = self.mode == Some(ChatStreamKind::Reasoning);
        if was_reasoning {
            execute!(self.output, ResetColor)?;
        } else if self.mode == Some(ChatStreamKind::Content) && !self.plain {
            let stdout = &mut self.output;
            write!(stdout, "{}", self.markdown.flush())?;
            stdout.flush()?;
        }
        if self.mode.is_some() {
            writeln!(self.output)?;
            if was_reasoning {
                writeln!(self.output)?;
            }
            self.mode = None;
        }
        Ok(())
    }

    fn finalize_reasoning_summary(&mut self) -> Result<()> {
        if self.reasoning_mode == ReasoningDisplayMode::Summary
            && (self.reasoning_title.is_some() || !self.reasoning_text.is_empty())
        {
            self.stop_waiting()?;
            let summary = self.reasoning_summary_text();
            if self.summary_line_active {
                self.clear_summary_lines()?;
                self.summary_line_active = false;
                self.summary_lines_active = 0;
            }
            let stdout = &mut self.output;
            write_activity_summary(stdout, &summary, SummaryStyle::Reasoning)?;
            stdout.flush()?;
            self.reasoning_text.clear();
            self.reasoning_tokens = 0;
            self.reasoning_title = None;
            self.reasoning_started_at = None;
            self.reasoning_elapsed = None;
            self.mode = None;
        }
        Ok(())
    }

    fn end_subagent_stream_line(&mut self) -> Result<()> {
        let was_reasoning = self.subagent_mode == Some(ChatStreamKind::Reasoning);
        if was_reasoning {
            execute!(self.output, ResetColor)?;
        }
        if self.subagent_mode.is_some() {
            writeln!(self.output)?;
            if was_reasoning {
                writeln!(self.output)?;
            }
            self.subagent_mode = None;
        }
        Ok(())
    }

    fn finalize_tools_summary(&mut self) -> Result<()> {
        if self.tool_call_mode == ToolCallDisplayMode::Summary && !self.tool_stats.is_empty() {
            self.stop_waiting()?;
            execute!(self.output, ResetColor)?;
            let summary = self.tool_summary_text();
            if self.summary_line_active {
                self.clear_summary_lines()?;
                self.summary_line_active = false;
                self.summary_lines_active = 0;
            }
            let stdout = &mut self.output;
            write_activity_summary(stdout, &summary, SummaryStyle::Tool)?;
            stdout.flush()?;
            self.tool_stats.clear();
            self.last_tool_summary.clear();
        }
        Ok(())
    }

    fn render_summary_line(&mut self, text: &str, style: SummaryStyle) -> Result<()> {
        self.stop_waiting()?;
        if !self.live_summary {
            return Ok(());
        }
        self.clear_summary_lines()?;
        let stdout = &mut self.output;
        let lines = transient_summary_lines(text, command_terminal_width());
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                writeln!(stdout)?;
            }
            execute!(stdout, MoveToColumn(0))?;
            write!(stdout, "{}\x1b[K", style_summary_text(line, style))?;
        }
        stdout.flush()?;
        self.summary_line_active = true;
        self.summary_lines_active = lines.len().max(1) as u16;
        Ok(())
    }

    fn clear_summary_lines(&mut self) -> Result<()> {
        if !self.summary_line_active {
            return Ok(());
        }
        let stdout = &mut self.output;
        let lines = self.summary_lines_active.max(1);
        for index in 0..lines {
            if index > 0 {
                execute!(stdout, crossterm::cursor::MoveUp(1))?;
            }
            execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        }
        stdout.flush()?;
        self.summary_line_active = false;
        self.summary_lines_active = 0;
        Ok(())
    }

    fn reasoning_summary_text(&self) -> String {
        let elapsed = self.reasoning_elapsed_text();
        format!("{} · {elapsed}", self.reasoning_live_metrics_text())
    }

    fn reasoning_live_text(&self) -> String {
        if self.reasoning_started_at.is_none() {
            return match &self.reasoning_title {
                Some(title) if crate::i18n::is_zh() => {
                    format!("{}：{title}", t("thinking", "思考"))
                }
                Some(title) => format!("{}: {title}", t("thinking", "思考")),
                None => t("thinking", "思考").to_string(),
            };
        }
        let elapsed = self.reasoning_elapsed_text();
        format!("{} · {elapsed}", self.reasoning_live_metrics_text())
    }

    fn reasoning_elapsed_text(&self) -> String {
        self.reasoning_elapsed
            .or_else(|| self.reasoning_started_at.map(|started| started.elapsed()))
            .map(format_reasoning_elapsed)
            .unwrap_or_else(|| "0ms".to_string())
    }

    fn freeze_reasoning_elapsed_at(&mut self, received_at: std::time::Instant) {
        self.reasoning_elapsed = self
            .reasoning_started_at
            .map(|started_at| received_at.saturating_duration_since(started_at));
    }

    fn reasoning_live_metrics_text(&self) -> String {
        let phase = match &self.reasoning_title {
            Some(title) if crate::i18n::is_zh() => {
                format!("{}：{title}", t("thinking", "思考"))
            }
            Some(title) => format!("{}: {title}", t("thinking", "思考")),
            None => t("thinking", "思考").to_string(),
        };
        if self.reasoning_tokens == 0 {
            return phase;
        }
        format!(
            "{phase} · {} {}",
            self.reasoning_tokens,
            t("tokens", "词元")
        )
    }

    fn record_reasoning_text(&mut self, text: &str) {
        self.reasoning_started_at
            .get_or_insert_with(std::time::Instant::now);
        self.reasoning_text.push_str(text);
        // Incremental: recounting the whole accumulated text on every chunk is
        // O(n²) over the stream and the value only feeds the spinner label.
        // Per-chunk sums drift <1% from a full recount (BPE merges across
        // chunk boundaries) — fine for a display estimate.
        self.reasoning_tokens += crate::token_estimate::estimate_tokens(text);
    }

    /// Gets or creates a tool's stats entry, stamping first-seen order so
    /// parallel blocks render in launch order rather than name order.
    fn tool_stats_entry(&mut self, name: &str) -> &mut ToolStats {
        self.tool_seq += 1;
        let seq = self.tool_seq;
        self.tool_stats
            .entry(name.to_string())
            .or_insert_with(|| ToolStats {
                seq,
                ..ToolStats::default()
            })
    }

    /// Tools in first-seen order (stable for direct test inserts with seq 0).
    fn ordered_tool_stats(&self) -> Vec<(&String, &ToolStats)> {
        let mut entries: Vec<_> = self.tool_stats.iter().collect();
        entries.sort_by_key(|(_, stats)| stats.seq);
        entries
    }

    fn tool_summary_text(&self) -> String {
        self.ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| self.tool_block_lines(name, stats, false).join("\n"))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Builds one tool's display block: a `~`-prefixed status header plus
    /// its own subject/progress lines. In `live` mode a still-running tool's
    /// header carries [`wait_spinner::BLOCK_MARKER`] so the spinner animates
    /// it, and a settled tool freezes into its final `✓` stats in place. The
    /// committed variant (`live == false`) prefers `final_progress` with `✓`.
    fn tool_block_lines(&self, name: &str, stats: &ToolStats, live: bool) -> Vec<String> {
        let display = self.display_tool_name(name);
        let mut header = tool_status_text(&display, stats, is_subagent_tool(name));
        if inline_tool_subject(name) {
            if let Some(subject) = &stats.subject {
                header.push_str(" · ");
                header.push_str(subject);
            }
        }
        let header = self.tool_summary_with_prefix(header);
        // In live mode a running block's detail lines are indented to sit
        // under its spinner glyph; a settled block drops the glyph and sits
        // flush, matching the committed layout.
        let running_live = live && !stats.settled();
        // Detail lines always sit two columns in, matching command blocks
        // (`$ …` / `  ↳ cmd` / `  │ output`) and avoiding the leftward jump
        // a block used to make when it settled.
        let detail_indent = "  ";
        let mut lines = Vec::new();
        if running_live {
            lines.push(format!("{}{header}", wait_spinner::BLOCK_MARKER));
        } else {
            lines.push(header);
        }
        if !inline_tool_subject(name) {
            if let Some(subject) = &stats.subject {
                // Subagent headers already carry the description — don't
                // repeat it as a subject line.
                if !lines[0].contains(subject.as_str()) {
                    lines.push(format!("{detail_indent}↳ {subject}"));
                }
            }
        }
        let (progress_text, is_final) = if live {
            if stats.settled() {
                (stats.final_progress.as_ref(), true)
            } else {
                (stats.progress.as_ref(), false)
            }
        } else if stats.final_progress.is_some() {
            (stats.final_progress.as_ref(), true)
        } else {
            (stats.progress.as_ref(), false)
        };
        let progress_prefix = if is_final { "✓" } else { "↳" };
        if let Some(message) = progress_text {
            for line in message.lines().filter(|line| !line.trim().is_empty()) {
                let line = if is_final {
                    clip_progress_line_preserving_spaces(line, 120)
                } else {
                    clip_progress_line(line, 120)
                };
                // 自带记号的行原样保留:失败清单是 `✗ …`,再套一个 `✓` 就成了
                // 「✓ ✗ 权限不足」。
                if line.starts_with('✗') {
                    lines.push(format!("{detail_indent}{line}"));
                } else {
                    lines.push(format!("{detail_indent}{progress_prefix} {line}"));
                }
            }
        }
        lines
    }

    fn tool_summary_header(&self) -> String {
        let parts = self
            .ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| {
                let display = self.display_tool_name(name);
                let mut header = tool_status_text(&display, stats, is_subagent_tool(name));
                if inline_tool_subject(name) {
                    if let Some(subject) = &stats.subject {
                        header.push_str(" · ");
                        header.push_str(subject);
                    }
                }
                header
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.tool_summary_with_prefix(parts)
    }

    /// Live status for the wait spinner. A single tool keeps the classic
    /// one-line phase + progress sub-block. Multiple tools (e.g. parallel
    /// subagents) switch the spinner into block mode: the phase line is
    /// empty and every tool renders as its own block — running blocks carry
    /// their own animated glyph, settled blocks freeze into their final
    /// stats, and blocks are separated by blank lines:
    ///
    /// ```text
    /// ⠋ ~ 子代理·任务A×1 运行中 · 3s
    ///   ↳ 任务A进度
    ///
    ///   ~ 子代理·任务B×1 ok · 2s
    ///   ✓ 工具调用 1 次
    /// ```
    fn tool_summary_live(&self) -> (String, Option<String>) {
        if self.tool_stats.len() <= 1 {
            return (self.tool_summary_header(), self.tool_summary_progress());
        }
        let blocks = self
            .ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| self.tool_block_lines(name, stats, true).join("\n"))
            .collect::<Vec<_>>()
            .join("\n\n");
        (String::new(), Some(blocks))
    }

    fn tool_summary_with_prefix(&self, parts: String) -> String {
        if self.tool_stats.len() == 1
            && self
                .tool_stats
                .keys()
                .next()
                .is_some_and(|name| name == "run_command")
        {
            format!("$ {parts}")
        } else {
            format!("~ {parts}")
        }
    }

    fn tool_summary_progress(&self) -> Option<String> {
        for (name, stats) in self.ordered_tool_stats() {
            let mut lines = Vec::new();
            if !inline_tool_subject(name) {
                if let Some(subject) = &stats.subject {
                    // Skip subjects already shown in the header (subagent
                    // descriptions are part of the display name).
                    if !self.display_tool_name(name).contains(subject.as_str()) {
                        lines.push(format!("  ↳ {subject}"));
                    }
                }
            }
            if let Some(message) = &stats.progress {
                let progress = message
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| format!("  ↳ {}", clip_progress_line(line, 120)))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !progress.is_empty() {
                    lines.push(progress);
                }
            }
            if !lines.is_empty() {
                return Some(lines.join("\n"));
            }
        }
        None
    }

    fn has_running_subagent_timer(&self) -> bool {
        self.tool_stats
            .iter()
            .any(|(name, stats)| is_subagent_tool(name) && stats.started_at.is_some())
    }

    fn finish_subagent_timer(&mut self, name: &str) -> Option<std::time::Duration> {
        if !is_subagent_tool(name) {
            return None;
        }
        let stats = self.tool_stats.get_mut(name)?;
        let elapsed = stats.started_at.take()?.elapsed();
        stats.elapsed = Some(elapsed);
        Some(elapsed)
    }

    fn display_tool_name<'a>(&self, name: &'a str) -> String {
        // Subagents keep their per-call description so parallel task calls
        // show as separate lines: "子代理·<描述>".
        if let Some(description) = name.strip_prefix("task:") {
            let base = if self.readable_tool_names {
                readable_tool_name("task")
            } else {
                "task".to_string()
            };
            return format!("{base}·{description}");
        }
        let name = tool_event_base_name(name);
        if self.readable_tool_names {
            readable_tool_name(name)
        } else {
            name.to_string()
        }
    }

    fn hide_cursor(&mut self) -> Result<()> {
        if self.external_cursor_control {
            return Ok(());
        }
        if !self.cursor_hidden && !self.plain && self.wait_spinner.is_none() {
            execute!(self.output, Hide)?;
            self.cursor_hidden = true;
        }
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<()> {
        if self.external_cursor_control {
            return Ok(());
        }
        if self.cursor_hidden && !self.plain {
            execute!(self.output, Show)?;
            self.cursor_hidden = false;
        }
        Ok(())
    }

    fn set_waiting_phase(&mut self, phase: String) {
        if let Some(spinner) = &mut self.wait_spinner {
            spinner.set_phase(phase);
        }
    }

    fn ensure_waiting_phase(&mut self, phase: String, style: SpinnerStyle) -> Result<()> {
        if self.command_display.is_some() {
            return Ok(());
        }
        if self.plain || !WaitSpinner::supported() {
            if self.summary_line_active {
                self.clear_summary_lines()?;
            }
            self.render_summary_line(&phase, summary_style_for(style))?;
            return Ok(());
        }
        if self.wait_spinner.is_none() {
            self.wait_spinner = Some(WaitSpinner::start(phase, style));
            self.last_tick = None;
            self.tick_spinner()?;
        } else {
            self.set_waiting_phase(phase);
        }
        Ok(())
    }

    fn ensure_tool_waiting_phase(&mut self) -> Result<()> {
        debug_assert!(self.command_display.is_none());
        let (header, sub) = self.tool_summary_live();
        if self.plain || !self.live_summary {
            let summary = match &sub {
                Some(s) if header.is_empty() => s.clone(),
                Some(s) => format!("{header}\n{s}"),
                None => header,
            };
            let summary = summary.replace(wait_spinner::BLOCK_MARKER, "");
            if self.summary_line_active {
                self.clear_summary_lines()?;
            }
            self.last_tool_summary = summary.clone();
            return self.render_summary_line(&summary, SummaryStyle::Tool);
        }
        if self.summary_line_active {
            self.clear_summary_lines()?;
        }
        if self.wait_spinner.is_none() {
            self.hide_cursor()?;
            self.wait_spinner = Some(WaitSpinner::start(header, SpinnerStyle::Braille));
            self.last_tick = None;
        } else {
            self.set_waiting_phase(header);
        }
        if let Some(spinner) = &mut self.wait_spinner {
            spinner.set_sub_phase(sub);
        }
        self.tick_spinner()
    }

    fn start_preparing_question(&mut self) -> Result<()> {
        if self.plain || self.preparing_question_started_at.is_some() {
            return Ok(());
        }
        self.release_transient_output()?;
        self.preparing_question_started_at = Some(std::time::Instant::now());
        if !WaitSpinner::supported() {
            return Ok(());
        }
        self.hide_cursor()?;
        self.wait_spinner = Some(WaitSpinner::start(
            self.waiting_phase_text(),
            SpinnerStyle::Braille,
        ));
        self.last_tick = None;
        self.tick_spinner()
    }

    fn set_tool_waiting_phase(&mut self, header: &str, sub: Option<&str>) {
        if let Some(spinner) = &mut self.wait_spinner {
            spinner.set_phase(header.to_string());
            spinner.set_sub_phase(sub.map(|s| s.to_string()));
        }
    }

    fn stop_waiting(&mut self) -> Result<()> {
        if let Some(mut spinner) = self.wait_spinner.take() {
            spinner.stop(&mut self.output)?;
        }
        self.last_tick = None;
        Ok(())
    }

    fn release_transient_output(&mut self) -> Result<()> {
        self.stop_waiting()?;
        if let Some(mut display) = self.command_display.take() {
            display.commit(
                &mut self.output,
                self.tool_call_mode == ToolCallDisplayMode::Summary,
            )?;
        }
        self.end_subagent_stream_line()?;
        self.end_active_stream_line()?;
        self.finalize_reasoning_summary()?;
        self.clear_summary_lines()
    }
}

fn stream_needs_terminating_newline(
    mode: Option<ChatStreamKind>,
    reasoning_mode: ReasoningDisplayMode,
) -> bool {
    mode.is_some()
        && !(mode == Some(ChatStreamKind::Reasoning)
            && reasoning_mode == ReasoningDisplayMode::Summary)
}

#[derive(Default)]
struct SentMemeStreamFilter {
    pending: String,
    inside_tag: bool,
}

impl SentMemeStreamFilter {
    fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        let mut output = String::new();
        loop {
            if self.inside_tag {
                if let Some(end) = self.pending.find("</sent_meme>") {
                    let after = end + "</sent_meme>".len();
                    self.pending.drain(..after);
                    self.inside_tag = false;
                    continue;
                }
                self.pending.clear();
                return output;
            }

            let Some(start) = self.pending.find("<sent_meme>") else {
                let keep = longest_sent_meme_prefix_suffix(&self.pending);
                let emit_len = self.pending.len().saturating_sub(keep);
                output.push_str(&self.pending[..emit_len]);
                self.pending.drain(..emit_len);
                return output;
            };

            output.push_str(&self.pending[..start]);
            self.pending.drain(..start + "<sent_meme>".len());
            self.inside_tag = true;
        }
    }

    fn finish(&mut self) -> String {
        if self.inside_tag {
            self.pending.clear();
            self.inside_tag = false;
            return String::new();
        }
        std::mem::take(&mut self.pending)
    }
}

fn longest_sent_meme_prefix_suffix(text: &str) -> usize {
    const TAG: &str = "<sent_meme>";
    let max = TAG.len().saturating_sub(1).min(text.len());
    for len in (1..=max).rev() {
        if text.ends_with(&TAG[..len]) {
            return len;
        }
    }
    0
}

#[derive(Default)]
struct ToolStats {
    calls: usize,
    ok: usize,
    error: usize,
    subject: Option<String>,
    progress: Option<String>,
    final_progress: Option<String>,
    started_at: Option<std::time::Instant>,
    elapsed: Option<std::time::Duration>,
    /// The subagent handed itself off to the background. Its call returned at
    /// once, so the elapsed timer would only ever read `0s` — and worse, imply
    /// the work finished instantly. The job strip tracks it from here on.
    detached: bool,
    seq: usize,
}

impl ToolStats {
    fn elapsed(&self) -> Option<std::time::Duration> {
        self.elapsed
            .or_else(|| self.started_at.map(|started| started.elapsed()))
    }

    /// Every issued call has completed (ok or err) — nothing running.
    fn settled(&self) -> bool {
        self.calls > 0 && self.ok + self.error >= self.calls
    }
}

#[derive(Clone, Copy)]
enum SummaryStyle {
    Reasoning,
    Tool,
}

/// The still-line equivalent of a spinner style, for terminals that cannot
/// animate — so a phase keeps its identity (thinking vs tool) either way.
fn summary_style_for(style: SpinnerStyle) -> SummaryStyle {
    match style {
        SpinnerStyle::Scanner => SummaryStyle::Reasoning,
        SpinnerStyle::Braille => SummaryStyle::Tool,
    }
}

fn style_summary_text(text: &str, style: SummaryStyle) -> String {
    match style {
        SummaryStyle::Reasoning => format!("\x1b[38;5;10m{text}\x1b[0m"),
        SummaryStyle::Tool => format!("\x1b[2m{text}\x1b[0m"),
    }
}

fn write_activity_summary(writer: &mut impl Write, text: &str, style: SummaryStyle) -> Result<()> {
    writeln!(writer, "{}", style_summary_text(text, style))?;
    writeln!(writer)?;
    Ok(())
}

fn tool_status_text(name: &str, stats: &ToolStats, subagent: bool) -> String {
    let calls = stats.calls.max(stats.ok + stats.error).max(1);
    let running = stats.calls.saturating_sub(stats.ok + stats.error);
    let text = if calls == 1 && running > 0 {
        format!("{name}×1 {}", t("running", "运行中"))
    } else if calls == 1 && stats.error > 0 {
        format!("{name}×1 err")
    } else if calls == 1 && stats.ok > 0 {
        format!("{name}×1 ok")
    } else if running > 0 {
        let mut text = format!(
            "{name}×{calls} {}:{} ok:{}",
            t("running", "运行中"),
            running,
            stats.ok,
        );
        if stats.error > 0 {
            text.push_str(&format!(" err:{}", stats.error));
        }
        text
    } else if stats.error > 0 {
        format!("{name}×{calls} ok:{} err:{}", stats.ok, stats.error)
    } else {
        format!("{name}×{calls} ok:{}", stats.ok)
    };
    if subagent && !stats.detached {
        if let Some(elapsed) = stats.elapsed() {
            return format!("{text} · {}", format_elapsed(elapsed));
        }
    }
    text
}

fn tool_result_status(status: &str, elapsed: Option<std::time::Duration>) -> String {
    elapsed.map_or_else(
        || status.to_string(),
        |elapsed| format!("{status} · {}", format_elapsed(elapsed)),
    )
}

fn format_elapsed(elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3_600, (seconds % 3_600) / 60)
    }
}

fn format_reasoning_elapsed(elapsed: std::time::Duration) -> String {
    if elapsed < std::time::Duration::from_millis(1) {
        "<1ms".to_string()
    } else if elapsed < std::time::Duration::from_secs(1) {
        format!("{}ms", elapsed.as_millis())
    } else if elapsed < std::time::Duration::from_secs(60) {
        format!("{:.1}s", elapsed.as_secs_f64())
    } else if elapsed < std::time::Duration::from_secs(3_600) {
        format!("{}m {:02}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
    } else {
        format!(
            "{}h {:02}m",
            elapsed.as_secs() / 3_600,
            (elapsed.as_secs() % 3_600) / 60
        )
    }
}

fn is_silent_tool(name: &str) -> bool {
    matches!(name, "show_meme" | "ask_question")
}

fn is_subagent_tool(name: &str) -> bool {
    let name = tool_event_base_name(name);
    matches!(name, "deep_research" | "task")
}

fn tool_event_base_name(name: &str) -> &str {
    if name.starts_with("load_skill:") {
        "load_skill"
    } else if name.starts_with("load_tools:") {
        "load_tools"
    } else if name.starts_with("task:") {
        "task"
    } else {
        name
    }
}

fn inline_tool_subject(name: &str) -> bool {
    // 回收站的 subject 是条数,贴在标题上比单占一行更紧凑,
    // 而成功时整个块本来就只有这一行。
    matches!(tool_event_base_name(name), "load_tools" | "trash_path")
}

pub(crate) fn tool_subject(name: &str, arguments: &str) -> Option<String> {
    let args = serde_json::from_str::<Value>(arguments).ok()?;
    let name = tool_event_base_name(name);
    let value = match name {
        "task" => string_arg(&args, &["description"]),
        "web_search"
        | "search_web_images"
        | "search_meme"
        | "search_knowledge_base"
        | "search_evicted_context"
        | "recall_memories"
        | "recall_past_events"
        | "aur_search_packages"
        | "online_man_search"
        | "protondb_query"
        | "query_caniplayonlinux"
        | "fcitx5_input_method_wiki_qurey" => string_arg(&args, &["query", "topic"]),
        "archwiki_query" | "query_moegirl" => string_arg(&args, &["title", "query"]),
        "search_knowledge_base_by_name" => string_arg(&args, &["file_name_query"]),
        "read_file" => {
            let path = string_arg(&args, &["path"])?;
            Some(match read_page_label(&args) {
                Some(page) => format!("{path} ({page})"),
                None => path,
            })
        }
        "write_file" | "edit_file" | "edit_string" | "register_script" => {
            string_arg(&args, &["path"])
        }
        "trash_path" => {
            let paths = args.get("paths").and_then(Value::as_array)?;
            match paths.len() {
                0 => None,
                // 只删一个时报路径更有用;成堆删时路径无信息量,报条数。
                1 => paths[0].as_str().map(str::to_string),
                count => Some(format!("{count} {}", t("items", "项"))),
            }
        }
        "run_command" => {
            let command = string_arg(&args, &["command"])?;
            Some(
                if args.get("background").and_then(Value::as_bool) == Some(true) {
                    format!("[后台] {command}")
                } else {
                    command
                },
            )
        }
        "read_knowledge_base_file" | "edit_knowledge_base_file" | "remove_knowledge_base_file" => {
            string_arg(&args, &["file_name"])
        }
        "glob" | "grep" => {
            let pattern = string_arg(&args, &["pattern"]);
            let path = string_arg(&args, &["path"]);
            match (pattern, path) {
                (Some(pattern), Some(path)) if !path.trim().is_empty() => {
                    Some(format!("{pattern} · {path}"))
                }
                (pattern, _) => pattern,
            }
        }
        "web_fetch" => string_arg(&args, &["url"]).and_then(|url| safe_url_subject(&url)),
        "load_skill" => string_arg(&args, &["name"]),
        "create_skill" | "update_skill" | "delete_skill" => string_arg(&args, &["name"]),
        "publish_skill" => string_arg(&args, &["draft_id"]),
        "load_tools" => args.get("names").and_then(Value::as_array).map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(|name| {
                    let display = readable_tool_name(&format!("load_tools:{name}"));
                    display
                        .split_once('：')
                        .or_else(|| display.split_once(": "))
                        .map(|(_, target)| target.to_string())
                        .unwrap_or(display)
                })
                .collect::<Vec<_>>()
                .join(t(", ", "、"))
        }),
        "deep_research" => string_arg(&args, &["topic"]),
        "check_issue" => string_arg(&args, &["target", "area", "issue", "symptom"]),
        "get_weather" => string_arg(&args, &["location"])
            .or_else(|| Some(t("automatic location", "自动定位").to_string())),
        "get_exchange_rate" => {
            let base = string_arg(&args, &["base"])?;
            let target = string_arg(&args, &["target"])?;
            Some(format!(
                "{} → {}",
                base.to_uppercase(),
                target.to_uppercase()
            ))
        }
        "scientific_calculator" => string_arg(&args, &["expression", "operation"]),
        "set_alarm" => string_arg(&args, &["label", "time"]),
        "cancel_alarm" => string_arg(&args, &["id"]),
        "aur_get_package_info"
        | "archlinux_official_package_query"
        | "review_aur_package"
        | "install_aur_package" => string_arg(&args, &["package_name", "package"]),
        "online_man_get_page" => {
            let page = string_arg(&args, &["name"])?;
            let section = string_arg(&args, &["section"]);
            Some(section.map_or(page.clone(), |section| format!("{page}({section})")))
        }
        "vision_analyze" | "print_image" | "add_meme" => {
            string_arg(&args, &["image"]).map(|image| image_basename(&image))
        }
        "generate_image" => string_arg(&args, &["prompt"]),
        "upload_text_to_knowledge_base" => string_arg(&args, &["file_name", "title"]),
        "register_deep_research_topic_title" => string_arg(&args, &["topic_title"]),
        "register_deep_research_reference" => string_arg(&args, &["title"]),
        "remove_deep_research_reference" => string_arg(&args, &["ref"]),
        "unregister_script" => string_arg(&args, &["id"]),
        _ => None,
    }?;
    safe_inline_subject(&value)
}

/// Page label for a read_file call: `L<start>-<end>` when the range is
/// bounded, `L<start>+` for an open tail. `None` for a plain full read so
/// the common case stays a bare path.
fn read_page_label(args: &Value) -> Option<String> {
    let offset = args.get("offset").and_then(Value::as_u64);
    let limit = args.get("limit").and_then(Value::as_u64);
    let start = offset.unwrap_or(1).max(1);
    match (offset, limit) {
        (None, None) => None,
        (_, Some(limit)) => Some(format!(
            "L{start}-{}",
            start.saturating_add(limit.saturating_sub(1))
        )),
        (Some(_), None) => Some(format!("L{start}+")),
    }
}

fn string_arg(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn safe_inline_subject(value: &str) -> Option<String> {
    let value = truncate_inline_input(&sanitize_terminal_text(value), 256);
    let value = clip_progress_line(&value, 256);
    let value = redact_sensitive_inline(&value);
    let value = clip_progress_line(&value, 80);
    (!value.is_empty()).then_some(value)
}

fn truncate_inline_input(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn redact_sensitive_inline(value: &str) -> String {
    const KEYS: &[&str] = &[
        "secret_access_key",
        "secret-access-key",
        "access_key_id",
        "access-key-id",
        "api_key",
        "api-key",
        "apikey",
        "token",
        "password",
        "passwd",
        "secret",
        "authorization",
        "cookie",
        "credential",
        "private_key",
        "private-key",
    ];
    let mut output = value.to_string();
    for key in KEYS {
        let mut from = 0usize;
        loop {
            let lower = output.to_ascii_lowercase();
            let Some(relative) = lower[from..].find(key) else {
                break;
            };
            let key_start = from + relative;
            let key_end = key_start + key.len();
            let boundary_ok =
                key_start == 0 || !lower.as_bytes()[key_start - 1].is_ascii_alphanumeric();
            let mut separator = key_end;
            if matches!(lower.as_bytes().get(separator), Some(b'\'' | b'"')) {
                separator += 1;
            }
            let mut had_space = false;
            while lower.as_bytes().get(separator) == Some(&b' ') {
                had_space = true;
                separator += 1;
            }
            let flag_prefix = &lower[..key_start];
            let single_dash_flag = flag_prefix.ends_with('-')
                && (key_start == 1 || lower.as_bytes()[key_start - 2].is_ascii_whitespace());
            let flag_space = had_space && (flag_prefix.ends_with("--") || single_dash_flag);
            let space_delimited = had_space
                && (matches!(*key, "authorization" | "password" | "passwd") || flag_space);
            if !boundary_ok
                || (!space_delimited
                    && !matches!(lower.as_bytes().get(separator), Some(b'=' | b':')))
            {
                from = key_end;
                continue;
            }
            let mut value_start = separator + usize::from(!space_delimited);
            while lower.as_bytes().get(value_start) == Some(&b' ') {
                value_start += 1;
            }
            let quote = lower
                .as_bytes()
                .get(value_start)
                .copied()
                .filter(|value| matches!(value, b'\'' | b'"'));
            value_start += usize::from(quote.is_some());
            let value_end = quote
                .and_then(|quote| {
                    lower.as_bytes()[value_start..]
                        .iter()
                        .position(|value| *value == quote)
                        .map(|end| value_start + end)
                })
                .or_else(|| {
                    flag_space.then(|| {
                        lower.as_bytes()[value_start..]
                            .iter()
                            .position(|byte| byte.is_ascii_whitespace())
                            .map(|end| value_start + end)
                            .unwrap_or(output.len())
                    })
                })
                .or_else(|| {
                    lower[value_start..]
                        .find(['&', ',', ';'])
                        .map(|end| value_start + end)
                })
                .unwrap_or(output.len());
            output.replace_range(value_start..value_end, "[redacted]");
            from = value_start + "[redacted]".len();
        }
    }
    redact_bearer_token(output)
}

fn redact_bearer_token(mut output: String) -> String {
    let mut from = 0usize;
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(relative) = lower[from..].find("bearer") else {
            break;
        };
        let start = from + relative;
        let end = start + "bearer".len();
        let boundary_ok = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
        let mut value_start = end;
        while lower.as_bytes().get(value_start) == Some(&b' ') {
            value_start += 1;
        }
        if !boundary_ok || value_start == end || value_start == output.len() {
            from = end;
            continue;
        }
        let value_end = lower.as_bytes()[value_start..]
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || matches!(*byte, b',' | b';' | b'&'))
            .map(|relative| value_start + relative)
            .unwrap_or(output.len());
        output.replace_range(value_start..value_end, "[redacted]");
        from = value_start + "[redacted]".len();
    }
    output
}

fn safe_url_subject(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value).ok()?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

fn image_basename(value: &str) -> String {
    if let Some(url) = safe_url_subject(value) {
        return url;
    }
    std::path::Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value)
        .to_string()
}

fn readable_tool_name(name: &str) -> String {
    crate::tools::readable_tool_name(name)
}

struct MarkdownStreamRenderer {
    buffer: String,
    line_renderer: MarkdownLineRenderer,
}

impl MarkdownStreamRenderer {
    fn new() -> Self {
        Self {
            buffer: String::new(),
            line_renderer: MarkdownLineRenderer::new(),
        }
    }

    fn push(&mut self, delta: &str) -> String {
        self.buffer.push_str(delta);
        let mut output = String::new();
        while let Some(index) = self.buffer.find('\n') {
            let line = self.buffer[..index].to_string();
            self.buffer = self.buffer[index + 1..].to_string();
            output.push_str(&self.line_renderer.render_line(&line));
        }
        output
    }

    fn flush(&mut self) -> String {
        let mut output = String::new();
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            output.push_str(&self.line_renderer.render_line(&line));
        }
        output.push_str(&self.line_renderer.flush());
        output
    }
}

struct MarkdownLineRenderer {
    in_code_block: bool,
    in_math_block: bool,
    code_lang: String,
    code_buffer: Vec<String>,
    table_buffer: Vec<String>,
    active_table: Option<ActiveTable>,
    math_buffer: Vec<String>,
    /// 当前块级公式的闭合定界符("$$" 或 "\\]")。
    math_closer: &'static str,
}

struct ActiveTable {
    widths: Vec<usize>,
    alignments: Vec<TableAlign>,
}

impl MarkdownLineRenderer {
    fn new() -> Self {
        Self {
            in_code_block: false,
            in_math_block: false,
            code_lang: String::new(),
            code_buffer: Vec::new(),
            table_buffer: Vec::new(),
            active_table: None,
            math_buffer: Vec::new(),
            math_closer: "$$",
        }
    }

    fn render_line(&mut self, line: &str) -> String {
        if line.trim_start().starts_with("```") {
            if self.in_code_block {
                self.in_code_block = false;
                let code = render_code_block(&self.code_lang, &self.code_buffer);
                self.code_lang.clear();
                self.code_buffer.clear();
                return code;
            }
            let pending = self.flush();
            self.in_code_block = true;
            self.code_lang = line
                .trim_start()
                .trim_start_matches('`')
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            self.code_buffer.clear();
            return pending;
        }
        if self.in_code_block {
            self.code_buffer.push(line.to_string());
            return String::new();
        }
        if self.in_math_block {
            let trimmed = line.trim();
            if trimmed == self.math_closer || trimmed.ends_with(self.math_closer) {
                if trimmed != self.math_closer {
                    self.math_buffer
                        .push(trimmed[..trimmed.len() - self.math_closer.len()].to_string());
                }
                self.in_math_block = false;
                let tex = std::mem::take(&mut self.math_buffer).join("\n");
                return render_display_math(&tex, self.math_closer);
            }
            self.math_buffer.push(line.to_string());
            return String::new();
        }
        {
            let trimmed = line.trim();
            let opener = if trimmed.starts_with("$$") {
                Some(("$$", "$$"))
            } else if trimmed.starts_with("\\[") {
                Some(("\\[", "\\]"))
            } else {
                None
            };
            if let Some((open, close)) = opener {
                let pending = self.flush();
                let inner = &trimmed[open.len()..];
                // 单行闭合:$$E=mc^2$$ / \[x\]
                if let Some(tex) = inner.strip_suffix(close) {
                    if !tex.trim().is_empty() {
                        return format!("{pending}{}", render_display_math(tex, close));
                    }
                }
                self.in_math_block = true;
                self.math_closer = close;
                self.math_buffer.clear();
                if !inner.trim().is_empty() {
                    self.math_buffer.push(inner.to_string());
                }
                return pending;
            }
        }
        if let Some(table) = &self.active_table {
            if looks_like_table_row(line) {
                let row = parse_table_row(line);
                let mut output = middle_table_border(&table.widths);
                output.push_str(&render_table_row(
                    &row,
                    &table.widths,
                    &table.alignments,
                    false,
                ));
                return output;
            }
            let mut output = bottom_table_border(&table.widths);
            self.active_table = None;
            output.push_str(&self.render_line(line));
            return output;
        }
        if looks_like_table_row(line) {
            self.table_buffer.push(line.to_string());
            if self.table_buffer.len() < 3 {
                return String::new();
            }
            let second = self.table_buffer.get(1).cloned().unwrap_or_default();
            if is_table_separator(&second) {
                let header =
                    parse_table_row(self.table_buffer.first().map(String::as_str).unwrap_or(""));
                let alignments = parse_table_alignments(&second);
                let first_row =
                    parse_table_row(self.table_buffer.get(2).map(String::as_str).unwrap_or(""));
                let widths = table_widths_for_rows(&[header.clone(), first_row.clone()]);
                self.table_buffer.clear();
                self.active_table = Some(ActiveTable {
                    widths: widths.clone(),
                    alignments: alignments.clone(),
                });
                let mut output = top_table_border(&widths);
                output.push_str(&render_table_row(&header, &widths, &alignments, true));
                output.push_str(&middle_table_border(&widths));
                output.push_str(&render_table_row(&first_row, &widths, &alignments, false));
                return output;
            }
            return self.flush();
        }
        let mut output = self.flush();
        output.push_str(&render_markdown_line(line));
        output.push('\n');
        output
    }

    fn flush(&mut self) -> String {
        if self.in_math_block {
            // 流结束仍未闭合:按原样回放,不吞内容。
            self.in_math_block = false;
            let opener = if self.math_closer == "$$" { "$$" } else { "\\[" };
            let mut output = format!("\x1b[36m{opener}\x1b[0m\n");
            for line in std::mem::take(&mut self.math_buffer) {
                output.push_str(&format!("\x1b[36m{line}\x1b[0m\n"));
            }
            return output;
        }
        if self.in_code_block {
            self.in_code_block = false;
            let output = render_code_block(&self.code_lang, &self.code_buffer);
            self.code_lang.clear();
            self.code_buffer.clear();
            return output;
        }
        if let Some(table) = self.active_table.take() {
            return bottom_table_border(&table.widths);
        }
        if self.table_buffer.is_empty() {
            return String::new();
        }
        let lines = std::mem::take(&mut self.table_buffer);
        if lines.len() >= 2 && is_table_separator(lines.get(1).map(String::as_str).unwrap_or("")) {
            render_table(&lines)
        } else {
            let mut output = String::new();
            for line in lines {
                output.push_str(&render_markdown_line(&line));
                output.push('\n');
            }
            output
        }
    }
}

/// 块级公式:kitty 家族终端走图形协议(高清,复用 print_image 管线),
/// 其余终端半块画;渲染失败原样回放(青色+定界符)。
fn render_display_math(tex: &str, closer: &str) -> String {
    let max_cols = terminal::size()
        .map(|(cols, _)| cols as usize)
        .unwrap_or(100)
        .saturating_sub(6)
        .clamp(24, 110);
    if math::kitty_graphics_supported() {
        if let Some(kitty) = math::render_math_kitty(tex, max_cols) {
            // 占位行自带换行,逐行加两格缩进(图形转义段无换行,不受影响);
            // 首尾补空行,与正文拉开呼吸感。
            let mut output = String::from("\n");
            for line in kitty.sequence.split_inclusive('\n') {
                output.push_str("  ");
                output.push_str(line);
            }
            output.push('\n');
            return output;
        }
    }
    if let Some(art) = math::render_math(tex, math::MathMode::Block, 9, max_cols) {
        let mut output = String::from("\n");
        for line in art.lines {
            output.push_str("  ");
            output.push_str(&line);
            output.push('\n');
        }
        output.push('\n');
        return output;
    }
    let opener = if closer == "$$" { "$$" } else { "\\[" };
    let closing = if closer == "$$" { "$$" } else { "\\]" };
    let mut output = format!("\x1b[36m{opener}\x1b[0m\n");
    for line in tex.lines() {
        output.push_str(&format!("\x1b[36m{line}\x1b[0m\n"));
    }
    output.push_str(&format!("\x1b[36m{closing}\x1b[0m\n"));
    output
}

pub(crate) fn render_markdown_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    if let Some(header) = render_header(trimmed) {
        return header;
    }
    if let Some((depth, rest)) = parse_blockquote(trimmed) {
        let bars = "\x1b[32m| \x1b[0m".repeat(depth);
        return format!("{indent}{bars}\x1b[32m{}\x1b[0m", render_inline(rest));
    }
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        return format!("{indent}{TERTIARY_STYLE}-{RESET} {}", render_inline(rest));
    }
    let digits = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits > 0
        && trimmed.as_bytes().get(digits) == Some(&b'.')
        && trimmed.as_bytes().get(digits + 1) == Some(&b' ')
    {
        let marker = &trimmed[..=digits];
        let rest = &trimmed[digits + 2..];
        return format!(
            "{indent}{TERTIARY_STYLE}{marker}{RESET} {}",
            render_inline(rest)
        );
    }
    if is_horizontal_rule(trimmed) {
        return horizontal_rule();
    }
    render_inline(line)
}

fn parse_blockquote(line: &str) -> Option<(usize, &str)> {
    let mut depth = 0;
    let mut rest = line;
    while let Some(stripped) = rest.strip_prefix('>') {
        depth += 1;
        rest = stripped.strip_prefix(' ').unwrap_or(stripped);
    }
    (depth > 0).then_some((depth, rest))
}

fn render_header(line: &str) -> Option<String> {
    let level = line.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || level > 6 || line.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    let prefix = "#".repeat(level);
    Some(format!(
        "{HEADER_STYLE}{prefix} {}{RESET}",
        render_inline(&line[level + 1..])
    ))
}

fn render_inline(text: &str) -> String {
    let mut output = String::new();
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        // 行内公式 $…$ / $$…$$:Unicode 转写(xₙ₊₁、√π、α∈(0,1))。
        // 单 $ 启发式同 WebUI:内容非空、两端非空格、右侧不接数字(放过价格)。
        if chars[index] == '$' {
            let double = chars.get(index + 1) == Some(&'$');
            let open = if double { index + 2 } else { index + 1 };
            let close = if double {
                find_double_dollar(&chars, open)
            } else {
                find_marker(&chars, open, '$')
            };
            if let Some(end) = close {
                let tex: String = chars[open..end].iter().collect();
                let accept = !tex.trim().is_empty()
                    && (double
                        || (!tex.starts_with(' ')
                            && !tex.ends_with(' ')
                            && !chars
                                .get(end + 1)
                                .is_some_and(|next| next.is_ascii_digit())));
                if accept {
                    output.push_str(PRIMARY_STYLE);
                    output.push_str(&math::unicode_math(&tex));
                    output.push_str(RESET);
                    index = end + if double { 2 } else { 1 };
                    continue;
                }
            }
        }
        if chars[index] == '\\' && chars.get(index + 1) == Some(&'(') {
            let mut probe = index + 2;
            let mut closing = None;
            while probe + 1 < chars.len() {
                if chars[probe] == '\\' && chars[probe + 1] == ')' {
                    closing = Some(probe);
                    break;
                }
                probe += 1;
            }
            if let Some(end) = closing {
                let tex: String = chars[index + 2..end].iter().collect();
                output.push_str(PRIMARY_STYLE);
                output.push_str(&math::unicode_math(&tex));
                output.push_str(RESET);
                index = end + 2;
                continue;
            }
        }
        if index + 1 < chars.len() && chars[index] == '!' && chars[index + 1] == '[' {
            if let Some(label_end) = find_marker(&chars, index + 2, ']') {
                if chars.get(label_end + 1) == Some(&'(') {
                    if let Some(url_end) = find_marker(&chars, label_end + 2, ')') {
                        let alt = chars[index + 2..label_end].iter().collect::<String>();
                        output.push_str(IMAGE_STYLE);
                        output.push_str("[image");
                        if !alt.is_empty() {
                            output.push_str(": ");
                            output.push_str(&alt);
                        }
                        output.push_str("]");
                        output.push_str(RESET);
                        output.push('(');
                        output.push_str(&render_url(
                            &chars[label_end + 2..url_end].iter().collect::<String>(),
                        ));
                        output.push(')');
                        index = url_end + 1;
                        continue;
                    }
                }
            }
        }
        if chars[index] == '`' {
            if let Some(end) = find_marker(&chars, index + 1, '`') {
                output.push_str(INLINE_CODE_STYLE);
                output.extend(chars[index + 1..end].iter());
                output.push_str(RESET);
                index = end + 1;
                continue;
            }
        }
        if index + 1 < chars.len() && chars[index] == '~' && chars[index + 1] == '~' {
            if let Some(end) = find_double_marker(&chars, index + 2, '~') {
                output.push_str(STRIKE_STYLE);
                output.extend(chars[index + 2..end].iter());
                output.push_str(RESET);
                index = end + 2;
                continue;
            }
        }
        if index + 1 < chars.len() && chars[index] == '*' && chars[index + 1] == '*' {
            if let Some(end) = find_double_marker(&chars, index + 2, '*') {
                output.push_str(BOLD_STYLE);
                output.extend(chars[index + 2..end].iter());
                output.push_str(RESET);
                index = end + 2;
                continue;
            }
        }
        if chars[index] == '*' {
            if let Some(end) = find_marker(&chars, index + 1, '*') {
                output.push_str(ITALIC_STYLE);
                output.extend(chars[index + 1..end].iter());
                output.push_str(RESET);
                index = end + 1;
                continue;
            }
        }
        if chars[index] == '_' {
            if is_emphasis_start(&chars, index) {
                if let Some(end) = find_emphasis_end(&chars, index + 1, '_') {
                    output.push_str(ITALIC_STYLE);
                    output.extend(chars[index + 1..end].iter());
                    output.push_str(RESET);
                    index = end + 1;
                    continue;
                }
            }
        }
        if chars[index] == '[' {
            if let Some(label_end) = find_marker(&chars, index + 1, ']') {
                if chars.get(label_end + 1) == Some(&'(') {
                    if let Some(url_end) = find_marker(&chars, label_end + 2, ')') {
                        output.push_str(LINK_LABEL_STYLE);
                        output.extend(chars[index + 1..label_end].iter());
                        output.push_str(RESET);
                        output.push(' ');
                        output.push_str(&render_url_wrapped(
                            &chars[label_end + 2..url_end].iter().collect::<String>(),
                        ));
                        index = url_end + 1;
                        continue;
                    }
                }
            }
        }
        if chars[index] == '<' {
            if let Some(end) = find_marker(&chars, index + 1, '>') {
                let value = chars[index + 1..end].iter().collect::<String>();
                if value.starts_with("http://") || value.starts_with("https://") {
                    output.push_str("\x1b[4m");
                    output.push_str(&render_url_wrapped(&value));
                    output.push_str(RESET);
                    index = end + 1;
                    continue;
                }
                if let Some(rendered) = render_html_tag(&value) {
                    output.push_str(&rendered);
                    index = end + 1;
                    continue;
                }
            }
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

const RESET: &str = "\x1b[0m";
const PRIMARY_STYLE: &str = "\x1b[38;5;189m";
const SECONDARY_STYLE: &str = "\x1b[36m";
const TERTIARY_STYLE: &str = "\x1b[35m";
const HEADER_STYLE: &str = "\x1b[1m\x1b[35m";
const INLINE_CODE_STYLE: &str = SECONDARY_STYLE;
const LINK_LABEL_STYLE: &str = "\x1b[38;5;117m";
const URL_STYLE: &str = "\x1b[2m\x1b[38;5;75m";
const IMAGE_STYLE: &str = "\x1b[38;5;183m";

const BOLD_STYLE: &str = "\x1b[1m\x1b[34m";
const ITALIC_STYLE: &str = "\x1b[3m\x1b[38;5;250m";
const STRIKE_STYLE: &str = "\x1b[9m";
const CODE_BLOCK_BG: &str = "";
const CODE_BLOCK_FRAME_STYLE: &str = SECONDARY_STYLE;
const CODE_TOKEN_RESET: &str = "\x1b[0m";
const CODE_KEYWORD_STYLE: &str = "\x1b[38;2;196;167;231m";
const CODE_FUNCTION_STYLE: &str = "\x1b[38;2;156;207;216m";
const CODE_STRING_STYLE: &str = "\x1b[38;2;166;214;160m";
const CODE_NUMBER_STYLE: &str = "\x1b[38;2;246;193;119m";
const CODE_COMMENT_STYLE: &str = "\x1b[32m";
const PATCH_DELETE_STYLE: &str = "\x1b[48;2;60;41;53m\x1b[38;5;210m";
const PATCH_INSERT_STYLE: &str = "\x1b[48;2;32;52;67m\x1b[38;5;157m";

fn render_url(url: &str) -> String {
    format!("{URL_STYLE}{url}{RESET}")
}

fn render_url_wrapped(url: &str) -> String {
    format!("<{}>", render_url(url))
}

fn render_html_tag(tag: &str) -> Option<String> {
    match tag.trim().to_ascii_lowercase().as_str() {
        "u" => Some("\x1b[4m".to_string()),
        "/u" => Some("\x1b[0m".to_string()),
        "sub" => Some("\x1b[2m".to_string()),
        "/sub" => Some("\x1b[0m".to_string()),
        "sup" => Some("\x1b[1m".to_string()),
        "/sup" => Some("\x1b[0m".to_string()),
        "br" | "br/" | "br /" => Some("\n".to_string()),
        _ => None,
    }
}

fn horizontal_rule() -> String {
    let width = terminal::size()
        .map(|(width, _)| usize::from(width) / 3)
        .unwrap_or(24)
        .clamp(16, 40);
    format!("\x1b[2m{}\x1b[0m", "─".repeat(width))
}

fn render_table(lines: &[String]) -> String {
    render_table_with_header_style(lines, true)
}

fn render_table_with_header_style(lines: &[String], bold_header: bool) -> String {
    let alignments = lines
        .get(1)
        .filter(|line| is_table_separator(line))
        .map(|line| parse_table_alignments(line))
        .unwrap_or_default();
    let rows = lines
        .iter()
        .filter(|line| !is_table_separator(line))
        .map(|line| {
            line.trim()
                .trim_matches('|')
                .split('|')
                .map(|cell| render_inline(cell.trim()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let widths = table_widths_for_rows(&rows);
    let mut output = String::new();
    output.push_str(&top_table_border(&widths));
    for (row_index, row) in rows.iter().enumerate() {
        output.push_str(&render_table_row(
            row,
            &widths,
            &alignments,
            bold_header && row_index == 0,
        ));
        if row_index + 1 < rows.len() {
            output.push_str(&middle_table_border(&widths));
        }
    }
    output.push_str(&bottom_table_border(&widths));
    output
}

fn parse_table_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| render_table_cell(cell.trim()))
        .collect()
}

/// 表格单元格:整格为一条公式($…$ / \(…\) 完整包裹)时走二维转写,
/// 分式排成真正的上下结构(多行格);其余走常规行内渲染。
fn render_table_cell(cell: &str) -> String {
    let tex = cell
        .strip_prefix('$')
        .and_then(|rest| rest.strip_suffix('$'))
        .filter(|inner| !inner.is_empty() && !inner.contains('$'))
        .or_else(|| {
            cell.strip_prefix("\\(")
                .and_then(|rest| rest.strip_suffix("\\)"))
        });
    if let Some(tex) = tex {
        if !tex.trim().is_empty() {
            let lines = math::unicode_math_lines(tex);
            let styled = lines
                .iter()
                .map(|line| format!("{PRIMARY_STYLE}{line}{RESET}"))
                .collect::<Vec<_>>();
            return styled.join("\n");
        }
    }
    render_inline(cell)
}

fn table_widths_for_rows(rows: &[Vec<String>]) -> Vec<usize> {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            // 多行格(二维公式)取最宽一行。
            let cell_width = cell.split('\n').map(visible_width).max().unwrap_or(0);
            widths[index] = widths[index].max(cell_width);
        }
    }
    let readable_min = readable_table_min_width(cols);
    for width in &mut widths {
        *width = (*width).max(readable_min);
    }
    bounded_table_widths(widths)
}

fn readable_table_min_width(cols: usize) -> usize {
    match cols {
        0 => 0,
        1 => 16,
        2 => 14,
        3 | 4 => 10,
        _ => 8,
    }
}

fn render_table_row(
    row: &[String],
    widths: &[usize],
    alignments: &[TableAlign],
    header: bool,
) -> String {
    let wrapped = widths
        .iter()
        .enumerate()
        .map(|(index, width)| {
            let cell = row.get(index).map(String::as_str).unwrap_or("");
            cell.split('\n')
                .flat_map(|part| wrap_ansi_text(part, *width))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let row_height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    let mut output = String::new();
    for line_index in 0..row_height {
        push_table_vertical(&mut output);
        for (index, width) in widths.iter().enumerate() {
            let cell = wrapped
                .get(index)
                .and_then(|lines| lines.get(line_index))
                .map(String::as_str)
                .unwrap_or("");
            let cell = if header && !cell.is_empty() {
                format!("{BOLD_STYLE}{cell}{RESET}")
            } else {
                cell.to_string()
            };
            output.push(' ');
            output.push_str(&aligned_cell(
                &cell,
                *width,
                alignments.get(index).copied().unwrap_or(TableAlign::Left),
            ));
            output.push(' ');
            push_table_vertical(&mut output);
        }
        output.push('\n');
    }
    output
}

fn top_table_border(widths: &[usize]) -> String {
    table_border(widths, '┌', '┬', '┐')
}

fn middle_table_border(widths: &[usize]) -> String {
    table_border(widths, '├', '┼', '┤')
}

fn bottom_table_border(widths: &[usize]) -> String {
    table_border(widths, '└', '┴', '┘')
}

fn bounded_table_widths(mut widths: Vec<usize>) -> Vec<usize> {
    if widths.is_empty() {
        return widths;
    }
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100)
        .saturating_sub(1)
        .max(20);
    let border_overhead = widths.len().saturating_mul(3).saturating_add(1);
    let available = terminal_width
        .saturating_sub(border_overhead)
        .max(widths.len());
    while widths.iter().sum::<usize>() > available {
        let Some((index, width)) = widths
            .iter()
            .enumerate()
            .max_by_key(|(_, width)| **width)
            .map(|(index, width)| (index, *width))
        else {
            break;
        };
        if width <= 1 {
            break;
        }
        widths[index] -= 1;
    }
    widths
}

fn wrap_ansi_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            current.push(ch);
            for next in chars.by_ref() {
                current.push(next);
                if next == 'm' {
                    break;
                }
            }
            continue;
        }
        let ch_width = char_display_width(ch);
        if current_width > 0 && current_width + ch_width > width {
            lines.push(current);
            current = String::new();
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }
    lines.push(current);
    lines
}

fn char_display_width(ch: char) -> usize {
    if ch.is_ascii() {
        1
    } else if (ch as u32) >= 0x2e80 {
        2
    } else {
        1
    }
}

#[derive(Clone, Copy)]
enum TableAlign {
    Left,
    Center,
    Right,
}

fn parse_table_alignments(line: &str) -> Vec<TableAlign> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| {
            let cell = cell.trim();
            match (cell.starts_with(':'), cell.ends_with(':')) {
                (true, true) => TableAlign::Center,
                (false, true) => TableAlign::Right,
                _ => TableAlign::Left,
            }
        })
        .collect()
}

fn aligned_cell(cell: &str, width: usize, align: TableAlign) -> String {
    let padding = width.saturating_sub(visible_width(cell));
    match align {
        TableAlign::Left => format!("{cell}{}", " ".repeat(padding)),
        TableAlign::Right => format!("{}{cell}", " ".repeat(padding)),
        TableAlign::Center => {
            let left = padding / 2;
            let right = padding - left;
            format!("{}{cell}{}", " ".repeat(left), " ".repeat(right))
        }
    }
}

fn table_border(widths: &[usize], left: char, mid: char, right: char) -> String {
    let mut output = String::new();
    output.push_str("\x1b[2m");
    output.push(left);
    for (index, width) in widths.iter().enumerate() {
        output.push_str(&"─".repeat(width + 2));
        output.push(if index + 1 == widths.len() {
            right
        } else {
            mid
        });
    }
    output.push_str("\x1b[0m\n");
    output
}

fn push_table_vertical(output: &mut String) {
    output.push_str("\x1b[2m│\x1b[0m");
}

fn highlight_code_line(lang: &str, line: &str) -> String {
    let lang = lang.trim().to_ascii_lowercase();
    if lang.is_empty() {
        return line.to_string();
    }
    let comment_marker = match lang.as_str() {
        "py" | "python" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yaml" | "yml" => Some('#'),
        "rs" | "rust" | "js" | "ts" | "tsx" | "jsx" | "c" | "cpp" | "java" | "go" => None,
        _ => None,
    };
    let mut output = String::new();
    let chars = line.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        if let Some(marker) = comment_marker {
            if chars[index] == marker {
                output.push_str(CODE_COMMENT_STYLE);
                output.extend(chars[index..].iter());
                output.push_str(CODE_TOKEN_RESET);
                return output;
            }
        }
        if index + 1 < chars.len() && chars[index] == '/' && chars[index + 1] == '/' {
            output.push_str(CODE_COMMENT_STYLE);
            output.extend(chars[index..].iter());
            output.push_str(CODE_TOKEN_RESET);
            return output;
        }
        if chars[index] == '"'
            || chars[index] == '\''
            || (chars[index] == '`'
                && matches!(lang.as_str(), "js" | "ts" | "tsx" | "jsx" | "sh" | "bash"))
        {
            let quote = chars[index];
            let start = index;
            index += 1;
            let mut escaped = false;
            while index < chars.len() {
                if escaped {
                    escaped = false;
                } else if chars[index] == '\\' {
                    escaped = true;
                } else if chars[index] == quote {
                    index += 1;
                    break;
                }
                index += 1;
            }
            output.push_str(CODE_STRING_STYLE);
            output.extend(chars[start..index].iter());
            output.push_str(CODE_TOKEN_RESET);
            continue;
        }
        if chars[index].is_ascii_digit() {
            let start = index;
            index += 1;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric() || matches!(chars[index], '_' | '.'))
            {
                index += 1;
            }
            output.push_str(CODE_NUMBER_STYLE);
            output.extend(chars[start..index].iter());
            output.push_str(CODE_TOKEN_RESET);
            continue;
        }
        if is_code_word_start(chars[index]) {
            let start = index;
            index += 1;
            while index < chars.len() && is_code_word_char(chars[index]) {
                index += 1;
            }
            let token = chars[start..index].iter().collect::<String>();
            let style = if code_keywords(&lang).contains(&token.as_str()) {
                Some(CODE_KEYWORD_STYLE)
            } else if matches!(
                token.as_str(),
                "true" | "false" | "null" | "None" | "Some" | "Ok" | "Err"
            ) {
                Some(CODE_NUMBER_STYLE)
            } else if next_non_space_is_open_paren(&chars, index) {
                Some(CODE_FUNCTION_STYLE)
            } else {
                None
            };
            if let Some(style) = style {
                output.push_str(style);
                output.push_str(&token);
                output.push_str(CODE_TOKEN_RESET);
            } else {
                output.push_str(PRIMARY_STYLE);
                output.push_str(&token);
                output.push_str(CODE_TOKEN_RESET);
            }
            continue;
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

fn render_code_block(lang: &str, lines: &[String]) -> String {
    let label = if lang.is_empty() {
        "code".to_string()
    } else {
        format!("code {lang}")
    };
    let header = format!("-- {label}");
    let footer = "--";
    let width = lines
        .iter()
        .map(|line| line.chars().count())
        .chain([header.chars().count(), footer.chars().count()])
        .max()
        .unwrap_or(footer.len())
        .max(24);
    let mut output = String::new();
    output.push_str(&render_code_block_frame(&header, width));
    output.push('\n');
    for line in lines {
        output.push_str(&render_code_block_line_with_width(lang, line, width));
        output.push('\n');
    }
    output.push_str(&render_code_block_frame(footer, width));
    output.push('\n');
    output
}

fn render_code_block_frame(text: &str, width: usize) -> String {
    if text == "--" {
        return format!("{CODE_BLOCK_FRAME_STYLE}{}{RESET}", "─".repeat(width));
    }
    let label = text.strip_prefix("-- ").unwrap_or(text);
    let prefix = format!("╭─ {label} ");
    format!(
        "{CODE_BLOCK_FRAME_STYLE}{prefix}{}{RESET}",
        "─".repeat(width.saturating_sub(prefix.chars().count()))
    )
}

fn render_code_block_line_with_width(lang: &str, line: &str, width: usize) -> String {
    let line_width = line.chars().count();
    let padding = " ".repeat(width.saturating_sub(line_width));
    let highlighted = highlight_code_line(lang, line);
    if highlighted.is_empty() {
        format!("{CODE_BLOCK_BG}{}{RESET}", " ".repeat(width.max(1)))
    } else {
        format!("{CODE_BLOCK_BG}{highlighted}{padding}{RESET}")
    }
}

fn code_keywords(lang: &str) -> &'static [&'static str] {
    match lang {
        "rs" | "rust" => &[
            "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "fn",
            "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
            "return", "self", "Self", "static", "struct", "trait", "type", "unsafe", "use",
            "where", "while",
        ],
        "py" | "python" => &[
            "and", "as", "async", "await", "break", "class", "continue", "def", "elif", "else",
            "except", "finally", "for", "from", "if", "import", "in", "is", "lambda", "not", "or",
            "pass", "raise", "return", "try", "while", "with", "yield",
        ],
        "js" | "ts" | "tsx" | "jsx" => &[
            "async", "await", "break", "case", "catch", "class", "const", "continue", "default",
            "else", "export", "extends", "finally", "for", "from", "function", "if", "import",
            "let", "new", "return", "switch", "throw", "try", "typeof", "var", "while",
        ],
        "sh" | "bash" | "zsh" | "fish" => &[
            "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in",
            "then", "while",
        ],
        "json" | "toml" | "yaml" | "yml" => &["true", "false", "null"],
        _ => &[],
    }
}

fn is_code_word_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_code_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn next_non_space_is_open_paren(chars: &[char], mut index: usize) -> bool {
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    chars.get(index) == Some(&'(')
}

fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim().trim_matches('|').trim();
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|ch| matches!(ch, '-' | ':' | '|' | ' '))
        && trimmed.contains('-')
}

fn looks_like_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 2
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3 && trimmed.chars().all(|ch| ch == '-')
}

fn find_marker(chars: &[char], start: usize, marker: char) -> Option<usize> {
    (start..chars.len()).find(|index| chars[*index] == marker)
}

fn find_double_dollar(chars: &[char], start: usize) -> Option<usize> {
    (start..chars.len().saturating_sub(1))
        .find(|index| chars[*index] == '$' && chars[*index + 1] == '$')
}

fn find_emphasis_end(chars: &[char], start: usize, marker: char) -> Option<usize> {
    (start..chars.len()).find(|index| chars[*index] == marker && is_emphasis_end(chars, *index))
}

fn is_emphasis_start(chars: &[char], index: usize) -> bool {
    !chars
        .get(index.wrapping_sub(1))
        .is_some_and(|ch| is_word_char(*ch))
        && chars
            .get(index + 1)
            .is_some_and(|ch| !ch.is_whitespace() && *ch != '_')
}

fn is_emphasis_end(chars: &[char], index: usize) -> bool {
    chars
        .get(index.wrapping_sub(1))
        .is_some_and(|ch| !ch.is_whitespace() && *ch != '_')
        && !chars.get(index + 1).is_some_and(|ch| is_word_char(*ch))
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
}

fn find_double_marker(chars: &[char], start: usize, marker: char) -> Option<usize> {
    (start..chars.len().saturating_sub(1))
        .find(|index| chars[*index] == marker && chars[index + 1] == marker)
}

fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut escape = false;
    for ch in text.chars() {
        if ch == '\x1b' {
            escape = true;
        } else if escape {
            if ch == 'm' {
                escape = false;
            }
        } else {
            width += char_display_width(ch);
        }
    }
    width
}

fn write_tool_payload(stdout: &mut impl Write, label: &str, payload: &str) -> Result<()> {
    let formatted = format_tool_payload(payload);
    writeln!(stdout, "\x1b[2m{label}:\x1b[0m")?;
    for line in formatted.lines() {
        writeln!(stdout, "\x1b[2m  {line}\x1b[0m")?;
    }
    Ok(())
}

fn write_patch_result(stdout: &mut impl Write, output: &str) -> Result<bool> {
    let Ok(value) = serde_json::from_str::<Value>(output.trim()) else {
        return Ok(false);
    };
    let path = value.get("path").and_then(Value::as_str).unwrap_or("file");
    let diff = value.get("diff").and_then(Value::as_str).unwrap_or("");
    if diff.trim().is_empty() {
        return Ok(false);
    }
    write!(stdout, "{}", render_patch_diff(path, diff))?;
    Ok(true)
}

fn render_patch_diff(path: &str, diff: &str) -> String {
    let mut output = String::new();
    // apply_patch 是唯一编辑器(增/改/删同一语义),标签按 diff 形态区分:
    // 纯 + 无上下文=新建,纯 - 无上下文=删除,其余=修改。
    let mut plus = false;
    let mut minus = false;
    let mut context = false;
    for line in diff.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") || line.starts_with("@@") {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => plus = true,
            Some(b'-') => minus = true,
            Some(_) => context = true,
            None => {}
        }
    }
    let label = if plus && !minus && !context {
        t("Created", "已新建")
    } else if minus && !plus && !context {
        t("Deleted", "已删除")
    } else {
        t("Modified", "已修改")
    };
    output.push_str(&format!(
        "\x1b[2m{label}  \x1b[38;5;250m{path}\x1b[0m\n\n"
    ));

    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);

    let mut old_line = 0usize;
    let mut new_line = 0usize;
    for raw_line in diff.lines() {
        if raw_line.starts_with("--- ") || raw_line.starts_with("+++ ") {
            continue;
        }
        if raw_line.starts_with("@@") {
            if let Some((old_start, new_start)) = parse_diff_hunk_header(raw_line) {
                old_line = old_start;
                new_line = new_start;
            }
            if !output.ends_with("\n\n") {
                output.push('\n');
            }
            continue;
        }

        let (line_no, sign, body, style) = if let Some(body) = raw_line.strip_prefix('-') {
            let line_no = old_line;
            old_line += 1;
            (line_no, '-', body, PATCH_DELETE_STYLE)
        } else if let Some(body) = raw_line.strip_prefix('+') {
            let line_no = new_line;
            new_line += 1;
            (line_no, '+', body, PATCH_INSERT_STYLE)
        } else if let Some(body) = raw_line.strip_prefix(' ') {
            let line_no = new_line;
            old_line += 1;
            new_line += 1;
            (line_no, ' ', body, "\x1b[38;5;245m")
        } else {
            (new_line, ' ', raw_line, "\x1b[38;5;245m")
        };

        push_patch_diff_line(&mut output, line_no, sign, body, style, terminal_width);
    }
    output.push('\n');
    output
}

fn push_patch_diff_line(
    output: &mut String,
    line_no: usize,
    sign: char,
    body: &str,
    style: &str,
    terminal_width: usize,
) {
    let first_prefix = format!("\x1b[38;5;102m{line_no:>5}\x1b[0m {style}{sign} │ ");
    let continuation_prefix = format!("\x1b[38;5;102m     \x1b[0m {style}  │ ");
    let prefix_width = visible_width(&first_prefix);
    let body_width = terminal_width.saturating_sub(prefix_width + 1).max(1);
    let wrapped = wrap_ansi_text(body, body_width);

    for (index, segment) in wrapped.iter().enumerate() {
        if index == 0 {
            output.push_str(&first_prefix);
        } else {
            output.push_str(&continuation_prefix);
        }
        output.push_str(segment);
        output.push_str("\x1b[0m\n");
    }
}

fn parse_diff_hunk_header(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.split_whitespace();
    parts.next()?;
    let old_part = parts.next()?.trim_start_matches('-');
    let new_part = parts.next()?.trim_start_matches('+');
    Some((
        parse_diff_range_start(old_part)?,
        parse_diff_range_start(new_part)?,
    ))
}

fn parse_diff_range_start(value: &str) -> Option<usize> {
    value.split(',').next()?.parse().ok()
}

fn write_todo_table(stdout: &mut impl Write, output: &str) -> Result<bool> {
    let Ok(value) = serde_json::from_str::<Value>(output.trim()) else {
        return Ok(false);
    };
    let Some(todos) = value.get("todos").and_then(Value::as_array) else {
        return Ok(false);
    };

    if todos.is_empty() {
        let lines = vec![
            format!("| {} |", t("Todo List", "任务列表")),
            "|---|".to_string(),
            format!("| {} |", t("empty", "空")),
        ];
        write!(stdout, "{}", render_todo_table(&lines))?;
        return Ok(true);
    }

    let mut lines = vec![
        format!("| {} |", t("Todo List", "任务列表")),
        "|---|".to_string(),
    ];
    for item in todos {
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        let content = item.get("content").and_then(Value::as_str).unwrap_or("");
        let cell = escape_table_cell(content);
        let cell = if status == "in_progress" {
            format!("{TERTIARY_STYLE}{cell}{RESET}")
        } else {
            cell
        };
        lines.push(format!("| {} {} |", todo_status_marker(status), cell));
    }
    write!(stdout, "{}", render_todo_table(&lines))?;
    Ok(true)
}

fn render_todo_table(lines: &[String]) -> String {
    render_table_with_header_style(lines, false)
}

fn todo_status_marker(status: &str) -> &'static str {
    match status {
        "completed" => "[✔]",
        "in_progress" => "[·]",
        "cancelled" => "[×]",
        _ => "[ ]",
    }
}

fn escape_table_cell(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace('\n', " ")
        .trim()
        .to_string()
}

fn write_command_block(stdout: &mut impl Write, arguments: &str) -> Result<()> {
    write_command_block_with_status(stdout, arguments, CommandStatus::Running)
}

fn write_command_block_with_status(
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

fn write_command_result_blocks(stdout: &mut impl Write, output: &str) -> Result<()> {
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

fn write_fenced_block(stdout: &mut impl Write, label: &str, text: &str) -> Result<()> {
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

struct CommandResult {
    success: bool,
    exit_code: Option<i64>,
    stdout: String,
    stderr: String,
}

fn parse_command_result(output: &str) -> Option<CommandResult> {
    let value = serde_json::from_str::<Value>(output.trim()).ok()?;
    Some(CommandResult {
        success: value.get("success")?.as_bool()?,
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
    })
}

fn format_tool_payload(payload: &str) -> String {
    let text = payload.trim();
    let formatted = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.to_string());
    truncate_chars(&formatted, 2400)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let omitted = total - max_chars;
    format!(
        "{}\n... {} {omitted} {} ...",
        text.chars().take(max_chars).collect::<String>(),
        t("truncated", "已截断"),
        t("chars", "字符")
    )
}

fn clip_progress_line(text: &str, max_chars: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max_chars {
        text
    } else {
        format!(
            "{}...",
            text.chars()
                .take(max_chars.saturating_sub(3))
                .collect::<String>()
        )
    }
}

fn clip_progress_line_preserving_spaces(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!(
            "{}...",
            text.chars()
                .take(max_chars.saturating_sub(3))
                .collect::<String>()
        )
    }
}

impl Drop for StreamRenderer {
    fn drop(&mut self) {
        let _ = self.stop_waiting();
        if let Some(mut display) = self.command_display.take() {
            let _ = display.clear(&mut self.output);
        }
        if self.summary_line_active {
            let _ = self.clear_summary_lines();
            eprintln!();
        }
        let _ = self.show_cursor();
        if !self.plain {
            let _ = execute!(self.output, ResetColor);
        }
    }
}

fn normalize_stream_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 流式版终端转义过滤:与 [`sanitize_terminal_text`] 同一状态机,但状态由
/// 调用方跨 delta 持有,转义序列被流切成两半也能整段拦下。
fn sanitize_stream_chunk(state: &mut TerminalControlState, text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for ch in text.chars() {
        if let Some(ch) = sanitize_terminal_char(state, ch) {
            output.push(ch);
        }
    }
    output
}

fn write_full_reasoning_chunk(writer: &mut impl Write, text: &str) -> Result<()> {
    execute!(writer, SetForegroundColor(Color::Green))?;
    write!(writer, "{text}")?;
    Ok(())
}

fn print_reasoning(reasoning: &str) -> Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, SetForegroundColor(Color::Green))?;
    for line in reasoning.trim().lines() {
        writeln!(stdout, "  {line}")?;
    }
    execute!(stdout, ResetColor)?;
    if terminal::size().is_ok() {
        writeln!(stdout)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible_command_lines(lines: Vec<String>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| strip_ansi_for_test(&line))
            .collect()
    }

    #[test]
    fn full_reasoning_reapplies_color_for_every_chunk() {
        let mut green = Vec::new();
        execute!(green, SetForegroundColor(Color::Green)).unwrap();
        let green = String::from_utf8(green).unwrap();
        let mut output = Vec::new();

        write_full_reasoning_chunk(&mut output, "用户").unwrap();
        execute!(output, ResetColor).unwrap();
        write_full_reasoning_chunk(&mut output, "询问明天几号").unwrap();

        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.matches(&green).count(), 2);
        assert!(output.ends_with("询问明天几号"));
    }

    #[test]
    fn command_stream_handles_split_utf8_and_crlf() {
        let mut state = CommandStreamState::default();
        let text = "开始\r\n完成\n".as_bytes();
        let split = "开始".len() - 1;

        assert!(state.push(&text[..split], 1).is_empty());
        let completed = state.push(&text[split..], 2);

        assert_eq!(completed.len(), 2);
        assert_eq!(completed[0].text, "开始");
        assert_eq!(completed[1].text, "完成");
        assert!(state.current.is_empty());
    }

    #[test]
    fn command_stream_carriage_return_replaces_current_line() {
        let mut state = CommandStreamState::default();

        assert!(state.push(b"progress 10%\r", 1).is_empty());
        let completed = state.push(b"progress 20%\n", 2);

        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].text, "progress 20%");
    }

    #[test]
    fn command_stream_strips_split_terminal_sequences() {
        let mut state = CommandStreamState::default();

        assert!(state.push(b"safe\x1b[31", 1).is_empty());
        let completed = state.push(b"m red\x1b[0m\n", 2);

        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].text, "safe red");
    }

    #[test]
    fn command_stream_finalizes_incomplete_utf8() {
        let mut state = CommandStreamState::default();

        assert!(state.push(&[0xe4, 0xb8], 1).is_empty());
        state.finalize_pending(1);

        assert_eq!(state.current, "�");
    }

    #[test]
    fn command_text_strips_cursor_and_osc_sequences() {
        assert_eq!(
            sanitize_terminal_text("safe\x1b[2J text\x1b]52;c;secret\x07 end"),
            "safe text end"
        );
        assert_eq!(sanitize_terminal_text("a\x1b(Bb"), "ab");
    }

    #[test]
    fn command_wrap_uses_terminal_width_for_wide_graphemes() {
        assert_eq!(wrap_plain_text("中文测试", 4), vec!["中文", "测试"]);
        assert_eq!(wrap_plain_text("a👨‍👩‍👧‍👦b", 3), vec!["a👨‍👩‍👧‍👦", "b"]);
        assert_eq!(wrap_plain_text("e\u{301}x", 1), vec!["e\u{301}", "x"]);
    }

    #[test]
    fn display_width_clip_preserves_graphemes_and_reserves_last_column() {
        assert_eq!(clip_to_display_width("中文测试", 5), "中文…");
        assert_eq!(clip_to_display_width("a👨‍👩‍👧‍👦bc", 4), "a👨‍👩‍👧‍👦…");
        assert_eq!(clip_to_display_width("e\u{301}x", 2), "e\u{301}x");

        for columns in [20, 40, 80] {
            let lines = transient_summary_lines(&format!("思考：{}", "中文".repeat(80)), columns);
            assert_eq!(lines.len(), 1);
            assert!(UnicodeWidthStr::width(lines[0].as_str()) < columns);
        }
    }

    #[test]
    fn command_preview_limits_physical_rows_and_keeps_tail() {
        let mut display = CommandLiveDisplay::new(r#"{"command":"demo"}"#, 3, true, false);
        display.push(CommandOutputStream::Stdout, b"one\ntwo\nthree\nfour\n");

        let lines = visible_command_lines(display.rendered_log_lines(80));

        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("omitted") || lines[0].contains("省略"));
        assert!(lines[1].ends_with("three"));
        assert!(lines[2].ends_with("four"));
    }

    #[test]
    fn command_preview_counts_soft_wrapped_rows() {
        let mut display = CommandLiveDisplay::new(r#"{"command":"demo"}"#, 3, true, false);
        display.push(
            CommandOutputStream::Stdout,
            "第一行很长\n第二行\n".as_bytes(),
        );

        let lines = visible_command_lines(display.rendered_log_lines(4));

        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("omitted") || lines[0].contains("省略"));
        assert!(lines[1].ends_with("第二"));
        assert!(lines[2].ends_with("行"));
    }

    #[test]
    fn command_preview_orders_interleaved_streams_and_colors_stderr() {
        let mut display = CommandLiveDisplay::new(r#"{"command":"demo"}"#, 4, true, false);
        display.push(CommandOutputStream::Stdout, b"out");
        display.push(CommandOutputStream::Stderr, b"err");

        let lines = display.rendered_log_lines(80);

        assert!(strip_ansi_for_test(&lines[0]).ends_with("out"));
        assert!(strip_ansi_for_test(&lines[1]).ends_with("err"));
        assert!(lines[0].contains("\x1b[2mout\x1b[0m"));
        assert!(!lines[0].contains("\x1b[33m"));
        assert!(lines[1].contains("\x1b[2m\x1b[31merr\x1b[0m"));
        assert!(lines[1].contains("\x1b[31m"));
    }

    #[test]
    fn shared_command_output_preview_sanitizes_and_keeps_tail() {
        let mut output = CommandOutputTail::new(3);
        output.push(
            CommandOutputStream::Stdout,
            b"old\nprogress 10%\rprogress 20%\n",
        );
        output.push(CommandOutputStream::Stderr, b"\x1b[31mwarning\x1b[0m\n");
        let chinese = "完成".as_bytes();
        output.push(CommandOutputStream::Stdout, &chinese[..2]);
        output.push(CommandOutputStream::Stdout, &chinese[2..]);

        let preview = output.preview();

        assert!(preview.omitted);
        assert_eq!(preview.lines.len(), 3);
        assert_eq!(preview.lines[0].text, "progress 20%");
        assert_eq!(preview.lines[1].stream, "stderr");
        assert_eq!(preview.lines[1].text, "warning");
        assert_eq!(preview.lines[2].text, "完成");
    }

    #[test]
    fn shared_command_output_preview_can_be_disabled() {
        let mut output = CommandOutputTail::new(0);
        output.push(CommandOutputStream::Stdout, b"hidden\n");

        let preview = output.preview();

        assert!(preview.lines.is_empty());
        assert!(!preview.omitted);
    }

    #[test]
    fn command_heading_is_part_of_live_block_and_updates_status() {
        let mut display = CommandLiveDisplay::new(r#"{"command":"printf ok"}"#, 2, true, false);
        let running = visible_command_lines(display.rendered_lines(80, true));
        let command = t("run command", "运行命令");
        assert_eq!(
            running[0],
            format!("$ {command}×1 {}", t("running", "运行中"))
        );
        assert!(running[1].contains("printf ok"));

        display.set_result(true);
        let completed = visible_command_lines(display.rendered_lines(80, false));
        assert_eq!(completed[0], format!("$ {command}×1 ok"));
        assert_eq!(
            completed
                .iter()
                .filter(|line| line.starts_with(&format!("$ {command}")))
                .count(),
            1
        );
    }

    #[test]
    fn compact_multiline_command_keeps_two_head_and_four_tail_lines() {
        let command = (1..=10)
            .map(|line| format!("command line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let arguments = serde_json::json!({ "command": command }).to_string();
        let display = CommandLiveDisplay::new(&arguments, 0, false, false);

        let lines = visible_command_lines(display.rendered_lines(120, false));

        assert_eq!(lines.len(), 8);
        assert!(lines[1].starts_with("  ↳ ") && lines[1].ends_with("command line 1"));
        assert!(lines[2].starts_with("  │ ") && lines[2].ends_with("command line 2"));
        assert!(lines[3].contains('4'));
        assert!(lines[3].contains("omitted") || lines[3].contains("省略"));
        assert!(lines[4].ends_with("command line 7"));
        assert!(lines[5].ends_with("command line 8"));
        assert!(lines[6].ends_with("command line 9"));
        assert!(lines[7].starts_with("  └ ") && lines[7].ends_with("command line 10"));
        assert!(!lines.iter().any(|line| line.ends_with("command line 3")));
        assert!(!lines.iter().any(|line| line.ends_with("command line 6")));
    }

    #[test]
    fn full_multiline_command_keeps_every_logical_line() {
        let command = (1..=10)
            .map(|line| format!("command line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let arguments = serde_json::json!({ "command": command }).to_string();
        let display = CommandLiveDisplay::new(&arguments, 0, false, true);

        let lines = visible_command_lines(display.rendered_lines(120, false));

        assert_eq!(lines.len(), 11);
        assert!(lines.iter().any(|line| line.ends_with("command line 3")));
        assert!(lines.iter().any(|line| line.ends_with("command line 6")));
        assert!(!lines
            .iter()
            .any(|line| line.contains("omitted") || line.contains("省略")));
    }

    #[test]
    fn multiline_command_soft_wraps_with_continuation_prefix() {
        let arguments = serde_json::json!({
            "command": "1234567890abcdef\nlast"
        })
        .to_string();
        let display = CommandLiveDisplay::new(&arguments, 0, false, false);

        let lines = visible_command_lines(display.rendered_lines(16, false));

        assert_eq!(lines[1], "  ↳ 123456789");
        assert_eq!(lines[2], "  │   0abcdef");
        assert_eq!(lines[3], "  └ last");
    }

    #[test]
    fn final_multiline_command_wrap_closes_tree_on_last_physical_row() {
        let arguments = serde_json::json!({
            "command": "first\n1234567890abcdef"
        })
        .to_string();
        let display = CommandLiveDisplay::new(&arguments, 0, false, false);

        let lines = visible_command_lines(display.rendered_lines(16, false));

        assert_eq!(lines[2], "  │ 123456789");
        assert_eq!(lines[3], "  └   0abcdef");
    }

    #[test]
    fn omitted_command_notice_wraps_within_narrow_width() {
        let command = (1..=10)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        let lines = render_command_preview(&command, 12, false, false, 0);

        assert!(lines.iter().all(|line| command_ansi_width(line) <= 12));
        assert!(visible_command_lines(lines)
            .iter()
            .any(|line| line.contains('4')));
    }

    #[test]
    fn static_full_command_block_shows_multiline_body() {
        let arguments = serde_json::json!({
            "command": "first\nsecond\nthird\nfourth\nfifth\nsixth\nseventh"
        })
        .to_string();
        let mut output = Vec::new();

        write_command_block_with_status(&mut output, &arguments, CommandStatus::Ok).unwrap();

        let output = strip_ansi_for_test(&String::from_utf8(output).unwrap());
        assert!(output.contains("  │ third\n"));
        assert!(output.contains("  └ seventh\n"));
        assert!(!output.contains("omitted") && !output.contains("省略"));
    }

    #[test]
    fn command_display_detects_output_row_growth_before_redraw() {
        let mut display = CommandLiveDisplay::new(r#"{"command":"printf ok"}"#, 3, true, false);
        display.rendered_line_widths = display
            .rendered_lines(80, true)
            .iter()
            .map(|line| command_ansi_width(line))
            .collect();
        assert!(!display.tick_changes_layout_at_width(80));

        display.push(CommandOutputStream::Stdout, b"one\n");

        assert!(display.tick_changes_layout_at_width(80));
    }

    #[test]
    fn committed_command_blocks_end_with_exactly_one_blank_line() {
        let mut live = Vec::new();
        write_command_block_gap(&mut live, false).unwrap();
        assert_eq!(live, b"\n\n");

        let mut already_terminated = Vec::new();
        write_command_block_gap(&mut already_terminated, true).unwrap();
        assert_eq!(already_terminated, b"\n");
    }

    #[test]
    fn run_command_replaces_an_active_tool_summary() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.live_summary = true;
        renderer.summary_line_active = true;
        renderer.summary_lines_active = 1;

        renderer
            .write_tool_call("run_command", r#"{"command":"printf ok"}"#)
            .unwrap();

        assert!(!renderer.summary_line_active);
        assert_eq!(renderer.summary_lines_active, 0);
        assert!(renderer.command_display.is_some());
        assert!(renderer.tool_stats.is_empty());
    }

    #[test]
    fn completed_tools_are_committed_per_call_instead_of_aggregated() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.live_summary = false;

        renderer
            .write_tool_call("web_search", r#"{"query":"first subject"}"#)
            .unwrap();
        assert_eq!(
            renderer.tool_summary_text(),
            format!(
                "~ {}×1 {}\n  ↳ first subject",
                t("Web search", "网络搜索"),
                t("running", "运行中")
            )
        );
        renderer
            .write_tool_result("web_search", true, "{}")
            .unwrap();
        assert!(renderer.tool_stats.is_empty());

        renderer
            .write_tool_call("web_search", r#"{"query":"second subject"}"#)
            .unwrap();
        assert_eq!(
            renderer.tool_summary_text(),
            format!(
                "~ {}×1 {}\n  ↳ second subject",
                t("Web search", "网络搜索"),
                t("running", "运行中")
            )
        );
    }

    #[test]
    fn tool_summary_uses_spinner_and_updates_subagent_elapsed_time() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.live_summary = true;

        renderer
            .write_tool_call(
                "task",
                r#"{"description":"确认工作区环境","prompt":"details"}"#,
            )
            .unwrap();

        assert!(renderer.wait_spinner.is_some());
        assert!(!renderer.summary_line_active);
        assert_eq!(
            renderer.tool_summary_text(),
            format!(
                "~ {}×1 {} · 0s\n  ↳ 确认工作区环境",
                t("Subagent", "子代理"),
                t("running", "运行中")
            )
        );
        renderer.tool_stats.get_mut("task").unwrap().started_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
        renderer.tick_spinner().unwrap();
        assert_eq!(
            renderer.tool_summary_text(),
            format!(
                "~ {}×1 {} · 2s\n  ↳ 确认工作区环境",
                t("Subagent", "子代理"),
                t("running", "运行中")
            )
        );
    }

    #[test]
    fn subagent_summary_keeps_current_internal_tool_without_raw_reasoning() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Full,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.live_summary = false;
        renderer
            .write_tool_call(
                "task",
                r#"{"description":"查询磁盘占用","prompt":"details"}"#,
            )
            .unwrap();
        renderer
            .write_tool_progress("task", "工具 #2：运行命令 · du -sh /home/shorin/* 运行中")
            .unwrap();
        renderer
            .write_tool_progress("task", "__subagent_reasoning__private analysis")
            .unwrap();

        let summary = renderer.tool_summary_text();
        assert!(summary.contains("↳ 查询磁盘占用"));
        assert!(summary.contains("↳ 工具 #2：运行命令 · du -sh /home/shorin/* 运行中"));
        assert!(!summary.contains("private analysis"));
        assert_eq!(renderer.subagent_mode, None);
    }

    #[test]
    fn external_output_clears_every_active_summary_row() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.live_summary = true;
        renderer.summary_line_active = true;
        renderer.summary_lines_active = 2;

        renderer.prepare_for_external_output().unwrap();

        assert!(!renderer.summary_line_active);
        assert_eq!(renderer.summary_lines_active, 0);
    }

    #[test]
    fn streams_only_complete_lines() {
        let mut renderer = MarkdownStreamRenderer::new();
        assert_eq!(renderer.push("**bo"), "");
        assert_eq!(
            renderer.push("ld**\n"),
            format!("{BOLD_STYLE}bold{RESET}\n")
        );
    }

    #[test]
    fn flushes_partial_final_line() {
        let mut renderer = MarkdownStreamRenderer::new();
        assert_eq!(renderer.push("# Title"), "");
        assert_eq!(renderer.flush(), format!("{HEADER_STYLE}# Title{RESET}\n"));
    }

    #[test]
    fn headings_use_one_color_and_distinct_prefix_lengths() {
        assert_eq!(
            render_markdown_line("# One"),
            format!("{HEADER_STYLE}# One{RESET}")
        );
        assert_eq!(
            render_markdown_line("## Two"),
            format!("{HEADER_STYLE}## Two{RESET}")
        );
        assert_eq!(
            render_markdown_line("### Three"),
            format!("{HEADER_STYLE}### Three{RESET}")
        );
        assert_eq!(
            render_markdown_line("###### Six"),
            format!("{HEADER_STYLE}###### Six{RESET}")
        );
    }

    #[test]
    fn list_markers_use_tertiary_color() {
        assert!(render_markdown_line("- item").contains(&format!("{TERTIARY_STYLE}-{RESET}")));
        assert!(render_markdown_line("1. item").contains(&format!("{TERTIARY_STYLE}1.{RESET}")));
    }

    #[test]
    fn token_usage_hides_zero_turn_tokens() {
        assert_eq!(
            format_token_usage_inline(&TokenMeter {
                session_tokens: 1_300,
                context_window: Some(272_000),
                ..Default::default()
            }),
            "1.3k/272k(0.5%)"
        );
        assert_eq!(
            format_token_usage_inline(&TokenMeter {
                turn_tokens: 1_300,
                session_tokens: 1_300,
                context_window: Some(272_000),
                ..Default::default()
            }),
            "1.3k · 1.3k/272k(0.5%)"
        );
        assert_eq!(
            format_token_usage_inline(&TokenMeter {
                turn_tokens: 5_300,
                session_tokens: 10_000,
                context_window: Some(200_000),
                cumulative_tokens: Some(86_200),
                ..Default::default()
            }),
            "5.3k · 10k/200k(5.0%) · Σ86.2k"
        );
    }

    #[test]
    fn a_cache_rate_divides_by_the_prompt_not_the_whole_turn() {
        // 24.8k turn = 12.0k prompt + 12.8k output, 11.2k of the prompt cached.
        // Dividing by the turn total would report 45% and would sag further the
        // longer the model talked, which says nothing about the cache.
        let meter = TokenMeter {
            turn_tokens: 24_800,
            turn_prompt_tokens: 12_000,
            turn_cached_tokens: 11_200,
            session_tokens: 12_000,
            context_window: Some(200_000),
            cumulative_tokens: Some(380_000),
            cumulative_prompt_tokens: 248_000,
            cumulative_cached_tokens: 226_000,
        };
        assert_eq!(
            format_token_usage_inline(&meter),
            "24.8k(C93%) · 12k/200k(6.0%) · Σ380k(C91%)"
        );
    }

    #[test]
    fn a_provider_that_reports_no_cache_shows_no_rate() {
        // Turns recorded before the cache columns existed read as zeros; a flat
        // "C0%" would be a claim the database cannot support.
        let meter = TokenMeter {
            turn_tokens: 5_300,
            turn_prompt_tokens: 4_000,
            session_tokens: 10_000,
            context_window: Some(200_000),
            cumulative_tokens: Some(86_200),
            cumulative_prompt_tokens: 70_000,
            ..Default::default()
        };
        assert_eq!(
            format_token_usage_inline(&meter),
            "5.3k · 10k/200k(5.0%) · Σ86.2k"
        );
    }

    #[test]
    fn buffers_tables_until_non_table_line() {
        let mut renderer = MarkdownStreamRenderer::new();
        assert_eq!(renderer.push("| a | b |\n"), "");
        assert_eq!(renderer.push("| - | - |\n"), "");
        let output = renderer.push("| 1 | 2 |\n");
        assert!(output.contains(&format!("{BOLD_STYLE}a{RESET}")));
        assert!(output.contains("1"));
        assert!(output.contains('┌'));
        assert!(output.contains('┬'));
        assert!(output.contains('├'));
        assert!(output.contains('┼'));
        assert!(output.contains("\x1b[2m│\x1b[0m"));
        assert!(output.contains('─'));
        assert!(!output.contains('+'));
        let output = renderer.push("done\n");
        assert!(output.contains('└'));
        assert!(output.ends_with("done\n"));
    }

    #[test]
    fn short_tables_use_content_width() {
        let output = render_table(&[
            "| 项目 | 内容 |".to_string(),
            "|---|---|".to_string(),
            "| 名字 | 未有 / Miyu |".to_string(),
            "| 年龄 | 18 |".to_string(),
        ]);
        let terminal_width = terminal::size()
            .map(|(width, _)| usize::from(width))
            .unwrap_or(100);
        let widest = output.lines().map(visible_width).max().unwrap_or(0);
        assert!(widest < terminal_width / 2, "table too wide: {widest}");
    }

    #[test]
    fn todo_output_uses_single_column_rendered_table() {
        let output = render_todo_table(&[
            "| #Todo |".to_string(),
            "|---|".to_string(),
            "| [·] 修复 todo 表格渲染 |".to_string(),
            "| [ ] 补充单元测试 |".to_string(),
            "| [✔] 跑 cargo test |".to_string(),
        ]);
        let visible = strip_ansi_for_test(&output);
        assert!(output.contains('┌'));
        assert!(output.contains('├'));
        assert!(output.contains('└'));
        assert!(!output.contains('┬'));
        assert!(!output.contains('┼'));
        assert!(!output.contains('┴'));
        assert!(visible.contains("#Todo"));
        assert!(!output.contains(&format!("{BOLD_STYLE}#Todo{RESET}")));
        assert_eq!(visible.matches('│').count(), 8);
        assert!(visible.contains("[·]"));
        assert!(visible.contains("todo"));
        assert!(visible.contains("[ ]"));
        assert!(visible.contains("[✔]"));
        assert!(!visible.contains("优先级"));
        assert!(!visible.contains("序号"));
        let terminal_width = terminal::size()
            .map(|(width, _)| usize::from(width))
            .unwrap_or(100);
        for line in output.lines() {
            assert!(
                visible_width(line) < terminal_width,
                "line too wide: {line}"
            );
        }
    }

    #[test]
    fn todo_status_symbols_contribute_to_table_width() {
        assert_eq!(visible_width("把冰箱门打开"), 12);
        assert_eq!(visible_width("[✔] 把冰箱门打开"), 16);
        assert_eq!(visible_width("[·] 把冰箱门打开"), 16);

        let lines = [
            "| #Todo |".to_string(),
            "|---|".to_string(),
            "| [✔] 把冰箱门打开 |".to_string(),
            "| [·] 把冰箱门关上 |".to_string(),
        ];
        let normal = render_table(&lines);
        let output = render_todo_table(&lines);
        let visible = strip_ansi_for_test(&output);
        assert_eq!(
            visible_width(output.lines().next().unwrap()),
            visible_width(normal.lines().next().unwrap())
        );
        assert!(!output.contains(&format!("{BOLD_STYLE}#Todo{RESET}")));
        assert!(visible.contains("[✔]"));
        assert!(visible.contains("[·]"));
        assert_eq!(visible.lines().filter(|line| line.contains('│')).count(), 3);
    }

    #[test]
    fn patch_diff_uses_muted_change_backgrounds() {
        let diff = "--- a/demo.txt\n+++ b/demo.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n";
        let output = render_patch_diff("demo.txt", diff);

        assert!(output.contains("\x1b[48;2;60;41;53m"));
        assert!(output.contains("\x1b[48;2;32;52;67m"));
        assert!(!output.contains("\x1b[48;5;52m"));
        assert!(!output.contains("\x1b[48;5;22m"));
    }

    #[test]
    fn patch_diff_wraps_long_lines_with_aligned_gutter() {
        let diff = format!(
            "--- a/run-vm.sh\n+++ b/run-vm.sh\n@@ -1,0 +1,1 @@\n+{}\n",
            "RESULT=$(sudo virsh qemu-agent-command archlinux ".repeat(8)
        );
        let output = render_patch_diff("run-vm.sh", &diff);
        let visible = strip_ansi_for_test(&output);
        let diff_lines = visible
            .lines()
            .filter(|line| line.contains('│'))
            .collect::<Vec<_>>();
        assert!(diff_lines.len() > 1, "diff line was not wrapped: {visible}");
        assert!(diff_lines[0].starts_with("    1 + │ "));
        assert!(diff_lines[1].starts_with("        │ "));

        let terminal_width = terminal::size()
            .map(|(width, _)| usize::from(width))
            .unwrap_or(100);
        for line in output.lines().filter(|line| line.contains('│')) {
            assert!(
                visible_width(line) < terminal_width,
                "diff line too wide: {line}"
            );
        }
    }

    #[test]
    fn patch_diff_wraps_wide_character_lines() {
        let diff = format!(
            "--- a/demo.txt\n+++ b/demo.txt\n@@ -1,0 +1,1 @@\n+{}\n",
            "软换行问题".repeat(30)
        );
        let output = render_patch_diff("demo.txt", &diff);
        let visible = strip_ansi_for_test(&output);
        assert!(visible.lines().filter(|line| line.contains('│')).count() > 1);

        let terminal_width = terminal::size()
            .map(|(width, _)| usize::from(width))
            .unwrap_or(100);
        for line in output.lines().filter(|line| line.contains('│')) {
            assert!(
                visible_width(line) < terminal_width,
                "wide-char diff line too wide: {line}"
            );
        }
    }

    fn strip_ansi_for_test(input: &str) -> String {
        let mut output = String::new();
        let mut chars = input.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            output.push(ch);
        }
        output
    }

    #[test]
    fn wraps_wide_table_cells_to_terminal_width() {
        let output = render_table(&[
            "| 项目 | 内容 |".to_string(),
            "|---|---|".to_string(),
            format!("| 很长 | {} |", "这是一段非常长的内容".repeat(20)),
        ]);
        let terminal_width = terminal::size()
            .map(|(width, _)| usize::from(width))
            .unwrap_or(100);
        for line in output.lines() {
            assert!(
                visible_width(line) < terminal_width,
                "line too wide: {line}"
            );
        }
        assert!(output.lines().count() > 5);
    }

    #[test]
    fn many_column_tables_stay_within_terminal_width() {
        let output = render_table(&[
            "| 参数名 | 参数类型 | 默认值 | 是否必填 | 说明 | 取值范围 | 示例值 | 适用版本 | 更新日志 | 备注 |".to_string(),
            "|---|---|---|---|---|---|---|---|---|---|".to_string(),
            "| database_host | string | localhost | 否 | 数据库主机地址 | 合法IP或域名 | 192.168.1.100 | v1.0+ | 无 | 支持IPv6 |".to_string(),
        ]);
        let terminal_width = terminal::size()
            .map(|(width, _)| usize::from(width))
            .unwrap_or(100);
        for line in output.lines() {
            assert!(
                visible_width(line) < terminal_width,
                "line too wide: {line}"
            );
        }
    }

    #[test]
    fn blockquote_is_visually_distinct() {
        let mut renderer = MarkdownStreamRenderer::new();
        let output = renderer.push(">> quoted\n");
        assert!(output.contains("\x1b[32m| \x1b[0m\x1b[32m| \x1b[0m"));
        assert!(output.contains("\x1b[32mquoted\x1b[0m"));
        assert!(!output.contains("48;5;236"));
    }

    #[test]
    fn code_block_has_label_and_readable_content() {
        let mut renderer = MarkdownStreamRenderer::new();
        let output = renderer.push("```rust\nfn main() {}\n```\n");
        assert!(output.contains("╭─ code rust"));
        assert!(!output.contains(",-- code rust"));
        assert!(!output.contains("\x1b[2m|\x1b[0m"));
        assert!(output.contains(&format!(
            "{CODE_BLOCK_BG}{CODE_KEYWORD_STYLE}fn{CODE_TOKEN_RESET}"
        )));
        assert!(output.contains(&format!("{CODE_FUNCTION_STYLE}main{CODE_TOKEN_RESET}")));
        assert!(output.contains(&format!("{CODE_BLOCK_FRAME_STYLE}╭─ code rust ─")));
        assert!(output.contains(&format!(
            "{CODE_BLOCK_FRAME_STYLE}{}{RESET}",
            "─".repeat(24)
        )));
        assert!(!output.contains("`--"));
    }

    #[test]
    fn code_block_content_has_default_color() {
        let mut renderer = MarkdownStreamRenderer::new();
        let output = renderer.push("```\nXMODIFIERS \"@im=fcitx\"\n```\n");
        assert!(output.contains(&format!(
            "{CODE_BLOCK_BG}XMODIFIERS \"@im=fcitx\"{}{RESET}",
            " ".repeat(2)
        )));
        assert!(!output.contains("\x1b[33mXMODIFIERS"));
    }

    #[test]
    fn code_block_variables_use_primary_color() {
        let mut renderer = MarkdownStreamRenderer::new();
        let output = renderer.push("```rust\nlet msg = String::from(\"hi\");\n```\n");
        assert!(output.contains(&format!("{PRIMARY_STYLE}msg{CODE_TOKEN_RESET}")));
    }

    #[test]
    fn code_block_background_uses_longest_line_width() {
        let mut renderer = MarkdownStreamRenderer::new();
        let output = renderer.push("```\nshort\nlonger line\n```\n");
        assert!(output.contains(&format!("{CODE_BLOCK_BG}short{}{RESET}", " ".repeat(19))));
        assert!(output.contains(&format!(
            "{CODE_BLOCK_BG}longer line{}{RESET}",
            " ".repeat(13)
        )));
        assert!(output.contains(&format!(
            "{CODE_BLOCK_FRAME_STYLE}{}{RESET}",
            "─".repeat(24)
        )));
        assert!(!output.contains("48;5;236"));
    }

    #[test]
    fn renders_more_inline_markdown() {
        let output = render_inline(
            "*i* ~~gone~~ [site](https://example.com) <https://example.org> ![pic](https://img)",
        );
        assert!(output.contains(&format!("{ITALIC_STYLE}i{RESET}")));
        assert!(output.contains(&format!("{STRIKE_STYLE}gone{RESET}")));
        assert!(output.contains(&format!("<{URL_STYLE}https://example.com{RESET}>")));
        assert!(output.contains(&format!(
            "\x1b[4m<{URL_STYLE}https://example.org{RESET}>{RESET}"
        )));
        assert!(output.contains(&format!(
            "{IMAGE_STYLE}[image: pic]{RESET}({URL_STYLE}https://img{RESET})"
        )));
        assert!(!output.contains("\x1b[35mimage\x1b[0m"));
    }

    #[test]
    fn renders_inline_code_at_start_of_bullet() {
        let output = render_markdown_line("- `read_file` — 读文件内容");
        assert!(output.contains(&format!("{INLINE_CODE_STYLE}read_file\x1b[0m")));
        assert!(output.contains("— 读文件内容"));
    }

    #[test]
    fn renders_multiple_inline_code_spans_in_bullet_with_chinese_text() {
        let output = render_markdown_line(
            "- `~/.config/Thunar/` - 里面有 `accels.scm`（快捷键绑定）和 `uca.xml`（自定义右键菜单）",
        );
        assert!(output.contains(&format!("{INLINE_CODE_STYLE}~/.config/Thunar/\x1b[0m")));
        assert!(output.contains(&format!("{INLINE_CODE_STYLE}accels.scm\x1b[0m")));
        assert!(output.contains(&format!("{INLINE_CODE_STYLE}uca.xml\x1b[0m")));
        assert!(!output.contains('`'));
    }

    #[test]
    fn renders_inline_code_when_stream_chunks_split_backticks() {
        let mut renderer = MarkdownStreamRenderer::new();
        assert_eq!(renderer.push("- `~/.config/Thu"), "");
        let output = renderer.push("nar/` - 里面有 `accels.scm`\n");
        assert!(output.contains(&format!("{INLINE_CODE_STYLE}~/.config/Thunar/\x1b[0m")));
        assert!(output.contains(&format!("{INLINE_CODE_STYLE}accels.scm\x1b[0m")));
        assert!(!output.contains('`'));
    }

    #[test]
    fn tool_status_prefers_running_for_single_active_call() {
        let stats = ToolStats {
            calls: 1,
            ok: 0,
            error: 0,
            subject: None,
            progress: None,
            final_progress: None,
            ..ToolStats::default()
        };
        assert_eq!(
            tool_status_text("grep", &stats, false),
            format!("grep×1 {}", t("running", "运行中"))
        );
    }

    #[test]
    fn tool_status_uses_simple_single_success() {
        let stats = ToolStats {
            calls: 1,
            ok: 1,
            error: 0,
            subject: None,
            progress: None,
            final_progress: None,
            ..ToolStats::default()
        };
        assert_eq!(tool_status_text("grep", &stats, false), "grep×1 ok");
    }

    #[test]
    fn detached_subagents_drop_the_meaningless_elapsed_timer() {
        let finished = ToolStats {
            calls: 1,
            ok: 1,
            elapsed: Some(std::time::Duration::from_secs(12)),
            ..ToolStats::default()
        };
        assert_eq!(
            tool_status_text("子代理", &finished, true),
            "子代理×1 ok · 12s"
        );

        // Handing off to the background returns immediately, so the timer only
        // ever read `0s` — which looked like the work had finished instantly.
        let detached = ToolStats {
            calls: 1,
            ok: 1,
            elapsed: Some(std::time::Duration::from_millis(3)),
            detached: true,
            ..ToolStats::default()
        };
        assert_eq!(tool_status_text("子代理", &detached, true), "子代理×1 ok");
    }

    #[test]
    fn tool_status_subagent_tool_keeps_count_suffix() {
        let stats = ToolStats {
            calls: 1,
            ok: 0,
            error: 0,
            subject: None,
            progress: None,
            final_progress: None,
            ..ToolStats::default()
        };
        assert_eq!(
            tool_status_text("deep_research", &stats, true),
            format!("deep_research×1 {}", t("running", "运行中"))
        );
        let stats = ToolStats {
            calls: 1,
            ok: 1,
            error: 0,
            subject: None,
            progress: None,
            final_progress: None,
            ..ToolStats::default()
        };
        assert_eq!(
            tool_status_text("deep_research", &stats, true),
            "deep_research×1 ok"
        );
    }

    #[test]
    fn subagent_status_shows_live_and_frozen_elapsed_time() {
        let running = ToolStats {
            calls: 1,
            started_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(68)),
            ..ToolStats::default()
        };
        assert_eq!(
            tool_status_text("task", &running, true),
            format!("task×1 {} · 1m 08s", t("running", "运行中"))
        );
        assert_eq!(
            tool_status_text("task", &running, false),
            format!("task×1 {}", t("running", "运行中"))
        );

        let completed = ToolStats {
            calls: 1,
            ok: 1,
            elapsed: Some(std::time::Duration::from_secs(3_720)),
            ..ToolStats::default()
        };
        assert_eq!(
            tool_status_text("deep_research", &completed, true),
            "deep_research×1 ok · 1h 02m"
        );
    }

    #[test]
    fn elapsed_time_formats_seconds_minutes_and_hours() {
        assert_eq!(format_elapsed(std::time::Duration::from_secs(5)), "5s");
        assert_eq!(format_elapsed(std::time::Duration::from_secs(65)), "1m 05s");
        assert_eq!(
            format_elapsed(std::time::Duration::from_secs(7_380)),
            "2h 03m"
        );
    }

    #[test]
    fn full_mode_subagent_result_uses_elapsed_status_and_clears_timer() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Full,
            false,
            true,
            10,
        );
        renderer.live_summary = false;
        renderer
            .write_tool_call("task", r#"{"description":"计时","prompt":"details"}"#)
            .unwrap();
        renderer.tool_stats.get_mut("task").unwrap().started_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(5));

        renderer.write_tool_result("task", true, "{}").unwrap();

        assert!(!renderer.tool_stats.contains_key("task"));
        assert_eq!(
            tool_result_status("ok", Some(std::time::Duration::from_secs(5))),
            "ok · 5s"
        );
    }

    #[test]
    fn tool_summary_suppresses_subagent_reasoning_even_when_reasoning_is_full() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Full,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.live_summary = false;
        renderer
            .write_tool_call("task", r#"{"description":"分析问题","prompt":"details"}"#)
            .unwrap();

        renderer
            .write_tool_progress("task", "__subagent_reasoning__Inspecting state")
            .unwrap();

        let stats = renderer.tool_stats.get("task").unwrap();
        assert_eq!(stats.calls, 1);
        assert!(stats.started_at.is_some());
        assert_eq!(renderer.subagent_mode, None);
    }

    #[test]
    fn tool_summary_keeps_final_subagent_stats() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );
        renderer.tool_stats.insert(
            "deep_research".to_string(),
            ToolStats {
                calls: 1,
                ok: 1,
                error: 0,
                subject: None,
                progress: None,
                final_progress: Some("工具调用 1 次　消耗词元 2.3K".to_string()),
                ..ToolStats::default()
            },
        );

        assert_eq!(
            renderer.tool_summary_text(),
            format!(
                "~ {}×1 ok\n  ✓ 工具调用 1 次　消耗词元 2.3K",
                t("Deep research", "深度研究")
            )
        );
    }

    #[test]
    fn task_summary_omits_tool_prefix() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );
        renderer.tool_stats.insert(
            "task".to_string(),
            ToolStats {
                calls: 1,
                ok: 0,
                error: 0,
                subject: Some("定位活动摘要渲染链路".to_string()),
                progress: None,
                final_progress: None,
                ..ToolStats::default()
            },
        );

        let header = format!("~ {}×1 {}", t("Subagent", "子代理"), t("running", "运行中"));
        assert_eq!(renderer.tool_summary_header(), header);
        assert_eq!(
            renderer.tool_summary_text(),
            format!("{header}\n  ↳ 定位活动摘要渲染链路")
        );
    }

    #[test]
    fn parallel_subagents_render_stacked_blocks() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );
        for (name, subject, progress) in [
            ("task:任务A", "任务A", Some("工具 #1: 运行命令")),
            ("task:任务B", "任务B", None),
            ("task:任务C", "任务C", Some("正在搜索")),
        ] {
            renderer.tool_stats.insert(
                name.to_string(),
                ToolStats {
                    calls: 1,
                    ok: 0,
                    error: 0,
                    subject: Some(subject.to_string()),
                    progress: progress.map(str::to_string),
                    final_progress: None,
                    ..ToolStats::default()
                },
            );
        }
        let (phase, sub) = renderer.tool_summary_live();
        // Block mode: no shared phase line — every subagent is its own block.
        assert_eq!(phase, "");
        let sub = sub.expect("stacked blocks present");
        let marker = wait_spinner::BLOCK_MARKER;
        let lines: Vec<&str> = sub.lines().collect();
        // Each running block header carries the spinner marker; its own
        // progress follows; blank lines separate blocks. The redundant
        // subject line (same as the description in the header) is dropped.
        assert!(lines[0].starts_with(marker) && lines[0].contains("任务A"));
        assert_eq!(lines[1], "  ↳ 工具 #1: 运行命令");
        assert_eq!(lines[2], "");
        assert!(lines[3].starts_with(marker) && lines[3].contains("任务B"));
        assert_eq!(lines[4], "");
        assert!(lines[5].starts_with(marker) && lines[5].contains("任务C"));
        assert_eq!(lines[6], "  ↳ 正在搜索");
        assert_eq!(lines.len(), 7);
    }

    #[test]
    fn live_blocks_freeze_settled_subagents_in_place() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );
        renderer.tool_stats.insert(
            "task:任务A".to_string(),
            ToolStats {
                calls: 1,
                subject: Some("任务A".to_string()),
                progress: Some("正在搜索".to_string()),
                ..ToolStats::default()
            },
        );
        renderer.tool_stats.insert(
            "task:任务B".to_string(),
            ToolStats {
                calls: 1,
                ok: 1,
                subject: Some("任务B".to_string()),
                final_progress: Some("工具调用 1 次".to_string()),
                ..ToolStats::default()
            },
        );
        let (phase, sub) = renderer.tool_summary_live();
        assert_eq!(phase, "");
        let sub = sub.expect("blocks present");
        let marker = wait_spinner::BLOCK_MARKER;
        let lines: Vec<&str> = sub.lines().collect();
        // Running block keeps its animated marker + indented live progress…
        assert!(lines[0].starts_with(marker) && lines[0].contains("任务A"));
        assert_eq!(lines[1], "  ↳ 正在搜索");
        assert_eq!(lines[2], "");
        // …while the settled block drops the spinner glyph from its header;
        // detail lines stay two columns in, matching the committed layout.
        assert!(lines[3].starts_with("~ ") && lines[3].contains("任务B"));
        assert!(lines[3].contains("ok"));
        assert_eq!(lines[4], "  ✓ 工具调用 1 次");
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn committed_summary_keeps_block_headers_when_one_subagent_finishes() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );
        renderer.tool_stats.insert(
            "task:任务A".to_string(),
            ToolStats {
                calls: 1,
                subject: Some("任务A".to_string()),
                ..ToolStats::default()
            },
        );
        renderer.tool_stats.insert(
            "task:任务B".to_string(),
            ToolStats {
                calls: 1,
                ok: 1,
                subject: Some("任务B".to_string()),
                final_progress: Some("工具调用 1 次".to_string()),
                ..ToolStats::default()
            },
        );
        let text = renderer.tool_summary_text();
        let lines: Vec<&str> = text.lines().collect();
        // Each block keeps its own "~" header; a blank line separates blocks.
        assert!(lines[0].starts_with("~ ") && lines[0].contains("任务A"));
        assert_eq!(lines[1], "");
        assert!(lines[2].starts_with("~ ") && lines[2].contains("任务B"));
        assert_eq!(lines[3], "  ✓ 工具调用 1 次");
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn all_subagent_summaries_use_activity_prefix() {
        for name in ["task", "deep_research"] {
            let mut renderer = StreamRenderer::new(
                ReasoningDisplayMode::Summary,
                ToolCallDisplayMode::Summary,
                true,
                true,
                10,
            );
            renderer.tool_stats.insert(
                name.to_string(),
                ToolStats {
                    calls: 1,
                    ok: 0,
                    error: 0,
                    subject: None,
                    progress: None,
                    final_progress: None,
                    ..ToolStats::default()
                },
            );

            assert_eq!(
                renderer.tool_summary_header(),
                format!(
                    "~ {}×1 {}",
                    readable_tool_name(name),
                    t("running", "运行中")
                )
            );
        }
    }

    #[test]
    fn load_tools_keeps_targets_on_the_status_line() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.tool_stats.insert(
            "load_tools:web_search,get_weather".to_string(),
            ToolStats {
                calls: 1,
                ok: 1,
                subject: Some("网络搜索、天气查询".to_string()),
                ..ToolStats::default()
            },
        );

        assert_eq!(
            renderer.tool_summary_text(),
            format!("~ {}×1 ok · 网络搜索、天气查询", t("Load", "加载"))
        );
        assert!(!renderer.tool_summary_text().contains("\n↳"));
    }

    #[test]
    fn tool_status_counts_mixed_multiple_calls() {
        let stats = ToolStats {
            calls: 3,
            ok: 1,
            error: 1,
            subject: None,
            progress: None,
            final_progress: None,
            ..ToolStats::default()
        };
        assert_eq!(
            tool_status_text("grep", &stats, false),
            format!("grep×3 {}:1 ok:1 err:1", t("running", "运行中"))
        );
    }

    #[test]
    fn trash_subject_counts_items_but_names_a_lone_path() {
        assert_eq!(
            tool_subject("trash_path", r#"{"paths":["/tmp/a","/tmp/b","/tmp/c"]}"#).as_deref(),
            Some(t("3 items", "3 项"))
        );
        // 只删一个时路径比「1 项」有用。
        assert_eq!(
            tool_subject("trash_path", r#"{"paths":["/tmp/only.txt"]}"#).as_deref(),
            Some("/tmp/only.txt")
        );
        assert_eq!(tool_subject("trash_path", r#"{"paths":[]}"#), None);
    }

    /// 失败清单自带 `✗`,不能再被套一个 `✓` 变成「✓ ✗ 权限不足」。
    #[test]
    fn final_progress_keeps_lines_that_carry_their_own_marker() {
        let renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        let stats = ToolStats {
            calls: 1,
            ok: 1,
            final_progress: Some("✗ /etc/hosts  权限不足\n收尾完成".to_string()),
            ..Default::default()
        };
        let lines = renderer.tool_block_lines("trash_path", &stats, false);
        assert!(
            lines.iter().any(|line| line.trim() == "✗ /etc/hosts  权限不足"),
            "失败行应原样保留：{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.trim() == "✓ 收尾完成"),
            "普通行仍要套 ✓：{lines:?}"
        );
    }

    #[test]
    fn tool_subject_extracts_safe_operation_targets() {
        assert_eq!(
            tool_subject("web_search", r#"{"query":"OpenCode 工具摘要"}"#).as_deref(),
            Some("OpenCode 工具摘要")
        );
        assert_eq!(
            tool_subject(
                "task",
                r#"{"description":"定位渲染链路","prompt":"private details"}"#
            )
            .as_deref(),
            Some("定位渲染链路")
        );
        assert_eq!(
            tool_subject("grep", r#"{"pattern":"ToolStats","path":"src"}"#).as_deref(),
            Some("ToolStats · src")
        );
        assert_eq!(
            tool_subject("run_command", r#"{"command":"du -sh /home/shorin/*"}"#).as_deref(),
            Some("du -sh /home/shorin/*")
        );
        let expected_load_tools_subject = format!(
            "{}{}{}",
            t("Web search", "网络搜索"),
            t(", ", "、"),
            t("Weather", "天气查询")
        );
        assert_eq!(
            tool_subject(
                "load_tools:web_search,get_weather",
                r#"{"names":["web_search","get_weather"]}"#
            )
            .as_deref(),
            Some(expected_load_tools_subject.as_str())
        );
    }

    #[test]
    fn read_file_subject_shows_the_page_range() {
        assert_eq!(
            tool_subject("read_file", r#"{"path":"/tmp/a.rs"}"#).as_deref(),
            Some("/tmp/a.rs")
        );
        assert_eq!(
            tool_subject("read_file", r#"{"path":"/tmp/a.rs","offset":2001,"limit":2000}"#)
                .as_deref(),
            Some("/tmp/a.rs (L2001-4000)")
        );
        assert_eq!(
            tool_subject("read_file", r#"{"path":"/tmp/a.rs","limit":500}"#).as_deref(),
            Some("/tmp/a.rs (L1-500)")
        );
        assert_eq!(
            tool_subject("read_file", r#"{"path":"/tmp/a.rs","offset":300}"#).as_deref(),
            Some("/tmp/a.rs (L300+)")
        );
    }

    #[test]
    fn tool_subject_redacts_urls_and_ignores_unknown_arguments() {
        let subject = tool_subject(
            "web_fetch",
            r#"{"url":"https://user:secret@example.com/path?token=hidden#fragment"}"#,
        )
        .unwrap();
        assert_eq!(subject, "https://example.com/path");
        assert!(!subject.contains("secret"));
        assert!(!subject.contains("token"));
        assert_eq!(
            tool_subject("mcp_unknown", r#"{"password":"hidden","query":"private"}"#),
            None
        );
        assert_eq!(
            tool_subject(
                "web_search",
                r#"{"query":"查找 token=super-secret, Rust 文档"}"#
            )
            .as_deref(),
            Some("查找 token=[redacted], Rust 文档")
        );
        assert_eq!(
            safe_inline_subject(r#"请求 {"token":"super-secret"}"#).as_deref(),
            Some(r#"请求 {"token":"[redacted]"}"#)
        );
        assert_eq!(
            safe_inline_subject("Authorization Bearer super-secret").as_deref(),
            Some("Authorization [redacted]")
        );
        assert_eq!(
            safe_inline_subject("curl --password hunter2 https://example.com").as_deref(),
            Some("curl --password [redacted] https://example.com")
        );
        assert_eq!(
            safe_inline_subject("Bearer ghp_super-secret next").as_deref(),
            Some("Bearer [redacted] next")
        );
        assert_eq!(
            safe_inline_subject("curl --password\nhunter2 https://example.com").as_deref(),
            Some("curl --password [redacted] https://example.com")
        );
        assert_eq!(
            safe_inline_subject("Bearer\nghp_super-secret next").as_deref(),
            Some("Bearer [redacted] next")
        );
        assert_eq!(
            safe_inline_subject("AWS_SECRET_ACCESS_KEY=super-secret command").as_deref(),
            Some("AWS_SECRET_ACCESS_KEY=[redacted]")
        );
        assert_eq!(
            safe_inline_subject("AWS_ACCESS_KEY_ID=AKIAEXAMPLE command").as_deref(),
            Some("AWS_ACCESS_KEY_ID=[redacted]")
        );
        assert_eq!(
            safe_inline_subject("password hunter2").as_deref(),
            Some("password [redacted]")
        );
    }

    #[test]
    fn tool_subject_is_single_line_and_terminal_safe() {
        let subject = tool_subject("web_search", "{\"query\":\"safe\\ntext\\u001b[2J\"}").unwrap();
        assert_eq!(subject, "safe text");
    }

    #[test]
    fn show_meme_is_a_silent_tool() {
        assert!(is_silent_tool("show_meme"));
        assert!(!is_silent_tool("search_meme"));
    }

    #[test]
    fn readable_tool_names_translate_known_tools_and_fallback_unknown() {
        for (name, english, chinese) in [
            ("deep_research", "Deep research", "深度研究"),
            ("read_file", "Read file", "读取文件"),
            ("check_issue", "Check issue", "检查问题"),
            ("check_os_info", "System information", "查看系统信息"),
            ("get_weather", "Weather", "天气查询"),
            ("get_exchange_rate", "Exchange rates", "汇率查询"),
            ("draw_zhouyi_hexagram", "Draw I Ching hexagram", "周易起卦"),
            ("draw_tarot_card", "Draw tarot card", "抽塔罗牌"),
            ("draw_fortune_lot", "Draw fortune", "吉凶占"),
            ("vision_analyze", "Analyze image", "分析图片"),
            ("search_meme", "Search memes", "搜索表情包"),
            ("show_meme", "Send meme", "发送表情"),
            ("add_meme", "Add meme", "添加表情包"),
            ("task", "Subagent", "子代理"),
            (
                "upload_text_to_knowledge_base",
                "Import knowledge base",
                "导入知识库",
            ),
            (
                "search_evicted_context",
                "Search old context",
                "搜索旧上下文",
            ),
            ("recall_past_events", "Recall past events", "回忆往事"),
            ("aur_check_status", "Check AUR status", "查询 AUR 状态"),
            ("online_man_search", "Search online manuals", "搜索在线手册"),
            ("online_man_get_page", "Read online manual", "读取在线手册"),
            (
                "fcitx5_input_method_wiki_qurey",
                "Query Fcitx5 Wiki",
                "查询 Fcitx5 Wiki",
            ),
            ("install_aur_package", "Install AUR package", "安装 AUR 包"),
            (
                "search_knowledge_base_by_name",
                "Search knowledge base by name",
                "按名称搜索知识库",
            ),
            ("recall_memories", "Recall memories", "召回记忆"),
        ] {
            assert_eq!(readable_tool_name(name), t(english, chinese), "{name}");
        }
        assert_eq!(readable_tool_name("custom_skill"), "custom_skill");
    }

    #[test]
    fn summary_styles_distinguish_reasoning_from_tools() {
        assert_eq!(
            style_summary_text("工具", SummaryStyle::Tool),
            "\x1b[2m工具\x1b[0m"
        );
        assert_eq!(
            style_summary_text("思考", SummaryStyle::Reasoning),
            "\x1b[38;5;10m思考\x1b[0m"
        );
    }

    #[test]
    fn ordinary_activity_summaries_have_one_blank_line_without_leading_gap() {
        let mut output = Vec::new();
        write_activity_summary(&mut output, "思考摘要", SummaryStyle::Reasoning).unwrap();
        write_activity_summary(&mut output, "~ 工具×1 ok", SummaryStyle::Tool).unwrap();
        let output = strip_ansi_for_test(&String::from_utf8(output).unwrap());

        assert_eq!(output, "思考摘要\n\n~ 工具×1 ok\n\n");
        assert!(!output.starts_with('\n'));
    }

    #[test]
    fn reasoning_summary_reserves_one_blank_line_before_subagent_activity() {
        let mut output = Vec::new();
        write_activity_summary(
            &mut output,
            "思考 · 59 词元 · 2.5s",
            SummaryStyle::Reasoning,
        )
        .unwrap();
        write!(output, "~ Linux 游戏兼容性调查×1 运行中").unwrap();
        let output = strip_ansi_for_test(&String::from_utf8(output).unwrap());

        assert_eq!(
            output,
            "思考 · 59 词元 · 2.5s\n\n~ Linux 游戏兼容性调查×1 运行中"
        );
    }

    #[test]
    fn external_cursor_control_suppresses_renderer_visibility_changes() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.use_external_cursor_control();

        renderer.hide_cursor().unwrap();
        assert!(!renderer.cursor_hidden);
        renderer.cursor_hidden = true;
        renderer.show_cursor().unwrap();
        assert!(renderer.cursor_hidden);
    }

    #[test]
    fn pending_summary_reasoning_does_not_add_a_leading_newline_on_finish() {
        assert!(!stream_needs_terminating_newline(
            Some(ChatStreamKind::Reasoning),
            ReasoningDisplayMode::Summary,
        ));
        assert!(stream_needs_terminating_newline(
            Some(ChatStreamKind::Reasoning),
            ReasoningDisplayMode::Full,
        ));
        assert!(stream_needs_terminating_newline(
            Some(ChatStreamKind::Content),
            ReasoningDisplayMode::Summary,
        ));
    }

    #[test]
    fn finish_keeps_pending_reasoning_summary_state() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.reasoning_title = Some("检查摘要状态".to_string());
        renderer.reasoning_text = "some reasoning".to_string();
        renderer.reasoning_started_at = Some(std::time::Instant::now());
        renderer.finish().unwrap();
        assert!(renderer.reasoning_text.is_empty());
        assert!(renderer.reasoning_title.is_none());
        assert!(renderer.reasoning_started_at.is_none());
        assert!(!renderer.summary_line_active);
    }

    #[test]
    fn reasoning_summary_counts_tokens_and_uses_title() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.record_reasoning_text("one\nt");
        renderer.record_reasoning_text("wo\nthree");
        renderer.reasoning_title = Some("分析摘要协议".to_string());
        // 词元数按 chunk 增量累加(避免每 chunk 对全文 O(n²) 重算),
        // 期望值即各 chunk 估算之和;跨 chunk 切词处与全文重算略有出入。
        let expected = crate::token_estimate::estimate_tokens("one\nt")
            + crate::token_estimate::estimate_tokens("wo\nthree");
        let summary = renderer.reasoning_summary_text();
        let title_separator = t(": ", "：");
        assert!(summary.starts_with(&format!(
            "{}{title_separator}分析摘要协议 · ",
            t("thinking", "思考")
        )));
        assert!(summary.contains(&format!("{expected} {}", t("tokens", "词元"))));
        assert!(!summary.contains("字符"));
        assert!(!summary.contains(" 行"));
    }

    #[test]
    fn reasoning_without_title_still_estimates_summary_tokens() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );
        renderer
            .start_reasoning_phase(std::time::Instant::now())
            .unwrap();
        renderer.record_reasoning_text("Plain summary content without a title.");

        let expected = crate::token_estimate::estimate_tokens(&renderer.reasoning_text);
        let live = renderer.waiting_phase_text();
        assert!(live.starts_with(&format!("{} · ", t("thinking", "思考"))));
        assert!(live.contains(&format!("{expected} {}", t("tokens", "词元"))));
    }

    #[test]
    fn reasoning_part_end_commits_state_and_starts_next_timer_at_boundary() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        let started_at = std::time::Instant::now();
        let ended_at = started_at + std::time::Duration::from_millis(750);
        renderer.start_reasoning_phase(started_at).unwrap();
        renderer.reasoning_title = Some("检查当前阶段".to_string());
        renderer.record_reasoning_text("summary body");

        renderer.finish_reasoning_part(ended_at).unwrap();

        assert!(renderer.reasoning_title.is_none());
        assert!(renderer.reasoning_text.is_empty());
        assert_eq!(renderer.reasoning_tokens, 0);
        assert_eq!(renderer.reasoning_started_at, Some(ended_at));
        assert!(renderer.reasoning_elapsed.is_none());
    }

    #[test]
    fn new_reasoning_part_starts_a_fresh_timer_and_estimate() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        let started_at = std::time::Instant::now();
        let next_part_at = started_at + std::time::Duration::from_millis(900);
        renderer.start_reasoning_phase(started_at).unwrap();
        renderer.reasoning_title = Some("上一阶段".to_string());
        renderer.record_reasoning_text("old body");

        renderer.start_reasoning_part(next_part_at).unwrap();

        assert!(renderer.reasoning_title.is_none());
        assert!(renderer.reasoning_text.is_empty());
        assert_eq!(renderer.reasoning_tokens, 0);
        assert_eq!(renderer.reasoning_started_at, Some(next_part_at));
        assert!(renderer.reasoning_elapsed.is_none());
    }

    #[test]
    fn frozen_reasoning_elapsed_ignores_renderer_processing_delay() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );
        let started_at = std::time::Instant::now() - std::time::Duration::from_secs(30);
        renderer.reasoning_started_at = Some(started_at);
        renderer.freeze_reasoning_elapsed_at(started_at + std::time::Duration::from_millis(1_500));
        renderer.reasoning_title = Some("检查事件排队".to_string());

        assert_eq!(
            renderer.reasoning_elapsed,
            Some(std::time::Duration::from_millis(1_500))
        );
        assert!(renderer.reasoning_summary_text().ends_with(" · 1.5s"));
    }

    #[test]
    fn reasoning_live_text_updates_title_tokens_and_precise_elapsed_time() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );
        renderer.reasoning_title = Some("The user is asking \"你确定\"".to_string());
        renderer.record_reasoning_text("Inspecting the current implementation.");
        renderer.reasoning_started_at =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(11_700));

        let expected = crate::token_estimate::estimate_tokens(&renderer.reasoning_text);
        let title_separator = t(": ", "：");
        assert_eq!(
            renderer.reasoning_live_text(),
            format!(
                "{}{title_separator}The user is asking \"你确定\" · {expected} {} · 11.7s",
                t("thinking", "思考"),
                t("tokens", "词元")
            )
        );
    }

    #[test]
    fn reasoning_title_is_not_truncated_at_forty_characters() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.live_summary = false;
        let title = "a".repeat(60);

        renderer.write_reasoning_title(&title).unwrap();

        assert_eq!(renderer.reasoning_title.as_deref(), Some(title.as_str()));
    }

    #[test]
    fn reasoning_elapsed_uses_milliseconds_then_decimal_seconds() {
        assert_eq!(format_reasoning_elapsed(std::time::Duration::ZERO), "<1ms");
        assert_eq!(
            format_reasoning_elapsed(std::time::Duration::from_nanos(1)),
            "<1ms"
        );
        assert_eq!(
            format_reasoning_elapsed(std::time::Duration::from_millis(38)),
            "38ms"
        );
        assert_eq!(
            format_reasoning_elapsed(std::time::Duration::from_millis(976)),
            "976ms"
        );
        assert_eq!(
            format_reasoning_elapsed(std::time::Duration::from_millis(11_700)),
            "11.7s"
        );
    }

    #[test]
    fn reasoning_phase_starts_as_neutral_waiting_without_content() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );

        renderer
            .start_reasoning_phase(
                std::time::Instant::now() - std::time::Duration::from_millis(1_200),
            )
            .unwrap();

        assert!(renderer.reasoning_title.is_none());
        assert_eq!(renderer.waiting_phase_text(), "1.2s");
        assert!(!renderer.waiting_phase_text().contains("思考"));
        assert!(!renderer.waiting_phase_text().contains("词元"));
    }

    #[test]
    fn preparing_question_phase_overrides_reasoning_timer_until_handoff() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );
        renderer.reasoning_started_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(30));
        renderer.preparing_question_started_at =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(1_200));

        let phase = renderer.waiting_phase_text();

        assert!(phase.starts_with(t("~ Preparing question · ", "~ 准备问题 · ")));
        assert!(phase.ends_with("1.2s"));
        renderer.prepare_for_external_output().unwrap();
        assert!(renderer.preparing_question_started_at.is_none());
    }

    #[test]
    fn tool_preparing_announces_every_slow_argument_tool() {
        let phase_for = |name: &str| {
            let mut renderer = StreamRenderer::new(
                ReasoningDisplayMode::Summary,
                ToolCallDisplayMode::Summary,
                false,
                true,
                10,
            );
            renderer.use_external_cursor_control();
            renderer.use_buffered_output();
            // No TTY under test, so the spinner degrades to a summary line —
            // which is gated on the same flag a real terminal would set.
            renderer.live_summary = true;
            renderer.write_tool_preparing(name).unwrap();
            String::from_utf8_lossy(&renderer.take_output_frame()).into_owned()
        };

        // apply_artifact_patch used to fall through the label match and render
        // nothing even though the backend announced it.
        for name in ["apply_patch", "apply_artifact_patch", "write_file"] {
            let phase = phase_for(name);
            assert!(
                phase.contains(t("~ Preparing edit", "~ 准备编辑")),
                "{name}"
            );
            // Dim tool palette, not the green the model's thinking uses: a
            // tool is starting up here.
            assert!(phase.contains("\x1b[2m"), "{name}");
            assert!(!phase.contains("\x1b[38;5;10m"), "{name}");
        }
        assert!(phase_for("run_command").contains(t("~ Preparing command", "~ 准备执行")));
        assert!(phase_for("trash_path").contains(t("~ Preparing delete", "~ 准备删除")));
        assert!(phase_for("read_file").is_empty());
    }

    /// Regression: the hint above is announced mid-turn, when a reasoning
    /// spinner is already up and earlier tools have filled `tool_stats`. Every
    /// tick re-derives the phase from renderer state, so pushing the text into
    /// the spinner was not enough — the tool summary overwrote it inside the
    /// very `tick_spinner` that `ensure_waiting_phase` performs, and the hint
    /// never reached the screen for anything except `ask_question` (which has
    /// its own sticky flag).
    #[test]
    fn tool_preparing_survives_the_tick_that_re_derives_the_phase() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        renderer.live_summary = true;
        // Mid-turn state: the model has been thinking and already ran a tool.
        renderer.reasoning_started_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(30));
        renderer.tool_stats_entry("read_file").calls += 1;

        renderer.write_tool_preparing("run_command").unwrap();
        assert!(renderer
            .waiting_phase_text()
            .starts_with(t("~ Preparing command · ", "~ 准备执行 · ")));
        renderer.last_tick = None;
        renderer.tick_spinner().unwrap();
        assert!(
            renderer
                .waiting_phase_text()
                .contains(t("Preparing command", "准备执行")),
            "a tick must not hand the spinner back to the tool summary"
        );

        // The arguments arrived: the hint steps aside for the tool summary.
        renderer.write_tool_call("run_command", "{}").unwrap();
        assert!(renderer.tool_preparing.is_none());
        assert!(!renderer
            .waiting_phase_text()
            .contains(t("Preparing command", "准备执行")));
    }

    #[test]
    fn buffered_output_returns_complete_frames_without_terminal_queries() {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Hidden,
            ToolCallDisplayMode::Hidden,
            true,
            true,
            10,
        );
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        renderer
            .write_chunk(ChatStreamChunk {
                kind: ChatStreamKind::Content,
                text: "hello".to_string(),
            })
            .unwrap();

        assert_eq!(renderer.take_output_frame(), b"hello");
        assert!(renderer.take_output_frame().is_empty());

        renderer.finish().unwrap();
        let frame = renderer.take_output_frame();
        assert_eq!(frame, b"\n");
        assert!(!frame.windows(5).any(|bytes| bytes == b"?2026"));
        assert!(!frame.windows(3).any(|bytes| bytes == b"[6n"));
    }

    #[test]
    fn full_reasoning_waiting_phase_is_empty() {
        let renderer = StreamRenderer::new(
            ReasoningDisplayMode::Full,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );

        assert!(renderer.waiting_phase_text().is_empty());
    }

    #[test]
    fn keeps_identifier_underscores_literal() {
        let output = render_inline("GTK_IM_MODULE and _italic_");
        assert!(output.contains("GTK_IM_MODULE"));
        assert!(output.contains(&format!("{ITALIC_STYLE}italic{RESET}")));
        assert!(!output.contains("GTK\x1b[3mIM\x1b[0mMODULE"));
        assert_eq!(render_inline("abc_def_ghi"), "abc_def_ghi");
    }

    #[test]
    fn renders_math_formulas_visibly() {
        let output = render_inline("inline $E=mc^2$ and display $$a^2+b^2=c^2$$");
        assert!(output.contains("E=mc²"), "{output}");
        assert!(output.contains("a²+b²=c²"), "{output}");
        assert!(!output.contains("$E"), "raw tex must be replaced: {output}");
    }

    #[test]
    fn renders_multiline_math_blocks_visibly() {
        let mut renderer = MarkdownStreamRenderer::new();
        let output = renderer.push("$$\na^2 + b^2 = c^2\n$$\n");
        assert!(
            output.contains('▀') || output.contains('▄'),
            "block math should render halfblocks: {output}"
        );
        assert!(!output.contains("a^2"), "{output}");
    }

    #[test]
    fn renders_selected_inline_html_tags() {
        let output = render_inline("<u>under</u> H<sub>2</sub> x<sup>2</sup><br>next");
        assert!(output.contains("\x1b[4munder\x1b[0m"));
        assert!(output.contains("H\x1b[2m2\x1b[0m"));
        assert!(output.contains("x\x1b[1m2\x1b[0m"));
        assert!(output.contains("\nnext"));
    }

    #[test]
    fn horizontal_rule_uses_terminal_width_fallback() {
        let output = render_markdown_line("---");
        assert!(output.starts_with("\x1b[2m"));
        assert!(output.ends_with("\x1b[0m"));
        assert!(visible_width(&output) >= 16);
    }

    #[test]
    fn supports_table_alignment_markers() {
        let mut renderer = MarkdownStreamRenderer::new();
        let output =
            renderer.push("| left | mid | right |\n| :--- | :---: | ---: |\n| a | b | c |\n");
        let output = format!("{output}{}", renderer.flush());
        assert!(output.contains('┌'));
        assert!(output.contains('│'));
        assert!(!output.contains('+'));
        assert!(!output.contains(":---"));
        assert!(output.contains(&format!("{BOLD_STYLE}left{RESET}")));
    }

    #[test]
    fn does_not_buffer_plain_lines_with_pipes_as_tables() {
        let mut renderer = MarkdownStreamRenderer::new();
        let output = renderer.push("echo hi | wc -l\nnext\n");
        assert!(output.contains("echo hi | wc -l\nnext\n"));
    }

    #[test]
    fn parses_command_result_json() {
        let result = parse_command_result(
            r#"{"success":false,"exit_code":1,"stdout":"unused","stderr":"not found"}"#,
        )
        .unwrap();
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(1));
        assert_eq!(result.stdout, "unused");
        assert_eq!(result.stderr, "not found");
    }
}

#[cfg(test)]
mod math_stream_tests {
    use super::*;

    fn render_document(document: &str) -> String {
        let mut renderer = MarkdownLineRenderer::new();
        let mut output = String::new();
        for line in document.lines() {
            output.push_str(&renderer.render_line(line));
        }
        output.push_str(&renderer.flush());
        output
    }

    #[test]
    fn block_math_renders_to_halfblocks_and_inline_transliterates() {
        let document = "推导如下:\n$$\nE = mc^2\n$$\n其中 $\\alpha\\in(0,1)$,价格 $5 和 $10 不动。\n";
        let output = render_document(document);
        assert!(output.contains('▀') || output.contains('▄'), "block math should render halfblocks");
        assert!(output.contains("α∈(0,1)"), "inline math should transliterate: {output}");
        assert!(output.contains("$5"), "prices must stay literal");
        assert!(!output.contains("mc^2"), "raw tex should be replaced");
    }

    #[test]
    fn table_cells_render_stacked_fractions() {
        let document = "| 方法 | 收敛阶 |\n| --- | --- |\n| 牛顿法 | $q=2$ |\n| 割线法 | $q=\\frac{1+\\sqrt5}{2}$ |\n\n";
        let output = render_document(document);
        assert!(output.contains("q=2"), "{output}");
        assert!(output.contains("1+√5"), "分子应独立成行: {output}");
        assert!(output.contains("───"), "分数线应存在: {output}");
        assert!(!output.contains("\\frac"), "{output}");
    }

    #[test]
    fn unclosed_math_block_replays_verbatim_on_flush() {
        let output = render_document("$$\nE=mc^2\n");
        assert!(output.contains("$$"));
        assert!(output.contains("E=mc^2"));
    }

    #[test]
    fn single_line_display_math_renders() {
        let output = render_document("$$E=mc^2$$\n");
        assert!(output.contains('▀') || output.contains('▄'), "{output}");
    }

    /// 检视产物:整段 markdown 渲染输出落盘,供 ANSI→PNG 回显人工核看。
    #[test]
    #[ignore]
    fn dump_stream_preview() {
        let document = "偏导与分式:\n\n| 名称 | 表达式 |\n| --- | --- |\n| 偏导数 | $\\frac{\\partial f}{\\partial x}=\\lim_{h\\to 0}\\frac{f(x+h,y)-f(x,y)}{h}$ |\n| 二次方程 | $x=\\frac{-b\\pm\\sqrt{b^2-4ac}}{2a}$ |\n| 组合数 | $\\binom{n}{k}=\\frac{n!}{k!(n-k)!}$ |\n| 波函数 | $i\\hbar\\frac{\\partial}{\\partial t}\\Psi=\\hat{H}\\Psi$ |\n| 极限 | $\\lim_{x\\to\\infty}(1+1/x)^x=e$ |\n\n完事～\n";
        let output = render_document(document);
        std::fs::write("/tmp/claude-1000/math-stream.ansi", output).unwrap();
    }
}
