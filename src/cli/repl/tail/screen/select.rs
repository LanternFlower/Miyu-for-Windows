//! 自绘选区：拖选、反显、复制。
//!
//! 全屏下终端的原生拖选只能选到画面上的字符——包括左边那根 `┃`。程序自己
//! 接管之后，反显与复制都按**可复制列**来算，装饰不进剪贴板。
//!
//! 选区坐标用的是「缓冲绝对行 + 显示列」而不是屏幕坐标：视口一滚，屏幕行
//! 就变了，绝对行不会。

use super::ansi::{spans_text, AnsiSpan};
use super::Screen;
use ratatui::style::Modifier;
use unicode_width::UnicodeWidthChar;

/// 一次拖选。谁先谁后不管，取文本时会排序。
#[derive(Clone, Copy)]
pub(in crate::cli) struct Selection {
    pub(in crate::cli) anchor: (usize, u16),
    pub(in crate::cli) cursor: (usize, u16),
    pub(in crate::cli) dragging: bool,
}

impl Selection {
    pub(in crate::cli) fn ordered(self) -> ((usize, u16), (usize, u16)) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }
}

/// 这一行左边有多少列是装饰。
///
/// 用户消息和输入区都由 `┃ ` 开头（`layout.rs` 的 `input_prompt_bar`），
/// 那两列是画给人看的，不该进剪贴板。
fn decoration_width(spans: &[AnsiSpan]) -> u16 {
    if spans
        .first()
        .is_some_and(|first| first.text.starts_with('┃'))
    {
        return 2;
    }
    // 左边那两格是**页边距**（正文、时间线共用的装订边），不是内容。
    // 让它进选区的话，复制出来的每一行都带着两个莫名其妙的空格。
    let leading = spans_text(spans)
        .chars()
        .take_while(|ch| *ch == ' ')
        .count()
        .min(usize::from(MARGIN));
    u16::try_from(leading).unwrap_or(0)
}

/// 按**显示列**切一行：只留 `[from, to]` 这一段，`skip` 之前的列当页边距丢掉。
///
/// 正文选区和输入框选区共用它——两边都是「屏幕上看到哪几列就复制哪几列」，
/// 差别只在文字从哪儿取。
pub(in crate::cli) fn slice_columns(spans: &[AnsiSpan], from: u16, to: u16, skip: u16) -> String {
    let from = from.max(skip);
    let mut line = String::new();
    let mut column = 0u16;
    for ch in spans_text(spans).chars() {
        let width = u16::try_from(ch.width().unwrap_or(0)).unwrap_or(0);
        if column >= from && column <= to {
            line.push(ch);
        }
        column = column.saturating_add(width);
    }
    line.trim_end().to_string()
}

/// 这一行左边有多少列是装饰/页边距。
pub(in crate::cli) fn decoration_of(spans: &[AnsiSpan]) -> u16 {
    decoration_width(spans)
}

/// 展开区的底色。比终端背景深一档——用 256 色的近黑灰，深浅主题下都还看得出
/// 层次，又不至于像另开了一个控件。
const EXPANSION_BG: ratatui::style::Color = ratatui::style::Color::Indexed(236);

