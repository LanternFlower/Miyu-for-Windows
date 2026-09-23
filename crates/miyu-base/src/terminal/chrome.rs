//! 全屏 TUI 的共享版面：渐变 banner + 轨 + 正文视口 + 渐变细线 + 按键条 + 星空边栏。
//!
//! 引导（OOBE）与设置界面（`miyu config`）共用这一张脸。两边只负责说「这屏有
//! 哪些行」（[`View`]），版面、滚动、逐行淡入、居中、星空全在这里。
//! 2026-09-20 从 `src/oobe/ui/{widgets,draw}.rs` 抽出来，好让设置界面复用同一
//! 套构件——不是新造一套。
//!
//! 版式规矩（都是引导那边实际踩出来的，原样保留）：
//! - **列宽一律按显示宽度算**。`format!("{:<n}")` 按字符数补，中文会推歪。
//! - **光标和选中态分开画**：`▸` 是光标，`●`/`○` 是选中态。
//! - **块宽必须是定值**——按键条文案进出编辑态会变长变短，拿「最宽那行」算宽度
//!   整块会左右横跳。
//! - **顶边锚定，不按内容居中**——各屏内容长短不一，按实际高度居中会上下跳。
//! - **光标自己管**——这里只回报插入点该落在哪，`MoveTo` + `Show` 交给调用方的
//!   主循环（ratatui 写内容那一路光标跟着每个 MoveTo 跳）。

use super::palette::{Theme, BLUE, CORAL, DIM, FAINT, GOLD};
use super::starfield::{gradient_banner, hairline, star_seg, subtitle_rule, BannerArt, Seg};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 正文最宽多少列；窄终端跟着视口缩。
pub const BODY_MAX: usize = 62;
pub const NAME_COL: usize = 16;

/// 正文视口的**下限**行数：内容再短也占这么高，整块不至于缩成一条。
const CONTENT_MIN: usize = 9;
/// 用来定整块顶边的「典型高度」。按它算 top，banner 就在每屏的**同一个 y** 上。
const TYPICAL_ROWS: usize = 27;
/// 换屏时内容逐行落下的节奏（帧/行）。
const FADE_STEP: usize = 1;
/// 边栏星空：稀、暗，只在正文两侧的空白里，**绝不压到内容上**。
const STAR_SPARSE: u32 = 11;
const STAR_MARGIN_DIM: f32 = 0.30;
/// banner 上持续扫过的亮带：扫完停一小会儿再来，不歇着。
const GLINT_SPEED: f32 = 0.9;
const GLINT_PAUSE: usize = 40;
/// 矮到这个行数以下，banner 塌成一行——正文比装饰要紧。
pub const COMPACT_ROWS: usize = 30;
/// [`Anchor::Centered`] 上下最多各留多少行。按屏高走（屏越高留得越多），封顶
/// 免得超高的窗口上下空出两大片。
const BLOCK_MARGIN_MAX: usize = 5;

/// 整块怎么摆。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// 顶边锚定，内容往下长。引导专用：五屏内容长短接近，锚死顶边 banner 就
    /// 停在同一个 y 上。
    Top,
    /// 块高随这一屏的内容走，整块垂直居中。设置界面专用：层级深、每屏内容
    /// 差一大截，顶边锚定的话条目一多就贴着屏底、顶上空一大片（2026-09-20
    /// 用户在 58 行的窗口里看到的就是这个）；块高定死又会让按键条离内容老远。
    /// 代价是 banner 的 y 跟着内容多少小幅移动——按键条紧跟正文更要紧。
    Centered,
}

/// 用户自定义艺术字放哪：`<配置目录>/banner.txt`。空会话 banner、引导、设置
/// 界面读的是同一份。
pub const BANNER_FILE: &str = "banner.txt";

/// 取这台机器该用的艺术字：`banner.txt` 存在且能解析就用它，否则用内置那份。
pub fn load_banner(config_dir: &std::path::Path, ascii: bool) -> BannerArt {
    std::fs::read_to_string(config_dir.join(BANNER_FILE))
        .ok()
        .and_then(|text| BannerArt::from_text(&text))
        .unwrap_or_else(|| BannerArt::builtin(ascii))
}

