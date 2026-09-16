//! 引导画面的版式工具与小部件：按显示宽度补齐/裁切/折行，单选行、动作行、
//! 文本框、分割线。都是 ratatui `Line`/`Span` 级别的东西，不碰终端。
//!
//! 版式规矩（来自 demo 的教训）：
//! - **列宽一律按显示宽度算**。`format!("{:<n}")` 按字符数补，中文会推歪。
//! - **光标和选中态分开画**：`▸` 是光标，`●`/`○` 是选中态。
//! - 文本框的焦点落在框身上（铺底色），下面那根线只区分「正在打字」。

use miyu_base::terminal::palette::{Theme, BLUE, DIM, FAINT, GOLD};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 正文最宽多少列；窄终端跟着视口缩。
pub(super) const BODY_MAX: usize = 62;
pub(super) const NAME_COL: usize = 16;

static BODY_W: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(BODY_MAX);

pub(super) fn body_w() -> usize {
    BODY_W.load(std::sync::atomic::Ordering::Relaxed)
}

pub(super) fn set_body_w(width: usize) {
    BODY_W.store(width, std::sync::atomic::Ordering::Relaxed);
}

pub(super) fn pad(text: &str, cols: usize) -> String {
    let width = text.width();
    if width >= cols {
        text.to_string()
    } else {
        format!("{}{}", text, " ".repeat(cols - width))
    }
}

pub fn clip(text: &str, cols: usize) -> String {
    if text.width() <= cols {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width > cols.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}

pub(super) fn wrap(text: &str, cols: usize) -> Vec<String> {
    if cols == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        let mut used = 0usize;
        for ch in paragraph.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + width > cols && !current.is_empty() {
                out.push(std::mem::take(&mut current));
                used = 0;
            }
            current.push(ch);
            used += width;
        }
        out.push(current);
    }
    out
}

pub(super) fn line_width(line: &Line) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

pub(super) fn ln(spans: Vec<Span<'static>>) -> Line<'static> {
    Line::from(spans)
}

pub(super) fn nil() -> Line<'static> {
    Line::from("")
}

/// 每屏的绘制上下文。把 `Theme` 随手带着，省得每个函数都多一个参数。
pub(in crate::oobe) struct Cx {
    pub theme: Theme,
}

