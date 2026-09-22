//! 终端文本原语:转义序列的识别、显示宽度的丈量、命令输出字节流的解码与过滤。
//!
//! 这些既不是渲染层的事也不是工具层的事,两边都要用:渲染层拿它们折行上色,
//! 工具层拿它们给工具调用做可读摘要(`tools::tool_display`)。原先住在
//! `render/{command,markdown,stream}.rs` 里,于是工具层要用就得反过来 use 渲染层。
//! 放在基础层,两边都往下依赖,方向一致(09-16)。
//!
//! `render` 那三处留了 `pub(crate) use crate::terminal::…` 的再导出,
//! `miyu_hosts::render::clip_to_display_width` 这类老路径一字未改。

use super::CommandOutputStream;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// 开头是不是一段转义序列？是就返回它的字节长度。
///
/// 认三类：CSI（`ESC [ … 终止字节`）、**字符串类**（OSC `ESC ]`、APC `ESC _`、
/// DCS `ESC P`、PM `ESC ^`、SOS `ESC X`，一律扫到 `BEL` 或 `ESC \` 为止）、
/// 以及其余「ESC + 一个字节」的短序列。
///
/// APC 必须认，而且必须整段认下来：kitty 的图片和公式走的就是 APC，载荷是
/// 几 KB 的 base64。少认一个字节的后果不是"宽度算偏一点"——是那几 KB 被当成
/// 正文去量宽度、去折行，折行插进去的换行把控制块劈成两半，终端报
/// `Malformed GraphicsCommand`，图整个不出来。
pub fn escape_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&0x1b) {
        return None;
    }
    match bytes.get(1) {
        Some(b'[') => {
            let mut index = 2;
            while index < bytes.len() && !(0x40..=0x7e).contains(&bytes[index]) {
                index += 1;
            }
            Some((index + 1).min(bytes.len()))
        }
        Some(b']' | b'_' | b'P' | b'^' | b'X') => {
            let mut index = 2;
            while index < bytes.len() {
                if bytes[index] == 0x07 {
                    return Some(index + 1);
                }
                if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                    return Some(index + 2);
                }
                index += 1;
            }
            Some(bytes.len())
        }
        Some(_) => Some(2),
        None => Some(1),
    }
}

/// 去掉所有转义序列，只留看得见的字。
///
/// 拿一行渲染好的东西当"一句话"用时要先过这儿——不然 SGR 和 OSC 会混进去，
/// 量宽度、截断、比较全是错的。
pub fn strip_ansi_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(len) = escape_len(rest) {
            rest = &rest[len..];
            continue;
        }
        let Some(ch) = rest.chars().next() else { break };
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// 显示宽度：跳过转义序列，其余按**字形簇**量（组合记号不该单独占一列）。
pub fn display_width_skipping_escapes(text: &str) -> usize {
    let mut width = 0usize;
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(len) = escape_len(rest) {
            rest = &rest[len..];
            continue;
        }
        let Some(grapheme) = rest.graphemes(true).next() else {
            break;
        };
        width += UnicodeWidthStr::width(grapheme);
        rest = &rest[grapheme.len()..];
    }
    width
}

/// 按**显示宽度**裁一行，转义序列不算宽度、也不会被从中间切断。
///
/// 以前是拿整串（连转义带控制字符）去量宽度的：一行带颜色的文字会被当成比它
/// 实际宽得多，于是提前截断；更糟的是切点可能落在转义序列中间，半个序列流到
/// 终端上就是乱码。全屏 TUI 把块标记（OSC）也放进了行里，这条必须是对的。
pub fn clip_to_display_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if display_width_skipping_escapes(text) <= max_width {
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
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(len) = escape_len(rest) {
            // 转义序列整段留下，一个字节都不切。
            output.push_str(&rest[..len]);
            rest = &rest[len..];
            continue;
        }
        let grapheme = rest.graphemes(true).next().unwrap_or("");
        if grapheme.is_empty() {
            break;
        }
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > content_width {
            break;
        }
        output.push_str(grapheme);
        width += grapheme_width;
        rest = &rest[grapheme.len()..];
    }
    output.push_str(ellipsis);
    // 裁掉的那一截里的转义序列要留下：颜色复位不留会把后面整行染色，块的
    // 结束标记不留会让那一块一直开到天边。它们不占宽度，补在省略号后面就是。
    while !rest.is_empty() {
        match escape_len(rest) {
            Some(len) => {
                output.push_str(&rest[..len]);
                rest = &rest[len..];
            }
            None => {
                let grapheme = rest.graphemes(true).next().unwrap_or("");
                if grapheme.is_empty() {
                    break;
                }
                rest = &rest[grapheme.len()..];
            }
        }
    }
    output
}

pub fn clip_progress_line(text: &str, max_chars: usize) -> String {
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

#[derive(Clone)]
pub struct CommandLogLine {
    pub stream: CommandOutputStream,
    pub text: String,
    pub sequence: u64,
}

#[derive(Default)]
pub struct CommandStreamState {
    pub(crate) utf8_pending: Vec<u8>,
    pub current: String,
    pub(crate) control: TerminalControlState,
    pub last_update: u64,
    pub current_sequence: Option<u64>,
    pub(crate) pending_cr: bool,
}

#[derive(Clone, Copy, Default)]
pub enum TerminalControlState {
    #[default]
    Text,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
}

impl CommandStreamState {
    pub fn push(&mut self, chunk: &[u8], sequence: u64) -> Vec<CommandLogLine> {
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

    pub fn finalize_pending(&mut self, sequence: u64) {
        if !self.utf8_pending.is_empty() {
            self.utf8_pending.clear();
            self.current_sequence.get_or_insert(sequence);
            self.current.push('\u{fffd}');
        }
        self.pending_cr = false;
        self.control = TerminalControlState::Text;
    }
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

pub fn sanitize_terminal_char(state: &mut TerminalControlState, ch: char) -> Option<char> {
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

pub fn sanitize_terminal_text(text: &str) -> String {
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