fn char_columns(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

/// 页边距宽度。和 `render/stream/timeline.rs` 的 `INDENT` 是同一件事。
const MARGIN: u16 = 2;

/// 给一行铺上"这是展开出来的一块"的暗底，右边补到 `width` 列。
///
/// 正文和覆盖层都用它——两处都得是同一块底色，不然点开的东西在面板里和在正文里
/// 长得不一样。
pub(in crate::cli) fn paint_expansion_bg(spans: Vec<AnsiSpan>, width: usize) -> Vec<AnsiSpan> {
    let used: usize = spans_text(&spans).chars().map(char_columns).sum();
    let mut out: Vec<AnsiSpan> = spans
        .into_iter()
        .map(|span| AnsiSpan {
            // 自己**已经有底色**的留着：diff 的 `+`/`-` 就是靠那两条底色带
            // 认出来的，一律盖成展开区的暗底等于把 diff 洗成一片灰
            //（用户实测：diff 颜色需要优化）。
            style: match span.style.bg {
                Some(_) => span.style,
                None => span.style.bg(EXPANSION_BG),
            },
            ..span
        })
        .collect();
    if used < width {
        out.push(AnsiSpan {
            text: " ".repeat(width - used),
            style: ratatui::style::Style::new().bg(EXPANSION_BG),
            link: None,
        });
    }
    out
}

/// 点在哪个链接上。
///
/// 全屏把鼠标捕获走了，终端自己那套"点链接"就失效了——链接看着是链接，点了
/// 没反应。所以得自己认：按显示宽度走到点击那一列，看它落在哪个 `http(s)://`
/// 串里。
///
/// 认的是**文本**而不是 OSC 8 的目标：缓冲里一格只存一个字符和样式，没地方
/// 挂链接；而正文里的裸链接（工具输出、日志）本来就没有 OSC 8，按文本认反而
/// 两种都能点。
pub(in crate::cli) fn url_at(spans: &[AnsiSpan], column: u16) -> Option<String> {
    let target = usize::from(column);
    // 先问 OSC 8：markdown 链接在屏幕上只露一个标题（「点这里」），目标藏在转义
    // 序列里，按文本根本认不出来。
    let mut width = 0usize;
    for span in spans {
        let span_width: usize = span.text.chars().map(char_columns).sum();
        if target < width + span_width {
            if let Some(link) = &span.link {
                // `file://` 也认:图表下面那行「点开看大图」指的就是缓存里那张
                // SVG,只放 http 的话点了没反应(用户 09-20 实测)。
                if link.starts_with("http://")
                    || link.starts_with("https://")
                    || link.starts_with("file://")
                {
                    return Some(link.clone());
                }
            }
            break;
        }
        width += span_width;
    }
    let text: String = spans.iter().map(|span| span.text.as_str()).collect();
    let mut width = 0usize;
    let mut hit: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        let next = width + char_columns(ch);
        if target < next {
            hit = Some(index);
            break;
        }
        width = next;
    }
    let hit = hit?;
    // 往左找到这一串的开头，往右找到结尾——链接里不会有空白，也不会有引号。
    let boundary = |ch: char| {
        ch.is_whitespace() || matches!(ch, '"' | '\'' | '(' | ')' | '<' | '>' | '`' | '｜' | '|')
    };
    let start = text[..hit]
        .char_indices()
        .rev()
        .take_while(|(_, ch)| !boundary(*ch))
        .last()
        .map_or(hit, |(index, _)| index);
    let end = text[hit..]
        .char_indices()
        .find(|(_, ch)| boundary(*ch))
        .map_or(text.len(), |(index, _)| hit + index);
    let token = text[start..end].trim_end_matches(['.', ',', '，', '。', ';', '；', ':', '：']);
    (token.starts_with("http://") || token.starts_with("https://")).then(|| token.to_string())
}

/// 交给桌面去开。开不了就算了——为了一个链接把整个界面弄崩没道理。
/// 交给系统去开。
///
/// **`xdg-open` 是 freedesktop 的东西,macOS 上根本没有**——原来写死它,于是
/// mac 上全屏里点任何链接都悄无声息(09-20 发现)。按平台取:macOS 用 `open`,
/// 其余用 `xdg-open`。
///
/// 只在**全屏**下需要走这条:全屏把鼠标捕获走了,终端自己那套「点链接」失效。
/// inline 那边链接照样是终端在管,轮不到我们开进程。
fn opener() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

