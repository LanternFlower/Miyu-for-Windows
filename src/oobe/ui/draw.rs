//! 把一屏画到 ratatui 的帧上：开场是一片星空里凝聚出 MIYU；其余各屏是
//! 渐变 banner + 横向进度轨 + 渐变细线 + 正文 + 紧贴正文的按键条。
//!
//! 三条反抖动铁律（都是实际踩出来的）：
//! 1. **块宽必须是定值**——按键条文案进出编辑态会变长变短，拿「最宽那行」算
//!    宽度整块会左右横跳。
//! 2. **顶边锚定，不按内容居中**——各屏内容长短不一，按实际高度居中 banner 会上下跳。
//! 3. **光标自己管**——从不调 `set_cursor_position`，写完帧再自己 `MoveTo` + `Show`。

use super::build::build;
use super::widgets::{body_w, line_width, nil, set_body_w, BODY_MAX};
use super::{App, Screen, FORM_AT, GLINT_AT, INTRO_END, STEPS, SUBTITLE_AT};
use crate::terminal::palette::{BLUE, CORAL, DIM, FAINT, GOLD};
use crate::terminal::starfield::{
    fade, gradient_banner, gradient_t, hairline, hash2, star_seg, subtitle_rule, Seg,
};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

/// 星域比 banner 大一圈——星星要把字**围住**，挤在外接框里就没那个味道。
const STAR_PAD_X: usize = 17;
const STAR_PAD_Y: usize = 4;
/// 正文视口的**下限**行数：内容再短也占这么高，整块不至于缩成一条。
const CONTENT_MIN: usize = 9;
/// 用来定整块顶边的「典型高度」。按它算 top，banner 就在每屏的**同一个 y** 上。
const TYPICAL_ROWS: usize = 27;
/// 换屏时内容逐行落下的节奏（帧/行）。
const FADE_STEP: usize = 1;
/// 开场星空的密度（值越小越密）。
const STAR_DENSE: u32 = 5;
/// 配置屏边栏星空：更稀、更暗，只在正文两侧的空白里，**绝不压到内容上**。
const STAR_SPARSE: u32 = 11;
const STAR_MARGIN_DIM: f32 = 0.30;

/// 配置屏 banner 上持续扫过的亮带:扫完停一小会儿再来,不歇着。30ms 一帧。
fn glint_position(tick: usize, cols: usize) -> f32 {
    let travel = cols as f32 + 12.0;
    let period = (travel / GLINT_SPEED) as usize + GLINT_PAUSE;
    (tick % period) as f32 * GLINT_SPEED - 6.0
}

const GLINT_SPEED: f32 = 0.9;
const GLINT_PAUSE: usize = 40;

fn seg_span(seg: Seg) -> Span<'static> {
    Span::styled(seg.text, seg.style)
}

fn star(x: usize, y: usize, app: &App, scale: f32, sparsity: u32) -> Span<'static> {
    seg_span(star_seg(x, y, app.tick, app.theme, scale, sparsity))
}

