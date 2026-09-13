//! ANSI → ratatui span。
//!
//! 现有渲染器（markdown / code / table / patch / command / tool_summary）产出的
//! 是带 SGR 的字符串，全屏 TUI 要的是「文本 + 样式」的序列。与其把 `render/`
//! 那 5000 行改成直接产 span（还要连带重写 ~2900 行 ANSI 字节断言），不如在
//! 它们的输出上接这一层：渲染器一行不动，公式的三层回退、patch 的行背景色、
//! 命令块的 `$ ↳ │ └` 词汇全部零成本继承，inline REPL 也不受影响。
//!
//! **只认三样**：SGR（`CSI m`）、OSC 8 超链接、换行。渲染器的行输出里不会有
//! 光标控制序列——live 路径靠 `use_external_cursor_control()` 把光标交给活动区，
//! 帧里只剩颜色和文本（`render/tests/` 的断言就是证据）。真要混进来了，
//! 这里按「跳过、但不吃掉文本」处理，最坏情况是掉样式而不是乱码。
//!
//! 图形协议（kitty 图片、公式的图片回退）**不走这里**：它们是整块的 APC，
//! 由 [`super::image`] 单独发给终端，行模型里留的是占位符格子。

use ratatui::style::{Color, Modifier, Style};
use vte::{Params, Parser, Perform};

/// 一段同样式的文本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) struct AnsiSpan {
    pub(in crate::cli) text: String,
    pub(in crate::cli) style: Style,
    /// OSC 8 的目标地址。终端超链接不是 SGR，装不进 `Style`，只能挂在 span 上。
    pub(in crate::cli) link: Option<String>,
}

impl AnsiSpan {
    pub(in crate::cli) fn raw(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::new(),
            link: None,
        }
    }

    pub(in crate::cli) fn styled(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            link: None,
        }
    }
}

/// 把一段可能含 SGR 的文本解析成逐行的 span。
///
/// 样式状态跨行保持：渲染器基本上每行自己 reset，但表格和代码块偶尔跨行，
/// 按终端的真实行为来才不会在折行处掉色。
pub(in crate::cli) fn parse_ansi(input: &str) -> Vec<Vec<AnsiSpan>> {
    let mut parser = Parser::new();
    let mut sink = AnsiSink::default();
    parser.advance(&mut sink, input.as_bytes());
    sink.finish()
}

/// 单行版本：输入保证不含换行时用，省一次 `Vec` 包装。
pub(in crate::cli) fn parse_ansi_line(input: &str) -> Vec<AnsiSpan> {
    parse_ansi(input).into_iter().next().unwrap_or_default()
}

/// 丢掉样式，只取文本——选区复制和宽度计算用。
pub(in crate::cli) fn spans_text(spans: &[AnsiSpan]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

/// span 画回 ANSI。
///
/// 解析的反向。全屏后端和 inline 一样是「自己拼字节写终端」，不引入第二套
/// 绘制体系——中间之所以要过一趟 span，是因为选区反显、折行、点击都得按
/// **显示列**操作，在裸 ANSI 串上做这些事没法收场。
pub(in crate::cli) fn spans_to_ansi(spans: &[AnsiSpan]) -> String {
    let mut out = String::new();
    let mut current = Style::new();
    let mut link: Option<&str> = None;
    for span in spans {
        if span.style != current {
            out.push_str(&style_to_ansi(span.style));
            current = span.style;
        }
        // 链接跟着格子走：缓冲里存着 OSC 8 的目标，画的时候得再发出去，终端
        // 才认得这是个链接（悬停有下划线、自己那套点开也能用）。原来这一层把它
        // 丢了——全屏里链接看着是普通文字（用户实测：真 TUI 链接无渲染）。
        if span.link.as_deref() != link {
            if link.is_some() {
                out.push_str("\x1b]8;;\x1b\\");
            }
            if let Some(url) = span.link.as_deref() {
                out.push_str("\x1b]8;;");
                out.push_str(url);
                out.push_str("\x1b\\");
            }
            link = span.link.as_deref();
        }
        out.push_str(&span.text);
    }
    if link.is_some() {
        out.push_str("\x1b]8;;\x1b\\");
    }
    if current != Style::new() {
        out.push_str("\x1b[0m");
    }
    out
}

/// 一段样式的 SGR。每次都从 reset 开始——省几个字节不值得让状态机出错。
fn style_to_ansi(style: Style) -> String {
    let mut out = String::from("\x1b[0m");
    for (modifier, code) in [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::SLOW_BLINK, "5"),
        (Modifier::REVERSED, "7"),
        (Modifier::HIDDEN, "8"),
        (Modifier::CROSSED_OUT, "9"),
    ] {
        if style.add_modifier.contains(modifier) {
            out.push_str("\x1b[");
            out.push_str(code);
            out.push('m');
        }
    }
    if let Some(color) = style.fg {
        out.push_str(&color_to_ansi(color, true));
    }
    if let Some(color) = style.bg {
        out.push_str(&color_to_ansi(color, false));
    }
    out
}