// 这两个量是「同一次绘制里，排版算出来的宽高传给下游渲染函数」的近路：
// `compose` 在开头写，十来个渲染辅助函数在后面读，中间隔着好几层调用，
// 一路透传参数会把签名污染一遍。
//
// 但它们**不能是进程级的**。绘制本身是单线程事件循环（生产的两个调用点
// 都在 TUI 的 `draw()` 里，`oobe` 那处 `set_body_w` 与 `compose` 就在同一个
// 函数体内），可测试是多线程并行的：两个用例各自用不同宽高调 `compose`，
// 进程级变量会被对方改掉，于是断言随并行度随机翻红（2026-09-21 取证）。
// 改成 thread_local 之后，生产那条单线程路径行为一字不变，并行用例则各看
// 各的那一份。
thread_local! {
    static BODY_W: std::cell::Cell<usize> = const { std::cell::Cell::new(BODY_MAX) };
    /// 上一帧正文视口实际有几行。列表自己分页（并排的几列各滚各的）要按
    /// **同一个**数来算，否则光标走到底时那一列会自己截掉几行。
    static VIEWPORT_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(10) };
}

/// 上一帧正文视口有几行。
pub fn viewport_rows() -> usize {
    VIEWPORT_ROWS.with(|rows| rows.get())
}

pub fn body_w() -> usize {
    BODY_W.with(|width| width.get())
}

pub fn set_body_w(width: usize) {
    BODY_W.with(|slot| slot.set(width));
}

pub fn pad(text: &str, cols: usize) -> String {
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

pub fn wrap(text: &str, cols: usize) -> Vec<String> {
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

pub fn line_width(line: &Line) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

pub fn ln(spans: Vec<Span<'static>>) -> Line<'static> {
    Line::from(spans)
}

pub fn nil() -> Line<'static> {
    Line::from("")
}

/// 每屏的绘制上下文。把 `Theme` 随手带着，省得每个函数都多一个参数。
#[derive(Clone, Copy)]
pub struct Cx {
    pub theme: Theme,
}

impl Cx {
    pub fn new(theme: Theme) -> Self {
        Self { theme }
    }

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

    /// 名称 + 右列值的一行。设置界面的菜单项、表单字段都是这个形状：
    /// 左边是能点的名字，右边是当前值。
    ///
    /// 值用的是 `DIM` 而不是更暗的 `FAINT`——`FAINT` 是「补充说明」那一档，
    /// 而这里的值（启用 / 禁用 / 10 / 完整）本来就是要读的内容，压太暗看着
    /// 费劲（2026-09-20 用户报的）。
    pub fn row(&self, cur: bool, name: &str, value: &str, col: usize) -> Line<'static> {
        self.row_styled(cur, name, value, col, self.theme.dim(DIM))
    }

    /// 同 [`Cx::row`]，右列自带样式（比如「未启用」要更暗、报错要用暖色）。
    pub fn row_styled(
        &self,
        cur: bool,
        name: &str,
        value: &str,
        col: usize,
        value_style: Style,
    ) -> Line<'static> {
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
                name.to_string(),
                if cur { theme.fg(BLUE) } else { Style::new() },
            ),
        ];
        let line = if value.is_empty() {
            Line::from(left)
        } else {
            self.two(
                left,
                col,
                vec![Span::styled(value.to_string(), value_style)],
            )
        };
        if cur {
            self.select(line)
        } else {
            line
        }
    }

    /// 复选行。`[*]` / `[ ]` 在低色深下也读得出来，所以不跟着色深变形。
    pub fn check(&self, cur: bool, on: bool, text: &str, note: &str, col: usize) -> Line<'static> {
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
                if on { "[*]" } else { "[ ]" }.to_string(),
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
                vec![Span::styled(note.to_string(), theme.dim(DIM))],
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
        self.field_at(value, placeholder, cur, editing, mask, usize::MAX)
    }

    /// 同 [`Cx::field`]，但插入点落在第 `caret_chars` 个字符前（设置界面的输入框
    /// 支持左右移动光标，光标不总在末尾）。
    pub fn field_at(
        &self,
        value: &str,
        placeholder: &str,
        cur: bool,
        editing: bool,
        mask: bool,
        caret_chars: usize,
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
        // 插入点：按「前 n 个字符占多宽」在折行后的行里定位。
        let head_width: usize = shown
            .chars()
            .take(caret_chars)
            .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
            .sum();
        let mut caret_row = last;
        let mut caret_col = 0usize;
        let mut consumed = 0usize;
        for (index, row) in rows.iter().enumerate() {
            let width = row.width();
            if caret_chars != usize::MAX && head_width <= consumed + width {
                caret_row = index;
                caret_col = head_width - consumed;
                break;
            }
            consumed += width;
            caret_row = index;
            caret_col = width;
        }
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
        (out, caret_row, caret_col)
    }
}

