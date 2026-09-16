//! 星空、渐变艺术字、扫光——引导开场与空会话 banner 共用的绘制原语。
//!
//! 只产「文本 + 样式」的片段（[`Seg`]），不碰终端：全屏后端把它转成 ANSI 行，
//! 引导向导把它塞进 ratatui 的 `Line`。两边看到的是同一颗星、同一道光。
//!
//! 几条设计上的决定（对应 `docs/plan/2026-09-12-tui-oobe.md`）：
//!
//! - **星星的尺寸是主角，不是亮度**。终端里亮度变化很弱，低色深下根本没有
//!   中间档；换字形（点 → 十字 → 四角星 → 六角星）在任何终端都成立。
//! - **每颗星各闪各的**。周期和相位都从坐标哈希出来，齐步走的星空像故障。
//! - **渐变从雾蓝扫到酒红**，不用金：蓝金是补色，中段会挤出脏黄绿；金色
//!   完整留给「可以按」这一个含义。
//! - 字形避开 `★`(U+2605) 与 `·`(U+00B7)：East Asian Width 是 Ambiguous，
//!   CJK 环境下有的终端按 2 格渲染，整行就歪了。

use super::palette::{Rgb, Theme, BLUE, CORAL, INK};
use ratatui::style::Style;
use unicode_width::UnicodeWidthStr;

/// 一段同样式的文本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seg {
    pub text: String,
    pub style: Style,
}

impl Seg {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }

    pub fn raw(text: impl Into<String>) -> Self {
        Self::new(text, Style::new())
    }
}

/// 一行片段的显示宽度。
pub fn segs_width(segs: &[Seg]) -> usize {
    segs.iter().map(|seg| seg.text.width()).sum()
}

// ───────────────────────────── 艺术字 ─────────────────────────────

/// ANSI Shadow，32×6。
pub const BANNER_UNICODE: [&str; 6] = [
    "███╗   ███╗██╗██╗   ██╗██╗   ██╗",
    "████╗ ████║██║╚██╗ ██╔╝██║   ██║",
    "██╔████╔██║██║ ╚████╔╝ ██║   ██║",
    "██║╚██╔╝██║██║  ╚██╔╝  ██║   ██║",
    "██║ ╚═╝ ██║██║   ██║   ╚██████╔╝",
    "╚═╝     ╚═╝╚═╝   ╚═╝    ╚═════╝ ",
];

/// 纯 ASCII 兜底，27×5。没有 Unicode 的终端也认。
pub const BANNER_ASCII: [&str; 5] = [
    r" __  __  ___ __   __ _   _ ",
    r"|  \/  ||_ _|\ \ / /| | | |",
    r"| |\/| | | |  \ V / | | | |",
    r"| |  | | | |   | |  | |_| |",
    r"|_|  |_||___|  |_|   \___/ ",
];

/// 艺术字的来源。这是 banner 的「插件接口」：内置那份之外，用户可以在
/// `config/banner.txt` 里放自己的文本画，渐变、星空、扫光照给。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BannerArt {
    /// 每行等宽（短行右侧补空格）。
    pub lines: Vec<String>,
    /// 副标题（艺术字下面那行细字），空 = 不画。
    pub subtitle: String,
}

impl BannerArt {
    pub fn builtin(ascii: bool) -> Self {
        let lines: &[&str] = if ascii {
            &BANNER_ASCII
        } else {
            &BANNER_UNICODE
        };
        Self::from_lines(
            lines.iter().map(|line| line.to_string()).collect(),
            "A G E N T",
        )
    }