fn color_to_ansi(color: Color, foreground: bool) -> String {
    let base = if foreground { 30 } else { 40 };
    let bright = if foreground { 90 } else { 100 };
    let extended = if foreground { 38 } else { 48 };
    match color {
        Color::Reset => format!("\x1b[{}m", base + 9),
        Color::Black => format!("\x1b[{}m", base),
        Color::Red => format!("\x1b[{}m", base + 1),
        Color::Green => format!("\x1b[{}m", base + 2),
        Color::Yellow => format!("\x1b[{}m", base + 3),
        Color::Blue => format!("\x1b[{}m", base + 4),
        Color::Magenta => format!("\x1b[{}m", base + 5),
        Color::Cyan => format!("\x1b[{}m", base + 6),
        Color::Gray => format!("\x1b[{}m", base + 7),
        Color::DarkGray => format!("\x1b[{}m", bright),
        Color::LightRed => format!("\x1b[{}m", bright + 1),
        Color::LightGreen => format!("\x1b[{}m", bright + 2),
        Color::LightYellow => format!("\x1b[{}m", bright + 3),
        Color::LightBlue => format!("\x1b[{}m", bright + 4),
        Color::LightMagenta => format!("\x1b[{}m", bright + 5),
        Color::LightCyan => format!("\x1b[{}m", bright + 6),
        Color::White => format!("\x1b[{}m", bright + 7),
        Color::Indexed(index) => format!("\x1b[{extended};5;{index}m"),
        Color::Rgb(red, green, blue) => format!("\x1b[{extended};2;{red};{green};{blue}m"),
    }
}

#[derive(Default)]
struct AnsiSink {
    lines: Vec<Vec<AnsiSpan>>,
    current: Vec<AnsiSpan>,
    pending: String,
    sgr: SgrState,
    link: Option<String>,
}

/// SGR 状态。直接存三件套而不是滚一个 `Style`，因为 22/23/24/27/29 这些
/// 「取消某一项」的参数在 `Style` 上得用 `remove_modifier`，分开存更直白。
#[derive(Clone, Default, PartialEq, Eq)]
struct SgrState {
    fg: Option<Color>,
    bg: Option<Color>,
    modifiers: Modifier,
}