/// 一屏的内容。
#[derive(Default)]
pub struct View {
    /// 钉在滚动视口**上方**、不跟着滚的行。列表一长，表头跟着滚走就没人知道
    /// 这屏在干什么了。
    pub sticky: Vec<Line<'static>>,
    pub body: Vec<Line<'static>>,
    pub cursor_row: usize,
    /// 插入点在 `body` 里的 (行, 列)。
    pub caret: Option<(usize, usize)>,
    /// 底部横线上方那几行（模型搜索的 `/查询`、够长要折行的报错）。空着就留
    /// 一行空行——块高要稳。
    pub footer: Vec<Line<'static>>,
    /// 搜索行里的光标列（落在 footer 的最后一行上）。
    pub footer_caret: Option<usize>,
    pub counter: Option<String>,
    /// 底部按键条：`(键, 说明)`。文案有动态的（搜索词、当前列），所以不是
    /// `&'static str`。
    pub keys: Vec<(String, String)>,
}

/// 轨上的一站。引导里是「第几步」，设置界面里是「进到第几层」——同一套视觉。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stop {
    pub label: String,
    pub state: StopState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopState {
    /// 走过的（引导：已完成的步；设置：上层菜单）。
    Done,
    /// 正在这儿。
    Here,
    /// 还没到（引导专用；设置界面不用）。
    Todo,
}

impl Stop {
    pub fn done(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            state: StopState::Done,
        }
    }

    pub fn here(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            state: StopState::Here,
        }
    }

    pub fn todo(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            state: StopState::Todo,
        }
    }
}

/// 一帧的外框参数。
pub struct Chrome<'a> {
    pub theme: Theme,
    pub art: &'a BannerArt,
    /// 动画帧号：星空闪烁与 banner 扫光都跟着它走。
    pub tick: usize,
    /// 换屏后的帧数，用来做逐行落下；不想要淡入就给个大数。
    pub fade: usize,
    pub rail: &'a [Stop],
    pub body_max: usize,
    pub anchor: Anchor,
    /// 屏顶自己画（引导开场那片凝聚中的星空）。给了就不画 banner 与轨。
    pub head_override: Option<Vec<Line<'static>>>,
    /// 逐行居中而不是整块左缘对齐。开场屏那句提示要正正落在字下面。
    pub per_line_center: bool,
}

impl<'a> Chrome<'a> {
    pub fn new(theme: Theme, art: &'a BannerArt, rail: &'a [Stop]) -> Self {
        Self {
            theme,
            art,
            tick: 0,
            fade: usize::MAX,
            rail,
            body_max: BODY_MAX,
            anchor: Anchor::Top,
            head_override: None,
            per_line_center: false,
        }
    }
}

pub struct Composed {
    pub lines: Vec<Line<'static>>,
    /// 真终端光标该摆哪（输入法候选框靠它定位）。
    pub caret: Option<(u16, u16)>,
    /// 这一次排版算出的正文可用行数，跟存进 `VIEWPORT_ROWS` 的是同一个数。
    /// 那份 thread_local 是留给**跨帧**读的调用方的（`config_tui` 的并排列表
    /// 要按上一帧的高度分页）；谁自己刚调完 `compose`，直接从这儿拿本次的
    /// 结果就行，不必绕全局一圈。
    pub viewport_rows: usize,
}

fn seg_span(seg: Seg) -> Span<'static> {
    Span::styled(seg.text, seg.style)
}