pub(in crate::cli) fn open_url(url: &str) {
    let _ = std::process::Command::new(opener())
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

impl Screen {
    /// 屏幕行 → 缓冲绝对行。
    ///
    /// 活动区那几行、以及内容不满一屏时上面垫出来的空行，都不在缓冲里，
    /// 返回 `None`——点在空白上不该选中任何东西。
    pub(in crate::cli) fn body_index(&self, screen_row: u16, body: u16) -> Option<usize> {
        if screen_row >= body {
            return None;
        }
        usize::from(screen_row)
            .checked_sub(self.top_pad())
            .map(|offset| self.scroll + offset)
    }

    fn absolute_row(&self, screen_row: u16, body: u16) -> Option<usize> {
        self.body_index(screen_row, body)
    }

    pub(in crate::cli) fn selection_begin(&mut self, column: u16, row: u16, body: u16) {
        let Some(absolute) = self.absolute_row(row, body) else {
            self.selection = None;
            return;
        };
        self.selection = Some(Selection {
            anchor: (absolute, column),
            cursor: (absolute, column),
            dragging: true,
        });
        self.invalidate();
    }

    pub(in crate::cli) fn selection_extend(&mut self, column: u16, row: u16, body: u16) {
        // 拖到活动区上就钉在正文最后一行，别让选区断掉。
        let absolute = self
            .absolute_row(row, body)
            .or_else(|| self.absolute_row(body.saturating_sub(1), body))
            .unwrap_or(self.scroll);
        if let Some(selection) = &mut self.selection {
            if selection.dragging {
                selection.cursor = (absolute, column);
                // **不** invalidate：反显只改那几行的内容，逐行 diff 自己就能
                // 认出来。整份缓存丢掉的话，拖一下就要重排整屏——AI 正在快速
                // 输出时两边叠在一起，手上就是明显的迟滞。
            }
        }
    }

    /// 松手。返回 `Some(行号)` 表示这是**原地点一下**而不是拖选——调用方
    /// 拿它去做点击该做的事（展开块）。拖过了就只管复制，返回 `None`。
    pub(in crate::cli) fn selection_finish(&mut self) -> Option<usize> {
        let mut selection = self.selection?;
        selection.dragging = false;
        if selection.anchor == selection.cursor {
            self.selection = None;
            self.invalidate();
            return Some(selection.anchor.0);
        }
        self.selection = Some(selection);
        let text = self.selection_text(selection);
        if std::env::var_os("MIYU_SCREEN_TRACE").is_some() {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/miyu-screen-trace.log")
            {
                let _ = writeln!(
                    f,
                    "select {:?}..{:?} text={text:?}",
                    selection.anchor, selection.cursor
                );
            }
        }
        if !text.trim().is_empty() {
            self.pending_copy = Some(text);
        }
        self.invalidate();
        None
    }

    pub(in crate::cli) fn selection_clear(&mut self) -> bool {
        if self.selection.take().is_some() {
            self.invalidate();
            return true;
        }
        false
    }

    /// 选中的文本。逐行按显示列切，跳过每行的装饰列。
    fn selection_text(&self, selection: Selection) -> String {
        let (start, end) = selection.ordered();
        let mut out = Vec::new();
        for index in start.0..=end.0 {
            let spans = self.view_row(index);
            if spans.is_empty() {
                out.push(String::new());
                continue;
            }
            let skip = decoration_width(&spans);
            let from = if index == start.0 {
                start.1.max(skip)
            } else {
                skip
            };
            let to = if index == end.0 { end.1 } else { u16::MAX };
            out.push(slice_columns(&spans, from, to, 0));
        }
        out.join("\n")
    }

    /// 展开出来的那一片铺一层暗底。
    ///
    /// 展开的内容和正文混在同一张画布上，光靠缩进分不清哪儿到哪儿是"点开看的"。
    /// 铺一层比背景略深的底色，那一片就成了一个可以整体收起来的面。整行铺满
    /// （补到屏宽），断在半路的底色比没有底色更难看。
    pub(in crate::cli) fn expansion_paint(
        &self,
        index: usize,
        spans: Vec<AnsiSpan>,
    ) -> Vec<AnsiSpan> {
        if !self.in_expansion(index) {
            return spans;
        }
        // 底色也守着右边距：一路铺到屏幕最后一列的话，这一块看着像是被切掉了
        // 半边，而左边还老老实实留着两格——两边不对称最扎眼。
        let width = usize::from(self.cols()).saturating_sub(usize::from(MARGIN));
        paint_expansion_bg(spans, width)
    }

    /// 鼠标悬浮在这一块上就把整块提亮：去掉 dim，可点的东西自己站出来。
    ///
    /// 只去 dim、不换颜色——时间线本来就是"想看再看"的附注，悬浮时让它回到
    /// 正常亮度已经足够醒目，再加底色就喧宾夺主了。
    pub(in crate::cli) fn hover_paint(&self, index: usize, spans: Vec<AnsiSpan>) -> Vec<AnsiSpan> {
        let Some(hovered) = self.hovered() else {
            return spans;
        };
        if self.block_at(index).map(|(id, _)| id) != Some(hovered) {
            return spans;
        }
        spans
            .into_iter()
            .map(|span| AnsiSpan {
                style: span.style.remove_modifier(Modifier::DIM),
                ..span
            })
            .collect()
    }

    /// 给一行加上选区反显。`index` 是缓冲绝对行。
    ///
    /// 只反显可复制的那几列——左边的装饰亮起来会让人以为竖条也复制进去了。
    pub(in crate::cli) fn highlight(&self, index: usize, spans: Vec<AnsiSpan>) -> Vec<AnsiSpan> {
        let Some(selection) = self.selection else {
            return spans;
        };
        let (start, end) = selection.ordered();
        if index < start.0 || index > end.0 || spans.is_empty() {
            return spans;
        }
        let skip = decoration_width(&spans);
        let from = if index == start.0 {
            start.1.max(skip)
        } else {
            skip
        };
        let to = if index == end.0 { end.1 } else { u16::MAX };

        highlight_columns(spans, from, to)
    }
}

/// 把 `[from, to]` 这几列反显。正文选区和输入框选区共用。
pub(in crate::cli) fn highlight_columns(spans: Vec<AnsiSpan>, from: u16, to: u16) -> Vec<AnsiSpan> {
    let mut out: Vec<AnsiSpan> = Vec::new();
    let mut column = 0u16;
    for span in spans {
        let mut chunk = String::new();
        let mut chunk_selected = None;
        for ch in span.text.chars() {
            let width = u16::try_from(ch.width().unwrap_or(0)).unwrap_or(0);
            let selected = column >= from && column <= to;
            if chunk_selected != Some(selected) && !chunk.is_empty() {
                push_chunk(
                    &mut out,
                    &mut chunk,
                    span.style,
                    chunk_selected == Some(true),
                );
            }
            chunk_selected = Some(selected);
            chunk.push(ch);
            column = column.saturating_add(width);
        }
        if !chunk.is_empty() {
            push_chunk(
                &mut out,
                &mut chunk,
                span.style,
                chunk_selected == Some(true),
            );
        }
    }
    out
}

fn push_chunk(
    out: &mut Vec<AnsiSpan>,
    chunk: &mut String,
    style: ratatui::style::Style,
    selected: bool,
) {
    let style = if selected {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    };
    out.push(AnsiSpan {
        text: std::mem::take(chunk),
        style,
        link: None,
    });
}