impl SgrState {
    fn style(&self) -> Style {
        let mut style = Style::new().add_modifier(self.modifiers);
        if let Some(fg) = self.fg {
            style = style.fg(fg);
        }
        if let Some(bg) = self.bg {
            style = style.bg(bg);
        }
        style
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    fn apply(&mut self, params: &Params) {
        // `CSI m` 不带参数等同 `CSI 0 m`。
        if params.is_empty() {
            self.reset();
            return;
        }
        let flat: Vec<&[u16]> = params.iter().collect();
        let mut index = 0;
        while index < flat.len() {
            let param = flat[index];
            let Some(&code) = param.first() else {
                index += 1;
                continue;
            };
            match code {
                0 => self.reset(),
                1 => self.modifiers.insert(Modifier::BOLD),
                2 => self.modifiers.insert(Modifier::DIM),
                3 => self.modifiers.insert(Modifier::ITALIC),
                4 => self.modifiers.insert(Modifier::UNDERLINED),
                5 | 6 => self.modifiers.insert(Modifier::SLOW_BLINK),
                7 => self.modifiers.insert(Modifier::REVERSED),
                8 => self.modifiers.insert(Modifier::HIDDEN),
                9 => self.modifiers.insert(Modifier::CROSSED_OUT),
                // 22 一次取消粗体和淡色——它们共用一个「强度」维度。
                22 => self.modifiers.remove(Modifier::BOLD | Modifier::DIM),
                23 => self.modifiers.remove(Modifier::ITALIC),
                24 => self.modifiers.remove(Modifier::UNDERLINED),
                25 => self.modifiers.remove(Modifier::SLOW_BLINK),
                27 => self.modifiers.remove(Modifier::REVERSED),
                28 => self.modifiers.remove(Modifier::HIDDEN),
                29 => self.modifiers.remove(Modifier::CROSSED_OUT),
                30..=37 => self.fg = Some(basic_color(code - 30)),
                38 => {
                    let (color, consumed) = extended_color(&flat, index, param);
                    self.fg = color;
                    index += consumed;
                }
                39 => self.fg = None,
                40..=47 => self.bg = Some(basic_color(code - 40)),
                48 => {
                    let (color, consumed) = extended_color(&flat, index, param);
                    self.bg = color;
                    index += consumed;
                }
                49 => self.bg = None,
                90..=97 => self.fg = Some(bright_color(code - 90)),
                100..=107 => self.bg = Some(bright_color(code - 100)),
                _ => {}
            }
            index += 1;
        }
    }
}

/// `38` / `48` 后面的扩展色。两种写法都要认：
/// 分号 `38;5;189`（渲染器现在发的）和冒号 `38:5:189`（子参数形式）。
/// 返回颜色和「额外吃掉了几个分号参数」。
fn extended_color(flat: &[&[u16]], index: usize, param: &[u16]) -> (Option<Color>, usize) {
    // 冒号形式：子参数就在同一个 param 里。
    if param.len() > 1 {
        return (color_from_parts(&param[1..]), 0);
    }
    let rest: Vec<u16> = flat[index + 1..]
        .iter()
        .filter_map(|values| values.first().copied())
        .collect();
    match rest.first() {
        Some(5) => (color_from_parts(&rest[..2.min(rest.len())]), 2),
        Some(2) => (color_from_parts(&rest[..4.min(rest.len())]), 4),
        _ => (None, 0),
    }
}

fn color_from_parts(parts: &[u16]) -> Option<Color> {
    match parts.first()? {
        5 => parts.get(1).map(|index| Color::Indexed(*index as u8)),
        2 => match (parts.get(1), parts.get(2), parts.get(3)) {
            (Some(r), Some(g), Some(b)) => Some(Color::Rgb(*r as u8, *g as u8, *b as u8)),
            _ => None,
        },
        _ => None,
    }
}

fn basic_color(offset: u16) -> Color {
    match offset {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        _ => Color::Gray,
    }
}

fn bright_color(offset: u16) -> Color {
    match offset {
        0 => Color::DarkGray,
        1 => Color::LightRed,
        2 => Color::LightGreen,
        3 => Color::LightYellow,
        4 => Color::LightBlue,
        5 => Color::LightMagenta,
        6 => Color::LightCyan,
        _ => Color::White,
    }
}

impl AnsiSink {
    fn flush_text(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending);
        self.current.push(AnsiSpan {
            text,
            style: self.sgr.style(),
            link: self.link.clone(),
        });
    }

    fn newline(&mut self) {
        self.flush_text();
        self.lines.push(std::mem::take(&mut self.current));
    }

    fn finish(mut self) -> Vec<Vec<AnsiSpan>> {
        self.flush_text();
        // 末尾没有换行时也要把最后一行交出去；有换行时 current 是空的，
        // 补一个空行反而会在每段文本后多出一行。
        if !self.current.is_empty() || self.lines.is_empty() {
            self.lines.push(std::mem::take(&mut self.current));
        }
        self.lines
    }
}

impl Perform for AnsiSink {
    fn print(&mut self, character: char) {
        self.pending.push(character);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.newline(),
            // 渲染器的行输出里不该有回车；真混进来就当它没发生，
            // 按「覆盖本行」处理会把已经攒好的 span 丢掉，代价更大。
            b'\r' => {}
            b'\t' => {
                self.pending.push('\t');
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        if action == 'm' {
            self.flush_text();
            self.sgr.apply(params);
        }
        // 其余 CSI（光标移动、清行）在行渲染里不该出现，跳过即可。
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // OSC 8 ; params ; URI —— URI 为空表示链接结束。
        let Some(b"8") = params.first().copied() else {
            return;
        };
        self.flush_text();
        self.link = params
            .get(2)
            .filter(|uri| !uri.is_empty())
            .map(|uri| String::from_utf8_lossy(uri).into_owned());
    }
}