fn star(x: usize, y: usize, tick: usize, theme: Theme) -> Span<'static> {
    seg_span(star_seg(x, y, tick, theme, STAR_MARGIN_DIM, STAR_SPARSE))
}

/// banner 上扫光此刻扫到第几列。
pub fn glint_position(tick: usize, cols: usize) -> f32 {
    let travel = cols as f32 + 12.0;
    let period = (travel / GLINT_SPEED) as usize + GLINT_PAUSE;
    (tick % period) as f32 * GLINT_SPEED - 6.0
}

/// 屏顶那一块：渐变 banner + 副标题 + 轨。矮终端（`compact`）塌成一行。
fn head_lines(chrome: &Chrome, compact: bool) -> Vec<Line<'static>> {
    let theme = chrome.theme;
    let bcols = chrome.art.cols();
    let mut head: Vec<Line> = Vec::new();

    if compact {
        // 塌成一行：正文比装饰要紧，但那道扫光留着——静止的标题条像死了。
        let title = "M I Y U";
        let glint = theme
            .depth
            .gradient_ok()
            .then(|| glint_position(chrome.tick, title.chars().count()));
        let mut spans: Vec<Span> = Vec::new();
        for (index, ch) in title.chars().enumerate() {
            let t = index as f32 / title.chars().count().max(1) as f32;
            let mut style = theme.lerp(BLUE, CORAL, t).add_modifier(Modifier::BOLD);
            if let Some(position) = glint {
                let distance = (index as f32 - position).abs();
                if distance < 5.0 {
                    style = theme
                        .lift(
                            if t < 0.5 { BLUE } else { CORAL },
                            (1.0 - distance / 5.0) * 0.85,
                        )
                        .add_modifier(Modifier::BOLD);
                }
            }
            spans.push(Span::styled(ch.to_string(), style));
        }
        if !chrome.art.subtitle.is_empty() {
            spans.push(Span::styled(
                format!("  {}  {}", theme.hline(), chrome.art.subtitle),
                theme.dim(DIM),
            ));
        }
        head.push(Line::from(spans));
    } else {
        let glint = theme
            .depth
            .gradient_ok()
            .then(|| glint_position(chrome.tick, bcols));
        for row in gradient_banner(chrome.art, theme, glint) {
            head.push(Line::from(
                row.into_iter().map(seg_span).collect::<Vec<_>>(),
            ));
        }
        let rule = subtitle_rule(theme, &chrome.art.subtitle);
        head.push(Line::from(Span::styled(
            format!(
                "{}{}",
                " ".repeat(bcols.saturating_sub(rule.width()) / 2),
                rule
            ),
            theme.dim(DIM),
        )));
    }
    head.push(nil());

    // 横向的轨。竖着挂在 banner 右边会把整块拉偏，横着才压得住中轴。
    if !chrome.rail.is_empty() {
        let mut rail: Vec<Span> = Vec::new();
        let here = chrome
            .rail
            .iter()
            .position(|stop| stop.state == StopState::Here);
        for (index, stop) in chrome.rail.iter().enumerate() {
            if index > 0 {
                let walked = here.map(|h| index <= h).unwrap_or(false);
                rail.push(Span::styled(
                    format!(" {} ", theme.hline().repeat(2)),
                    if walked {
                        theme.fg(BLUE)
                    } else {
                        theme.fg(FAINT)
                    },
                ));
            }
            let (dot, style) = match stop.state {
                StopState::Here => (
                    theme.dot_here(),
                    theme.fg(BLUE).add_modifier(Modifier::BOLD),
                ),
                StopState::Done => (theme.dot_done(), theme.fg(BLUE)),
                StopState::Todo => (theme.dot_todo(), theme.fg(FAINT)),
            };
            rail.push(Span::styled(dot, style));
            rail.push(Span::raw(" "));
            rail.push(Span::styled(
                stop.label.clone(),
                match stop.state {
                    StopState::Here => theme.fg(BLUE).add_modifier(Modifier::BOLD),
                    StopState::Done => theme.dim(DIM),
                    StopState::Todo => theme.fg(FAINT),
                },
            ));
        }
        head.push(Line::from(rail));
        head.push(nil());
    }
    head
}