    /// 从文本文件读：非空行就是艺术字；`# ` 开头的行当注释跳过；
    /// 首行若是 `subtitle: …` 则作副标题。
    pub fn from_text(text: &str) -> Option<Self> {
        let mut subtitle = String::new();
        let mut lines = Vec::new();
        for raw in text.lines() {
            let line = raw.trim_end_matches(['\r', '\n']);
            if let Some(rest) = line.strip_prefix("subtitle:") {
                if lines.is_empty() {
                    subtitle = rest.trim().to_string();
                    continue;
                }
            }
            if line.starts_with("# ") || line == "#" {
                continue;
            }
            lines.push(line.to_string());
        }
        while lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.pop();
        }
        while lines.first().is_some_and(|line| line.trim().is_empty()) {
            lines.remove(0);
        }
        if lines.is_empty() {
            return None;
        }
        Some(Self::from_lines(lines, &subtitle))
    }

    fn from_lines(mut lines: Vec<String>, subtitle: &str) -> Self {
        let width = lines.iter().map(|line| line.width()).max().unwrap_or(0);
        for line in &mut lines {
            let pad = width.saturating_sub(line.width());
            if pad > 0 {
                line.push_str(&" ".repeat(pad));
            }
        }
        Self {
            lines,
            subtitle: subtitle.to_string(),
        }
    }

    pub fn cols(&self) -> usize {
        self.lines.first().map(|line| line.width()).unwrap_or(0)
    }

    pub fn rows(&self) -> usize {
        self.lines.len()
    }
}

// ───────────────────────────── 一点数学 ─────────────────────────────

/// 便宜的二维哈希。解码噪点要每帧都变，所以把帧号也搅进去。
pub fn hash2(x: u32, y: u32, seed: u32) -> u32 {
    let mut h =
        x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ seed.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491);
    h ^ (h >> 13)
}

/// 星星的尺寸阶梯：点 → 十字 → 四角星 → 六角星，再收回去、消失。
const STAR_UNICODE: [&str; 8] = [".", "+", "✦", "✶", "✦", "+", ".", " "];
const STAR_ASCII: [&str; 8] = [".", "+", "*", "#", "*", "+", ".", " "];

/// 某一格此刻有没有星、亮到什么程度。`None` = 这格没星。
///
/// `sparsity` 越大越稀（开场 5，配置屏边栏 11）。
pub fn star_at(
    x: u32,
    y: u32,
    frame: u32,
    ascii: bool,
    sparsity: u32,
) -> Option<(&'static str, f32)> {
    // 稀疏一点才像星空，密了就是噪点。
    if hash2(x, y, 3) % sparsity.max(1) != 0 {
        return None;
    }
    let speed = 2 + hash2(x, y, 11) % 3; // 2~4 帧一档，快慢不一
    let offset = hash2(x, y, 9) % 24;
    let stage = (((frame + offset) / speed) % 8) as usize;
    let glyph = if ascii {
        STAR_ASCII[stage]
    } else {
        STAR_UNICODE[stage]
    };
    if glyph == " " {
        return None;
    }
    // 亮度跟着尺寸走：最大那档最亮。
    let bright = [0.15, 0.4, 0.7, 1.0, 0.7, 0.4, 0.15, 0.0][stage];
    Some((glyph, bright))
}

/// 一格星星的片段；没星就是空格。`scale` 用来压暗（比如贴着字的那一圈）。
pub fn star_seg(x: usize, y: usize, frame: usize, theme: Theme, scale: f32, sparsity: u32) -> Seg {
    match star_at(x as u32, y as u32, frame as u32, theme.ascii, sparsity) {
        Some((glyph, bright)) => Seg::new(
            glyph,
            theme.lerp(INK, if x % 3 == 0 { CORAL } else { BLUE }, bright * scale),
        ),
        None => Seg::raw(" "),
    }
}

/// 斜向渐变的位置参数：左上雾蓝 → 右下酒红。横向权重大于纵向，因为 banner
/// 是 32×6 的扁形，纵向再给等权会让色带看着是斜的。
pub fn gradient_t(x: usize, y: usize, cols: usize, rows: usize) -> f32 {
    (x as f32 / cols.max(1) as f32) * 0.68 + (y as f32 / rows.max(1) as f32) * 0.32
}

/// 一道扫过 banner 的亮带。`pos` 是亮带中心所在的列（可以在字外面），
/// 返回这一列该往白里提多少（0 = 不提）。
pub fn glint_lift(x: usize, pos: f32) -> f32 {
    let distance = (x as f32 - pos).abs();
    if distance < 5.0 {
        (1.0 - distance / 5.0) * 0.85
    } else {
        0.0
    }
}

