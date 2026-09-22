//! 把一屏画到 ratatui 的帧上。
//!
//! 版面（banner、进度轨、视口、细线、按键条、星空边栏）全在
//! [`miyu_base::terminal::chrome`] 里——设置界面用的是同一份。这里只剩引导
//! 独有的那一屏：开场，一片星空里凝聚出 MIYU。

use super::build::build;
use super::widgets::{compose, nil, set_body_w, Chrome, Stop, BODY_MAX};
use super::{App, Screen, FORM_AT, GLINT_AT, INTRO_END, STEPS, SUBTITLE_AT};
use miyu_base::terminal::chrome::glint_position;
use miyu_base::terminal::palette::{BLUE, CORAL, DIM, FAINT};
use miyu_base::terminal::starfield::{fade, gradient_t, hash2, star_seg, subtitle_rule, Seg};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// 星域比 banner 大一圈——星星要把字**围住**，挤在外接框里就没那个味道。
const STAR_PAD_X: usize = 17;
const STAR_PAD_Y: usize = 4;
/// 开场星空的密度（值越小越密）。
const STAR_DENSE: u32 = 5;

fn seg_span(seg: Seg) -> Span<'static> {
    Span::styled(seg.text, seg.style)
}

fn star(x: usize, y: usize, app: &App, scale: f32, sparsity: u32) -> Span<'static> {
    seg_span(star_seg(x, y, app.tick, app.theme, scale, sparsity))
}

/// 开场那一屏：星空 + 从中间凝聚出来的 MIYU。
fn intro_head(app: &App, tw: usize) -> Vec<Line<'static>> {
    let theme = app.theme;
    let cx = app.cx();
    let art = &app.art;
    let bcols = art.cols();
    let brows = art.rows();
    let sw = (bcols + STAR_PAD_X * 2)
        .min(tw.saturating_sub(2))
        .max(bcols);
    let sh = brows + STAR_PAD_Y * 2;
    let bx = (sw - bcols) / 2;
    let by = STAR_PAD_Y;
    let forming = app.intro < INTRO_END;
    // 开场那一道扫光跟着凝聚走；凝聚完之后照配置屏的节奏持续扫。
    let glint = if !theme.depth.gradient_ok() {
        None
    } else if forming {
        let p = app.intro as f32 - GLINT_AT as f32;
        (p >= -6.0 && p <= bcols as f32 + 6.0).then_some(p)
    } else {
        Some(glint_position(app.tick, bcols))
    };

    let mut head: Vec<Line> = Vec::new();
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
    head
}

pub(in crate::oobe) fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let tw = usize::from(area.width);
    let th = usize::from(area.height);
    app.caret_screen = None;
    set_body_w(BODY_MAX.min(tw.saturating_sub(10)).max(20));

    let welcome = app.screen == Screen::Welcome;
    let current = app.screen.step();
    let rail: Vec<Stop> = if welcome {
        Vec::new()
    } else {
        STEPS
            .iter()
            .enumerate()
            .map(|(index, name)| match current {
                Some(here) if index == here => Stop::here(*name),
                Some(here) if index < here => Stop::done(*name),
                _ => Stop::todo(*name),
            })
            .collect()
    };

    let cx = app.cx();
    let view = build(app, &cx);
    let art = app.art.clone();
    let mut chrome = Chrome::new(app.theme, &art, &rail);
    chrome.tick = app.tick;
    chrome.fade = app.fade;
    if welcome {
        chrome.head_override = Some(intro_head(app, tw));
        chrome.per_line_center = true;
    }
    let mut scroll = app.scroll;
    let composed = compose(tw, th, &chrome, &view, &mut scroll);
    app.scroll = scroll;
    app.caret_screen = composed.caret;
    frame.render_widget(
        Paragraph::new(composed.lines),
        Rect::new(0, 0, area.width, area.height),
    );
}