/// 屏顶那一块占几行。
fn head_rows(art_rows: usize, compact: bool, has_rail: bool) -> usize {
    let banner = if compact { 1 } else { art_rows + 1 };
    banner + 1 + if has_rail { 2 } else { 0 }
}

/// 按键条怎么折行：每一行放哪几项。数行数与真画时走同一份，两边不会算岔。
fn key_bar_groups(
    keys: &[(String, String)],
    counter_w: usize,
    width: usize,
) -> Vec<std::ops::Range<usize>> {
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    let mut start = 0usize;
    let mut used = 0usize;
    for (index, (key, label)) in keys.iter().enumerate() {
        let item = key.width() + 1 + label.width();
        // 末尾那项要给右下角的计数器留位，否则计数器会把它顶掉。
        let reserve = if index + 1 == keys.len() {
            counter_w
        } else {
            0
        };
        if index > start && used + 3 + item + reserve > width {
            groups.push(start..index);
            start = index;
            used = item;
        } else {
            used += if index == start { item } else { 3 + item };
        }
    }
    if start < keys.len() {
        groups.push(start..keys.len());
    }
    groups
}

/// 一屏的高度分配：正文能放几行、整块从第几行起笔。
struct Layout {
    avail: usize,
    top: usize,
}

fn measure(
    anchor: Anchor,
    rows: usize,
    head_len: usize,
    sticky: usize,
    footer: usize,
    key_rows: usize,
    content: usize,
) -> Layout {
    match anchor {
        Anchor::Top => {
            let compact = rows < COMPACT_ROWS;
            let typical = if compact { rows } else { TYPICAL_ROWS };
            let top = rows.saturating_sub(typical) / 2;
            // 这里的 4 = footer + 细线 + 按键条 + 一行余量，引导沿用至今；
            // 改成精确值会让每一屏都往下挪一行。
            let frame = head_len + sticky + 4;
            let avail = rows
                .saturating_sub(top + frame)
                .max(CONTENT_MIN.min(rows.saturating_sub(frame).max(1)));
            Layout { avail, top }
        }
        Anchor::Centered => {
            // 屏越矮越不讲究留白：40 行以下先把内容放下（留白吃掉的那两行，
            // 正好是 18 项的全局设置能不能一屏显全的差别），40 行往上再谈
            // 上下的气口。
            let margin = if rows < 40 {
                1
            } else {
                (rows / 10).min(BLOCK_MARGIN_MAX)
            };
            // +1 = 正文与底下那行（状态/报错）之间的空行，两者贴在一起时
            // 分不清哪句是列表的最后一项（2026-09-20 用户报的）。
            let frame =
                head_len + sticky + 1 + footer + if key_rows > 0 { 1 + key_rows } else { 0 };
            let room = rows.saturating_sub(margin * 2).saturating_sub(frame).max(1);
            // 正文占多高按内容来：条目少就矮一点，按键条跟着贴上去；条目多就
            // 长到屏幕吃得下为止，再多的滚动。整块居中。
            let avail = content.clamp(CONTENT_MIN.min(room), room);
            let block = frame + avail;
            Layout {
                avail,
                top: rows.saturating_sub(block) / 2,
            }
        }
    }
}

/// 还没画就想知道「正文能放几行」（并排列表要按它分页）。跟 compose 用的是
/// 同一个算法，差别只在这里得先把屏顶那一块的高度算出来。
pub fn viewport_estimate(
    art_rows: usize,
    screen_rows: usize,
    anchor: Anchor,
    sticky: usize,
    has_rail: bool,
    footer: usize,
    keys: &[(String, String)],
    counter: Option<&str>,
    content: usize,
) -> usize {
    let compact = screen_rows < COMPACT_ROWS;
    let counter_w = counter.map_or(0, |text| text.width() + 3);
    let key_rows = key_bar_groups(keys, counter_w, body_w()).len();
    measure(
        anchor,
        screen_rows,
        head_rows(art_rows, compact, has_rail),
        sticky,
        footer.max(1),
        key_rows,
        content,
    )
    .avail
}