/// 渐变着色的艺术字：每行一串单字片段。`glint` 给一个列位置就带扫光。
pub fn gradient_banner(art: &BannerArt, theme: Theme, glint: Option<f32>) -> Vec<Vec<Seg>> {
    let cols = art.cols();
    let rows = art.rows();
    art.lines
        .iter()
        .enumerate()
        .map(|(y, line)| {
            if !theme.depth.gradient_ok() {
                return vec![Seg::new(line.clone(), theme.fg(BLUE))];
            }
            let mut out = Vec::with_capacity(cols);
            for (x, ch) in line.chars().enumerate() {
                let t = gradient_t(x, y, cols, rows);
                let mut style = theme.lerp(BLUE, CORAL, t);
                if let Some(pos) = glint {
                    let lift = glint_lift(x, pos);
                    if lift > 0.0 && ch != ' ' {
                        style = theme.lift(if t < 0.5 { BLUE } else { CORAL }, lift);
                    }
                }
                out.push(Seg::new(ch.to_string(), style));
            }
            out
        })
        .collect()
}

/// 渐变细线。框撤了之后，分区靠它和留白表达：两端淡出，中间亮——像一道
/// 光带，不像一根尺子。
pub fn hairline(theme: Theme, width: usize) -> Vec<Seg> {
    if !theme.depth.gradient_ok() {
        return vec![Seg::new(
            theme.hline().repeat(width),
            theme.fg(super::palette::FAINT),
        )];
    }
    let mut out = Vec::with_capacity(width);
    for i in 0..width {
        let u = i as f32 / width.saturating_sub(1).max(1) as f32;
        let edge = 1.0 - (u - 0.5).abs() * 2.0; // 0 → 1 → 0
        out.push(Seg::new(
            theme.hline(),
            theme.lerp(INK, if u < 0.5 { BLUE } else { CORAL }, edge * 0.75),
        ));
    }
    out
}

/// 副标题那行：`──────  A G E N T  ──────`。
pub fn subtitle_rule(theme: Theme, subtitle: &str) -> String {
    if subtitle.trim().is_empty() {
        return String::new();
    }
    let rule = theme.hline().repeat(6);
    format!("{rule}  {}  {rule}", subtitle.trim())
}

/// 把颜色往底色压：`t` 越小越贴近底色。用来做淡入。
pub fn fade(theme: Theme, color: Rgb, t: f32) -> Style {
    theme.lerp(INK, color, t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::palette::Depth;

    #[test]
    fn builtin_art_is_rectangular() {
        let art = BannerArt::builtin(false);
        assert_eq!(art.rows(), 6);
        assert!(art.lines.iter().all(|line| line.width() == art.cols()));
        let ascii = BannerArt::builtin(true);
        assert!(ascii.lines.iter().all(|line| line.width() == ascii.cols()));
    }

    #[test]
    fn text_art_pads_short_lines_and_reads_subtitle() {
        let art = BannerArt::from_text("subtitle: hello\n# comment\nab\nabcd\n\n").unwrap();
        assert_eq!(art.subtitle, "hello");
        assert_eq!(art.lines, vec!["ab  ".to_string(), "abcd".to_string()]);
        assert!(BannerArt::from_text("# only\n\n").is_none());
    }

    #[test]
    fn stars_are_deterministic_and_sparse() {
        let a = star_at(3, 4, 10, false, 5);
        let b = star_at(3, 4, 10, false, 5);
        assert_eq!(a, b);
        let lit = (0..200u32)
            .flat_map(|x| (0..20u32).map(move |y| (x, y)))
            .filter(|(x, y)| star_at(*x, *y, 0, false, 5).is_some())
            .count();
        // 1/5 的格子有星位，再乘上八档里七档可见：肯定不到一半，也不会是零。
        assert!(lit > 0 && lit < 2000);
    }

    #[test]
    fn low_depth_banner_is_a_single_span_per_row() {
        let theme = Theme {
            depth: Depth::Ansi16,
            ascii: false,
        };
        let rows = gradient_banner(&BannerArt::builtin(false), theme, Some(3.0));
        assert!(rows.iter().all(|row| row.len() == 1));
    }
}