impl Cx {
    pub fn txt(&self, text: impl Into<String>, style: Style) -> Line<'static> {
        Line::from(Span::styled(text.into(), style))
    }

    pub fn bold(&self, text: impl Into<String>) -> Line<'static> {
        self.txt(text, Style::new().add_modifier(Modifier::BOLD))
    }

    /// 左右两列：左边占到 `col` 列（至少空两格），右边接着放。
    pub fn two(
        &self,
        left: Vec<Span<'static>>,
        col: usize,
        right: Vec<Span<'static>>,
    ) -> Line<'static> {
        let used: usize = left.iter().map(|span| span.content.width()).sum();
        let mut spans = left;
        spans.push(Span::raw(" ".repeat(col.saturating_sub(used).max(2))));
        spans.extend(right);
        Line::from(spans)
    }

    /// 选中行。真彩/256 铺底色，16 色以下换反显。
    pub fn select(&self, mut line: Line<'static>) -> Line<'static> {
        let used = line_width(&line);
        if used < body_w() {
            line.spans.push(Span::raw(" ".repeat(body_w() - used)));
        }
        for span in &mut line.spans {
            span.style = self.theme.select(span.style);
        }
        line
    }

    pub fn divider(&self, name: &str) -> Line<'static> {
        let tail = body_w().saturating_sub(4 + name.width());
        ln(vec![
            Span::styled(
                format!("{}{} ", self.theme.hline(), self.theme.hline()),
                self.theme.fg(FAINT),
            ),
            Span::styled(name.to_string(), self.theme.dim(DIM)),
            Span::raw(" "),
            Span::styled(self.theme.hline().repeat(tail), self.theme.fg(FAINT)),
        ])
    }

    pub fn radio(&self, cur: bool, on: bool, text: &str, note: &str, col: usize) -> Line<'static> {
        let theme = self.theme;
        let left = vec![
            Span::styled(
                if cur {
                    theme.cursor().to_string()
                } else {
                    "  ".into()
                },
                theme.fg(if cur { BLUE } else { FAINT }),
            ),
            Span::styled(
                if on {
                    theme.radio_on()
                } else {
                    theme.radio_off()
                },
                theme.fg(if on { BLUE } else { FAINT }),
            ),
            Span::raw(" "),
            Span::styled(
                text.to_string(),
                if cur {
                    theme.fg(BLUE)
                } else if on {
                    Style::new()
                } else {
                    theme.dim(DIM)
                },
            ),
        ];
        let line = if note.is_empty() {
            Line::from(left)
        } else {
            self.two(
                left,
                col,
                vec![Span::styled(note.to_string(), theme.fg(FAINT))],
            )
        };
        if cur {
            self.select(line)
        } else {
            line
        }
    }

    pub fn action(&self, cur: bool, text: &str) -> Line<'static> {
        let theme = self.theme;
        let line = ln(vec![
            Span::styled(
                if cur {
                    theme.cursor().to_string()
                } else {
                    "  ".into()
                },
                theme.fg(if cur { GOLD } else { FAINT }),
            ),
            Span::styled(
                text.to_string(),
                if cur {
                    theme.fg(GOLD).add_modifier(Modifier::BOLD)
                } else {
                    theme.dim(DIM)
                },
            ),
            Span::styled(
                format!("  {}", theme.arrow()),
                theme.fg(if cur { GOLD } else { FAINT }),
            ),
        ]);
        if cur {
            self.select(line)
        } else {
            line
        }
    }

    /// 文本框。返回行、插入点相对行号、插入点列。
    pub fn field(
        &self,
        value: &str,
        placeholder: &str,
        cur: bool,
        editing: bool,
        mask: bool,
    ) -> (Vec<Line<'static>>, usize, usize) {
        let theme = self.theme;
        let mut out = Vec::new();
        let shown = if mask {
            "•".repeat(value.chars().count())
        } else {
            value.to_string()
        };
        let rows = if shown.is_empty() {
            vec![String::new()]
        } else {
            wrap(&shown, body_w().saturating_sub(1))
        };
        let last = rows.len() - 1;
        let mut caret_col = 0usize;
        for (index, row) in rows.iter().enumerate() {
            let mut spans = Vec::new();
            if shown.is_empty() && index == 0 {
                spans.push(Span::styled(
                    clip(placeholder, body_w().saturating_sub(1)),
                    theme.fg(FAINT),
                ));
            } else {
                spans.push(Span::raw(row.clone()));
            }
            if index == last {
                caret_col = row.width();
            }
            let line = ln(spans);
            out.push(if (cur || editing) && index == last {
                self.select(line)
            } else {
                line
            });
        }
        let style = if editing {
            theme.fg(BLUE).add_modifier(Modifier::BOLD)
        } else {
            theme.fg(FAINT)
        };
        out.push(ln(vec![Span::styled(
            theme.hline().repeat(body_w()),
            style,
        )]));
        let caret_row = out.len() - 2;
        (out, caret_row, caret_col)
    }
}

/// 一屏的内容。
pub(super) struct View {
    /// 钉在滚动视口**上方**、不跟着滚的行。列表一长，表头跟着滚走就没人知道
    /// 这屏在干什么了。
    pub sticky: Vec<Line<'static>>,
    pub body: Vec<Line<'static>>,
    pub cursor_row: usize,
    pub caret: Option<(usize, usize)>,
    /// 底部横线上方那一行（模型搜索的 `/查询`，像 vim 的命令行）。没有就留空行。
    pub footer: Option<Line<'static>>,
    /// 搜索行里的光标列。
    pub footer_caret: Option<usize>,
    pub counter: Option<String>,
    pub keys: Vec<(&'static str, &'static str)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_and_clip_use_display_width() {
        assert_eq!(pad("工具", 6), "工具  ");
        assert_eq!(pad("abc", 2), "abc");
        assert_eq!(clip("工具与开发", 6), "工具…");
        assert_eq!(clip("ab", 6), "ab");
    }

    #[test]
    fn wrap_breaks_on_width_and_newlines() {
        assert_eq!(wrap("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(wrap("a\nb", 4), vec!["a", "b"]);
        assert_eq!(wrap("中文中文中", 4), vec!["中文", "中文", "中"]);
    }
}