/// 把一屏拼成整屏的行。`scroll` 是调用方存着的滚动位置，这里按光标行夹一次。
pub fn compose(
    cols: usize,
    rows: usize,
    chrome: &Chrome,
    view: &View,
    scroll: &mut usize,
) -> Composed {
    let theme = chrome.theme;
    let bcols = chrome.art.cols();
    let compact = rows < COMPACT_ROWS;
    set_body_w(chrome.body_max.min(cols.saturating_sub(10)).max(20));

    let head = match &chrome.head_override {
        Some(lines) => lines.clone(),
        None => head_lines(chrome, compact),
    };

    // ── 高度分配 ──
    // 按键条先量再排：它折不折行决定正文还剩几行（`Anchor::Centered` 下块高
    // 是定死的，多一行按键条就少一行正文）。
    let counter_w = view.counter.as_ref().map_or(0, |text| text.width() + 3);
    let groups = key_bar_groups(&view.keys, counter_w, body_w());
    let footer_rows = view.footer.len().max(1);
    let layout = measure(
        chrome.anchor,
        rows,
        head.len(),
        view.sticky.len(),
        footer_rows,
        groups.len(),
        view.body.len(),
    );
    let avail = layout.avail;
    VIEWPORT_ROWS.with(|rows| rows.set(avail));
    let total = view.body.len();
    if total > avail {
        if view.cursor_row < *scroll {
            *scroll = view.cursor_row;
        } else if view.cursor_row >= *scroll + avail {
            *scroll = view.cursor_row + 1 - avail;
        }
        *scroll = (*scroll).min(total - avail);
    } else {
        *scroll = 0;
    }

    let head_h = head.len() + view.sticky.len();
    let mut inner = head;
    inner.extend(view.sticky.iter().cloned());

    // 换屏时内容逐行落下（`chrome.fade` 给个大数就是「立刻全显示」）。
    for (index, line) in view.body.iter().skip(*scroll).take(avail).enumerate() {
        let due = index * FADE_STEP;
        if chrome.fade < due {
            inner.push(nil());
            continue;
        }
        let age = chrome.fade - due;
        if age < 2 {
            let mut faded = line.clone();
            for span in &mut faded.spans {
                span.style = span.style.add_modifier(Modifier::DIM);
            }
            inner.push(faded);
        } else {
            inner.push(line.clone());
        }
    }
    let drawn = total.saturating_sub(*scroll).min(avail);
    // 居中模式下正文一定占满视口：块高恒定，banner 才不会跟着条目多少上下跳。
    let fill_to = match chrome.anchor {
        Anchor::Centered => avail,
        Anchor::Top => CONTENT_MIN.min(avail),
    };
    for _ in drawn..fill_to {
        inner.push(nil());
    }

    // 横线上方那几行：平时一行空的，搜索时是 `/查询`，报错时是折了行的原话。
    if matches!(chrome.anchor, Anchor::Centered) {
        inner.push(nil());
    }
    let footer_row = inner.len() + footer_rows - 1;
    if view.footer.is_empty() {
        inner.push(nil());
    } else {
        inner.extend(view.footer.iter().cloned());
    }

    let mut key_bar: Vec<Vec<Span>> = Vec::new();
    for group in &groups {
        let mut row: Vec<Span> = Vec::new();
        for index in group.clone() {
            let (key, label) = &view.keys[index];
            if !row.is_empty() {
                row.push(Span::raw("   "));
            }
            row.push(Span::styled(key.clone(), theme.fg(GOLD)));
            row.push(Span::raw(" "));
            row.push(Span::styled(label.clone(), theme.fg(FAINT)));
        }
        key_bar.push(row);
    }
    if let Some(counter) = &view.counter {
        let mut tail = key_bar.pop().unwrap_or_default();
        let used: usize = tail.iter().map(|span| span.content.width()).sum();
        if body_w() >= used + counter.width() + 3 {
            tail.push(Span::raw(" ".repeat(body_w() - used - counter.width())));
            tail.push(Span::styled(counter.clone(), theme.fg(FAINT)));
        }
        key_bar.push(tail);
    }
    if !key_bar.is_empty() {
        inner.push(Line::from(
            hairline(theme, body_w())
                .into_iter()
                .map(seg_span)
                .collect::<Vec<_>>(),
        ));
        for row in key_bar {
            inner.push(Line::from(row));
        }
    }

    // ── 不要框：内容居中，两侧空白铺极稀的暗星 ──
    let inner_w = body_w().max(if compact { 0 } else { bcols });
    let left = cols.saturating_sub(inner_w) / 2;
    let top = if chrome.per_line_center {
        rows.saturating_sub(inner.len().min(rows)) / 2
    } else {
        layout.top.min(rows.saturating_sub(inner.len().min(rows)))
    };

    if chrome.per_line_center {
        // 开场屏：每行各自居中，不铺边栏星空（星空在 head 里，整块自带）。
        let mut out: Vec<Line> = (0..top).map(|_| nil()).collect();
        out.extend(inner.into_iter().map(|line| {
            let pad_left = cols.saturating_sub(line_width(&line)) / 2;
            let mut spans = vec![Span::raw(" ".repeat(pad_left))];
            spans.extend(line.spans);
            Line::from(spans)
        }));
        return Composed {
            lines: out,
            caret: None,
            viewport_rows: avail,
        };
    }
    let gap = 2usize;
    let mut out: Vec<Line> = Vec::with_capacity(rows);
    for y in 0..rows {
        let content = (y >= top && y < top + inner.len()).then(|| &inner[y - top]);
        let mut spans: Vec<Span> = Vec::new();
        let star_left = left.saturating_sub(gap);
        for x in 0..star_left {
            spans.push(star(x, y, chrome.tick, theme));
        }
        spans.push(Span::raw(" ".repeat(left - star_left)));
        let used = match content {
            Some(line) => {
                spans.extend(line.spans.iter().cloned());
                line_width(line)
            }
            None => 0,
        };
        let right_start = left + used;
        for x in right_start..cols {
            if x < left + inner_w + gap {
                spans.push(Span::raw(" "));
            } else {
                spans.push(star(x, y, chrome.tick, theme));
            }
        }
        out.push(Line::from(spans));
    }

    let mut caret = None;
    if let Some((row, col)) = view.caret {
        if row >= *scroll && row < *scroll + avail {
            let y = top + head_h + (row - *scroll);
            let x = left + col;
            if y < rows && x < cols {
                caret = Some((x as u16, y as u16));
            }
        }
    }
    // 底部搜索行不滚，按它在 inner 里的行号定位。
    if let Some(col) = view.footer_caret {
        let y = top + footer_row;
        let x = left + col;
        if y < rows && x < cols {
            caret = Some((x as u16, y as u16));
        }
    }

    Composed {
        lines: out,
        caret,
        viewport_rows: avail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::palette::Depth;

    fn theme() -> Theme {
        Theme {
            depth: Depth::True,
            ascii: false,
        }
    }

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

    #[test]
    fn compose_fills_the_whole_screen() {
        let art = BannerArt::builtin(false);
        let rail = [Stop::here("配置")];
        let chrome = Chrome::new(theme(), &art, &rail);
        let cx = Cx::new(theme());
        let view = View {
            body: vec![cx.row(true, "供应商和模型", "", 34)],
            keys: vec![("⏎".to_string(), "进入".to_string())],
            ..View::default()
        };
        let mut scroll = 0;
        let composed = compose(110, 36, &chrome, &view, &mut scroll);
        assert_eq!(composed.lines.len(), 36);
        // 每一行都铺满整屏宽：星空要占住正文两侧，diff 才不会留下旧字。
        for line in &composed.lines {
            assert_eq!(line_width(line), 110);
        }
    }

    #[test]
    fn compact_screen_drops_the_banner() {
        let art = BannerArt::builtin(false);
        let rail = [Stop::here("配置")];
        let chrome = Chrome::new(theme(), &art, &rail);
        let cx = Cx::new(theme());
        let view = View {
            body: vec![cx.row(true, "供应商和模型", "", 34)],
            ..View::default()
        };
        let mut scroll = 0;
        let tall = compose(110, 40, &chrome, &view, &mut scroll);
        let short = compose(110, 20, &chrome, &view, &mut scroll);
        let art_rows = |composed: &Composed| {
            composed
                .lines
                .iter()
                .filter(|line| line.spans.iter().any(|span| span.content.contains('█')))
                .count()
        };
        let art_rows_with_block = art.lines.iter().filter(|line| line.contains('█')).count();
        assert_eq!(art_rows(&tall), art_rows_with_block);
        assert_eq!(art_rows(&short), 0);
        assert_eq!(short.lines.len(), 20);
    }

    /// 长列表滚动：光标在头、在中、在尾，都必须落在视口里。
    /// （2026-09-20 从设置界面自己那份 `menu_window` 接过来的职责。）
    #[test]
    fn viewport_always_contains_the_cursor() {
        let art = BannerArt::builtin(false);
        let rail = [Stop::here("配置")];
        let chrome = Chrome::new(theme(), &art, &rail);
        let cx = Cx::new(theme());
        let mut scroll = 0;
        for cursor in [0usize, 1, 20, 50, 98, 99] {
            let view = View {
                body: (0..100)
                    .map(|i| cx.row(i == cursor, "项", "", 20))
                    .collect(),
                cursor_row: cursor,
                ..View::default()
            };
            // 从**自己这一次**的排版结果里拿可用行数。原来读的是
            // `viewport_rows()`，那时它背后是个进程级 atomic：同一个测试
            // 二进制里并行跑的其它用例只要也调 `compose` 就会把它改掉，这条
            // 断言于是随并行度随机翻红（2026-09-21 取证：单跑 miyu-base
            // 342/0，跟另外两个 crate 一起跑就 341/1，与被测代码无关）。
            // 那个变量现在已经是 thread_local，这里读返回值是第二道：不碰
            // 共享状态的写法，本就比"写进某处再读回来"更难出错。
            let avail = compose(110, 36, &chrome, &view, &mut scroll).viewport_rows;
            assert!(
                scroll <= cursor && cursor < scroll + avail,
                "光标 {cursor} 不在视口 {scroll}..{} 里",
                scroll + avail
            );
        }
        // 短列表不滚。
        let view = View {
            body: vec![cx.row(true, "项", "", 20)],
            ..View::default()
        };
        compose(110, 36, &chrome, &view, &mut scroll);
        assert_eq!(scroll, 0);
    }

    /// 居中锚定：条目多少都居中，按键条始终紧跟正文。
    #[test]
    fn centered_anchor_keeps_the_block_in_the_middle() {
        let art = BannerArt::builtin(false);
        let rail = [Stop::here("配置")];
        let cx = Cx::new(theme());
        let text_of = |line: &Line| -> String {
            line.spans
                .iter()
                .map(|span| span.content.to_string())
                .collect()
        };
        for count in [3usize, 12, 40] {
            let mut chrome = Chrome::new(theme(), &art, &rail);
            chrome.anchor = Anchor::Centered;
            let view = View {
                body: (0..count).map(|i| cx.row(i == 0, "项", "", 20)).collect(),
                keys: vec![("⏎".to_string(), "选择".to_string())],
                ..View::default()
            };
            let mut scroll = 0;
            let composed = compose(110, 58, &chrome, &view, &mut scroll);
            let first = composed
                .lines
                .iter()
                .position(|line| text_of(line).contains('█'))
                .expect("banner");
            let last = composed
                .lines
                .iter()
                .rposition(|line| text_of(line).contains("选择"))
                .expect("按键条");
            let (top, bottom) = (first, 57 - last);
            assert!(
                top.abs_diff(bottom) <= 1,
                "{count} 条时上留白 {top}、下留白 {bottom}"
            );
        }
    }

    #[test]
    fn scroll_follows_the_cursor() {
        let art = BannerArt::builtin(false);
        let rail = [Stop::here("配置")];
        let chrome = Chrome::new(theme(), &art, &rail);
        let cx = Cx::new(theme());
        let view = View {
            body: (0..40).map(|i| cx.row(i == 39, "项", "", 20)).collect(),
            cursor_row: 39,
            ..View::default()
        };
        let mut scroll = 0;
        compose(110, 36, &chrome, &view, &mut scroll);
        assert!(scroll > 0, "光标在末尾时视口要跟过去");
    }
}