pub(in crate::oobe) fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let tw = usize::from(area.width);
    let th = usize::from(area.height);
    let theme = app.theme;
    let cx = app.cx();
    app.caret_screen = None;

    set_body_w(BODY_MAX.min(tw.saturating_sub(10)).max(20));

    let art = app.art.clone();
    let bcols = art.cols();
    let brows = art.rows();
    let welcome = app.screen == Screen::Welcome;
    let current = app.screen.step();

    let mut head: Vec<Line> = Vec::new();

    if welcome {
        // ── 开场：一片星空，MIYU 从中间凝聚出来 ──
        let sw = (bcols + STAR_PAD_X * 2)
            .min(tw.saturating_sub(2))
            .max(bcols);
        let sh = brows + STAR_PAD_Y * 2;
        let bx = (sw - bcols) / 2;
        let by = STAR_PAD_Y;
        let forming = app.intro < INTRO_END;
        // 开场那一道扫光跟着凝聚走;凝聚完之后照配置屏的节奏持续扫。
        let glint = if !theme.depth.gradient_ok() {
            None
        } else if forming {
            let p = app.intro as f32 - GLINT_AT as f32;
            (p >= -6.0 && p <= bcols as f32 + 6.0).then_some(p)
        } else {
            Some(glint_position(app.tick, bcols))
        };

        for y in 0..sh {
            let mut spans: Vec<Span> = Vec::new();
            for x in 0..sw {
                let inside = y >= by && y < by + brows && x >= bx && x < bx + bcols;
                let glyph = inside
                    .then(|| art.lines[y - by].chars().nth(x - bx).unwrap_or(' '))
                    .filter(|ch| *ch != ' ');

                if let Some(ch) = glyph {
                    let lx = x - bx;
                    let ly = y - by;
                    let t = gradient_t(lx, ly, bcols, brows);
                    // 每格自己的凝聚时刻，从左往右推，带抖动。
                    let lock = FORM_AT as f32
                        + lx as f32 * 1.7
                        + (hash2(lx as u32, ly as u32, 7) % 13) as f32
                        + ly as f32 * 1.1;
                    let age = app.intro as f32 - lock;
                    if forming && age < 0.0 {
                        spans.push(star(x, y, app, 1.0, STAR_DENSE));
                        continue;
                    }
                    if !theme.depth.gradient_ok() {
                        spans.push(Span::styled(ch.to_string(), theme.fg(BLUE)));
                        continue;
                    }
                    let mut style = theme.lerp(BLUE, CORAL, t);
                    // 刚落定的那几帧往白里闪一下。
                    if forming && age >= 0.0 && age < 7.0 {
                        style = theme.lift(if t < 0.5 { BLUE } else { CORAL }, 1.0 - age / 7.0);
                    }
                    if let Some(p) = glint {
                        let distance = (lx as f32 - p).abs();
                        if distance < 5.0 {
                            style = theme.lift(
                                if t < 0.5 { BLUE } else { CORAL },
                                (1.0 - distance / 5.0) * 0.85,
                            );
                        }
                    }
                    spans.push(Span::styled(ch.to_string(), style));
                } else {
                    // 字周围的星。离字越近越暗一点，免得抢戏。
                    let near = if inside { 0.55 } else { 1.0 };
                    spans.push(star(x, y, app, near, STAR_DENSE));
                }
            }
            head.push(Line::from(spans));
        }

        if app.intro >= SUBTITLE_AT {
            let fade_t = (((app.intro - SUBTITLE_AT) as f32) / 20.0).min(1.0);
            head.push(cx.txt(
                subtitle_rule(theme, &art.subtitle),
                fade(theme, DIM, fade_t),
            ));
            head.push(cx.txt(
                format!("·  v{}  ·", env!("CARGO_PKG_VERSION")),
                fade(theme, FAINT, fade_t),
            ));
        } else {
            head.push(nil());
            head.push(nil());
        }
        head.push(nil());
    } else {
        // ── 其余各屏:渐变 banner(扫光持续走)、横向进度轨。
        let glint = theme
            .depth
            .gradient_ok()
            .then(|| glint_position(app.tick, bcols));
        for row in gradient_banner(&art, theme, glint) {
            head.push(Line::from(
                row.into_iter().map(seg_span).collect::<Vec<_>>(),
            ));
        }
        let rule = subtitle_rule(theme, &art.subtitle);
        head.push(cx.txt(
            format!(
                "{}{}",
                " ".repeat(bcols.saturating_sub(rule.width()) / 2),
                rule
            ),
            theme.dim(DIM),
        ));
        head.push(nil());

        // 横向进度轨。竖着挂在 banner 右边会把整块拉偏，横着才压得住中轴。
        let mut rail: Vec<Span> = Vec::new();
        for (index, name) in STEPS.iter().enumerate() {
            let here = current == Some(index);
            let done = current.map(|c| index < c).unwrap_or(false);
            if index > 0 {
                let seg_done = current.map(|c| index <= c).unwrap_or(false);
                rail.push(Span::styled(
                    format!(" {} ", theme.hline().repeat(2)),
                    if seg_done {
                        theme.fg(BLUE)
                    } else {
                        theme.fg(FAINT)
                    },
                ));
            }
            let (dot, style) = if here {
                (
                    theme.dot_here(),
                    theme.fg(BLUE).add_modifier(Modifier::BOLD),
                )
            } else if done {
                (theme.dot_done(), theme.fg(BLUE))
            } else {
                (theme.dot_todo(), theme.fg(FAINT))
            };
            rail.push(Span::styled(dot, style));
            rail.push(Span::raw(" "));
            rail.push(Span::styled(
                *name,
                if here {
                    theme.fg(BLUE).add_modifier(Modifier::BOLD)
                } else if done {
                    theme.dim(DIM)
                } else {
                    theme.fg(FAINT)
                },
            ));
        }
        head.push(Line::from(rail));
        head.push(nil());
    }

    let view = build(app, &cx);

    // ── 视口 ──
    let top_anchor = th.saturating_sub(TYPICAL_ROWS) / 2;
    let chrome = head.len() + view.sticky.len() + 4;
    let avail = th
        .saturating_sub(top_anchor + chrome)
        .max(CONTENT_MIN.min(th.saturating_sub(chrome).max(1)));
    let total = view.body.len();
    if total > avail {
        if view.cursor_row < app.scroll {
            app.scroll = view.cursor_row;
        } else if view.cursor_row >= app.scroll + avail {
            app.scroll = view.cursor_row + 1 - avail;
        }
        app.scroll = app.scroll.min(total - avail);
    } else {
        app.scroll = 0;
    }

    let head_h = head.len() + view.sticky.len();
    let mut inner = head;
    inner.extend(view.sticky.iter().cloned());

    // 换屏时逐行落下：第 i 行等到第 i 帧才出来，最新那两行先暗着。
    for (index, line) in view.body.iter().skip(app.scroll).take(avail).enumerate() {
        let due = index * FADE_STEP;
        if app.fade < due {
            inner.push(nil());
            continue;
        }
        let age = app.fade - due;
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
    let drawn = total.saturating_sub(app.scroll).min(avail);
    for _ in drawn..CONTENT_MIN.min(avail) {
        inner.push(nil());
    }
    // 横线上方那一行：平时留空，模型搜索时是 `/查询`。
    let footer_row = inner.len();
    inner.push(view.footer.clone().unwrap_or_else(nil));

    let mut key_bar: Vec<Span> = Vec::new();
    for (index, (key, label)) in view.keys.iter().enumerate() {
        if index > 0 {
            key_bar.push(Span::raw("   "));
        }
        key_bar.push(Span::styled(*key, theme.fg(GOLD)));
        key_bar.push(Span::raw(" "));
        key_bar.push(Span::styled(*label, theme.fg(FAINT)));
    }
    if let Some(counter) = &view.counter {
        let used: usize = key_bar.iter().map(|span| span.content.width()).sum();
        if body_w() >= used + counter.width() + 3 {
            key_bar.push(Span::raw(" ".repeat(body_w() - used - counter.width())));
            key_bar.push(Span::styled(counter.clone(), theme.fg(FAINT)));
        }
    }
    if !key_bar.is_empty() {
        inner.push(Line::from(
            hairline(theme, body_w())
                .into_iter()
                .map(seg_span)
                .collect::<Vec<_>>(),
        ));
        inner.push(Line::from(key_bar));
    }

    // ── 不要框：内容居中，两侧空白铺极稀的暗星 ──
    let inner_w = body_w().max(bcols);

    if welcome {
        let top = th.saturating_sub(inner.len()) / 2;
        let mut framed: Vec<Line> = (0..top).map(|_| nil()).collect();
        // 开场屏**逐行**居中，不是整块左缘对齐——那句提示要正正落在字下面。
        framed.extend(inner.into_iter().map(|line| {
            let left = tw.saturating_sub(line_width(&line)) / 2;
            let mut spans = vec![Span::raw(" ".repeat(left))];
            spans.extend(line.spans);
            Line::from(spans)
        }));
        frame.render_widget(
            Paragraph::new(framed),
            Rect::new(0, 0, area.width, area.height),
        );
        return;
    }

    let left = tw.saturating_sub(inner_w) / 2;
    let top = top_anchor.min(th.saturating_sub(inner.len().min(th)));
    let gap = 2usize;
    let mut out: Vec<Line> = Vec::with_capacity(th);
    for y in 0..th {
        let content = (y >= top && y < top + inner.len()).then(|| &inner[y - top]);
        let mut spans: Vec<Span> = Vec::new();
        let star_left = left.saturating_sub(gap);
        for x in 0..star_left {
            spans.push(star(x, y, app, STAR_MARGIN_DIM, STAR_SPARSE));
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
        for x in right_start..tw {
            if x < left + inner_w + gap {
                spans.push(Span::raw(" "));
            } else {
                spans.push(star(x, y, app, STAR_MARGIN_DIM, STAR_SPARSE));
            }
        }
        out.push(Line::from(spans));
    }

    frame.render_widget(
        Paragraph::new(out),
        Rect::new(0, 0, area.width, area.height),
    );

    // 真终端光标：输入法候选框靠它定位。这里只**记下坐标**，摆放交给主循环。
    if let Some((row, col)) = view.caret {
        if row >= app.scroll && row < app.scroll + avail {
            let y = top + head_h + (row - app.scroll);
            let x = left + col;
            if y < th && x < tw {
                app.caret_screen = Some((x as u16, y as u16));
            }
        }
    }
    // 底部搜索行不滚，按它在 inner 里的行号定位。
    if let Some(col) = view.footer_caret {
        let y = top + footer_row;
        let x = left + col;
        if y < th && x < tw {
            app.caret_screen = Some((x as u16, y as u16));
        }
    }
}
