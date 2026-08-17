//! 终端 LaTeX 公式渲染:RaTeX 排版成 PNG,再压成半块(▀/▄)真彩行。
//!
//! 走半块而不是 kitty 图形协议:半块就是普通文本行,与流式渲染器的
//! 行重绘、tmux、滚动回看天然兼容,不碰终端模式。RaTeX 解析不了的
//! 公式返回 None,调用方回退到样式化源码,永不阻断输出。

use ratex_layout::{layout, to_display_list, LayoutOptions};
use ratex_parser::parser::parse;
use ratex_render::{render_to_png, RenderOptions};
use ratex_types::color::Color;
use ratex_types::math_style::MathStyle;

/// 公式的半块画:每行等显示宽(`cols` 列),行内含 24-bit ANSI 与末尾复位。
pub(crate) struct MathArt {
    pub lines: Vec<String>,
    pub cols: usize,
}

/// 公式字色:适配深色终端的雾蓝(与 WebUI 主题同源);亮色终端下亦可辨。
const MATH_COLOR: (f32, f32, f32) = (0.843, 0.890, 1.0); // #d7e3ff
/// alpha 低于此值的像素视为背景,不上色。
const ALPHA_THRESHOLD: u8 = 24;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MathMode {
    /// 块级 `$$…$$`:display 排版,较大字号。
    Block,
    /// 表格单元格:text 排版,行高受限。
    Cell,
}

/// 渲染公式为半块行。`target_rows` 是期望的字符行数(1 行=2 像素高),
/// `max_cols` 是可用终端列数;等比缩放后超宽会整体压窄到 `max_cols`。
pub(crate) fn render_math(
    tex: &str,
    mode: MathMode,
    target_rows: usize,
    max_cols: usize,
) -> Option<MathArt> {
    let png = ratex_png(tex, mode)?;
    halfblock_art(&png, target_rows, max_cols)
}

/// RaTeX 纯 Rust 管线:解析→排版→PNG(透明底)。解析/渲染失败返回 None。
fn ratex_png(tex: &str, mode: MathMode) -> Option<Vec<u8>> {
    let normalized = tex.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let ast = parse(&normalized).ok()?;
    let (math_style, font_size, padding) = match mode {
        MathMode::Block => (MathStyle::Display, 28.0, 4.0),
        MathMode::Cell => (MathStyle::Text, 24.0, 1.0),
    };
    let color = Color { r: MATH_COLOR.0, g: MATH_COLOR.1, b: MATH_COLOR.2, a: 1.0 };
    let layout_opts = LayoutOptions::default().with_style(math_style).with_color(color);
    let render_opts = RenderOptions {
        font_size,
        padding,
        background_color: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 },
        font_dir: String::new(),
        device_pixel_ratio: 2.0,
    };
    let layout_box = layout(&ast, &layout_opts);
    let display_list = to_display_list(&layout_box);
    render_to_png(&display_list, &render_opts).ok()
}

struct Raster {
    pixels: Vec<[u8; 4]>,
    width: usize,
    height: usize,
}

impl Raster {
    fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        self.pixels[y * self.width + x]
    }
}

fn decode_and_trim(png: &[u8]) -> Option<Raster> {
    let image = image::load_from_memory(png).ok()?.to_rgba8();
    let (width, height) = (image.width() as usize, image.height() as usize);
    if width == 0 || height == 0 {
        return None;
    }
    let pixels: Vec<[u8; 4]> = image.pixels().map(|pixel| pixel.0).collect();
    // 裁掉四周全透明的边,让缩放尺寸贴着字形算。
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (width, height, 0usize, 0usize);
    let mut found = false;
    for y in 0..height {
        for x in 0..width {
            if pixels[y * width + x][3] >= ALPHA_THRESHOLD {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if !found {
        return None;
    }
    let trimmed_width = max_x - min_x + 1;
    let trimmed_height = max_y - min_y + 1;
    let mut trimmed = Vec::with_capacity(trimmed_width * trimmed_height);
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            trimmed.push(pixels[y * width + x]);
        }
    }
    Some(Raster { pixels: trimmed, width: trimmed_width, height: trimmed_height })
}

/// 区域平均采样(缩小时抗锯齿;放大时最近邻)。
fn sample(raster: &Raster, x: usize, y: usize, target_width: usize, target_height: usize) -> [u8; 4] {
    if target_width >= raster.width && target_height >= raster.height {
        let source_x = (x * raster.width / target_width).min(raster.width - 1);
        let source_y = (y * raster.height / target_height).min(raster.height - 1);
        return raster.pixel(source_x, source_y);
    }
    let start_x = (x * raster.width / target_width).min(raster.width - 1);
    let end_x = (((x + 1) * raster.width).div_ceil(target_width)).clamp(start_x + 1, raster.width);
    let start_y = (y * raster.height / target_height).min(raster.height - 1);
    let end_y = (((y + 1) * raster.height).div_ceil(target_height)).clamp(start_y + 1, raster.height);
    let (mut r, mut g, mut b, mut a, mut count) = (0u32, 0u32, 0u32, 0u32, 0u32);
    for sy in start_y..end_y {
        for sx in start_x..end_x {
            let pixel = raster.pixel(sx, sy);
            r += u32::from(pixel[0]);
            g += u32::from(pixel[1]);
            b += u32::from(pixel[2]);
            a += u32::from(pixel[3]);
            count += 1;
        }
    }
    [(r / count) as u8, (g / count) as u8, (b / count) as u8, (a / count) as u8]
}

/// 半块化:目标高 `target_rows` 字符行(=2×像素行),宽等比、封顶 `max_cols`。
fn halfblock_art(png: &[u8], target_rows: usize, max_cols: usize) -> Option<MathArt> {
    let raster = decode_and_trim(png)?;
    let target_rows = target_rows.max(1);
    let mut height_px = target_rows * 2;
    let mut width_px = (raster.width * height_px).div_ceil(raster.height).max(1);
    if width_px > max_cols.max(4) {
        width_px = max_cols.max(4);
        height_px = ((raster.height * width_px).div_ceil(raster.width)).max(2);
        // 保持偶数像素行,凑整字符行。
        height_px += height_px % 2;
    }
    let rows = height_px / 2;
    let mut lines = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut line = String::with_capacity(width_px * 24);
        for x in 0..width_px {
            let top = sample(&raster, x, row * 2, width_px, height_px);
            let bottom = sample(&raster, x, row * 2 + 1, width_px, height_px);
            line.push_str(&halfblock_cell(top, bottom));
        }
        line.push_str("\x1b[0m");
        lines.push(line);
    }
    Some(MathArt { lines, cols: width_px })
}

fn halfblock_cell(top: [u8; 4], bottom: [u8; 4]) -> String {
    let top_visible = top[3] >= ALPHA_THRESHOLD;
    let bottom_visible = bottom[3] >= ALPHA_THRESHOLD;
    match (top_visible, bottom_visible) {
        (true, true) => format!(
            "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m▀",
            top[0], top[1], top[2], bottom[0], bottom[1], bottom[2]
        ),
        (true, false) => format!("\x1b[49m\x1b[38;2;{};{};{}m▀", top[0], top[1], top[2]),
        (false, true) => format!("\x1b[49m\x1b[38;2;{};{};{}m▄", bottom[0], bottom[1], bottom[2]),
        (false, false) => "\x1b[49m ".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_block_formula_to_halfblock_lines() {
        let art = render_math(
            r"\operatorname{softmax}\left(\frac{QK^\top}{\sqrt{d_k}}\right)V",
            MathMode::Block,
            9,
            100,
        )
        .expect("ratex should render the attention formula");
        assert!(!art.lines.is_empty());
        assert!(art.cols > 10 && art.cols <= 100);
        assert!(art.lines[0].contains("\u{1b}[")); // 真彩 ANSI
        assert!(art.lines.iter().any(|line| line.contains('▀') || line.contains('▄')));
    }

    #[test]
    fn unparseable_input_falls_back_to_none() {
        assert!(render_math(r"\undefinedmacro{", MathMode::Block, 8, 80).is_none());
        assert!(render_math("", MathMode::Cell, 2, 40).is_none());
    }

    /// 生成检视产物:PNG 与 ANSI 半块文本落到 /tmp 供人工回看。
    #[test]
    #[ignore]
    fn dump_preview_artifacts() {
        let cases = [
            ("attention", r"\operatorname{Attention}(Q,K,V)=\operatorname{softmax}\left(\frac{QK^\top}{\sqrt{d_k}}\right)V", MathMode::Block, 9),
            ("newton", r"x_{n+1}=x_n-\frac{f(x_n)}{f'(x_n)}", MathMode::Cell, 2),
            ("newton3", r"x_{n+1}=x_n-\frac{f(x_n)}{f'(x_n)}", MathMode::Cell, 3),
            ("golden", r"q=\frac{1+\sqrt5}{2}\approx 1.618", MathMode::Cell, 3),
            ("gauss", r"\int_{-\infty}^{\infty} e^{-x^2}\,dx=\sqrt{\pi}", MathMode::Block, 8),
        ];
        for (name, tex, mode, rows) in cases {
            let png = ratex_png(tex, mode).expect(name);
            std::fs::write(format!("/tmp/claude-1000/math-{name}.png"), &png).unwrap();
            let art = render_math(tex, mode, rows, 110).expect(name);
            std::fs::write(
                format!("/tmp/claude-1000/math-{name}.ansi"),
                art.lines.join("\n"),
            )
            .unwrap();
        }
    }
}

// ─────────────── 行内/表格内:Unicode 数学转写(单行纯文本) ───────────────
// 半块图在 1-3 字符行高下不可读(实测),行内与表格单元格改走转写:
// xₙ₊₁、√π、α∈(0,1)——流式安全、表格行高零开销。尽力而为,
// 转不动的命令原样保留,永不失败。

/// 转写递归的嵌套深度上限:正常公式远低于此,超限说明是构造出的
/// 深嵌套(如上万层 `{`),递归转写会栈溢出 abort,回退原文。
const MAX_MATH_NESTING: usize = 64;

/// 统计裸 `{` 的最大嵌套深度(`\{` 转义不计,与递归转写的分组语义一致)。
fn max_brace_depth(chars: &[char]) -> usize {
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    let mut escaped = false;
    for &ch in chars {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '{' => {
                depth += 1;
                max_depth = max_depth.max(depth);
            }
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max_depth
}

/// 把 LaTeX 行内公式尽力转成 Unicode 数学文本。
pub(crate) fn unicode_math(tex: &str) -> String {
    let chars: Vec<char> = tex.chars().collect();
    if max_brace_depth(&chars) > MAX_MATH_NESTING {
        return tex.to_string();
    }
    let mut cursor = 0usize;
    let output = convert_sequence(&chars, &mut cursor, None);
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 递归转写,遇到 `stop`(组结束的 '}')返回。
fn convert_sequence(chars: &[char], cursor: &mut usize, stop: Option<char>) -> String {
    let mut output = String::new();
    while *cursor < chars.len() {
        let ch = chars[*cursor];
        if Some(ch) == stop {
            *cursor += 1;
            return output;
        }
        match ch {
            '\\' => {
                *cursor += 1;
                output.push_str(&convert_command(chars, cursor));
            }
            '^' => {
                *cursor += 1;
                let script = read_group(chars, cursor);
                output.push_str(&to_script(&script, SUPERSCRIPTS, '^'));
            }
            '_' => {
                *cursor += 1;
                let script = read_group(chars, cursor);
                output.push_str(&to_script(&script, SUBSCRIPTS, '_'));
            }
            '{' => {
                *cursor += 1;
                output.push_str(&convert_sequence(chars, cursor, Some('}')));
            }
            '\'' => {
                *cursor += 1;
                output.push('′');
            }
            '~' => {
                *cursor += 1;
                output.push(' ');
            }
            _ => {
                *cursor += 1;
                output.push(ch);
            }
        }
    }
    output
}

/// 读取一个参数组:`{...}`(递归转写)或单个字符/命令。
fn read_group(chars: &[char], cursor: &mut usize) -> String {
    match chars.get(*cursor) {
        Some('{') => {
            *cursor += 1;
            convert_sequence(chars, cursor, Some('}'))
        }
        Some('\\') => {
            *cursor += 1;
            convert_command(chars, cursor)
        }
        Some(ch) => {
            *cursor += 1;
            ch.to_string()
        }
        None => String::new(),
    }
}

fn convert_command(chars: &[char], cursor: &mut usize) -> String {
    // 单字符转义:\{ \} \, \; 等。
    if let Some(&ch) = chars.get(*cursor) {
        if !ch.is_ascii_alphabetic() {
            *cursor += 1;
            return match ch {
                ',' | ';' | ':' | ' ' | '!' => " ".to_string(),
                _ => ch.to_string(),
            };
        }
    }
    let start = *cursor;
    while chars.get(*cursor).is_some_and(|ch| ch.is_ascii_alphabetic()) {
        *cursor += 1;
    }
    let name: String = chars[start..*cursor].iter().collect();
    // 吃掉命令后的一个空格(TeX 语义)。
    if chars.get(*cursor) == Some(&' ') {
        *cursor += 1;
    }
    match name.as_str() {
        "frac" | "dfrac" | "tfrac" => {
            let numerator = read_group(chars, cursor);
            let denominator = read_group(chars, cursor);
            format!("{}/{}", parenthesize(&numerator), parenthesize(&denominator))
        }
        "sqrt" => {
            let radicand = read_group(chars, cursor);
            format!("√{}", parenthesize(&radicand))
        }
        "operatorname" | "text" | "mathrm" | "mathbf" | "mathit" | "textbf" | "textit"
        | "mathsf" | "mathcal" => read_group(chars, cursor),
        "binom" | "dbinom" | "tbinom" => {
            let upper = read_group(chars, cursor);
            let lower = read_group(chars, cursor);
            format!("C({},{})", upper.trim(), lower.trim())
        }
        "hat" | "bar" | "overline" | "vec" | "tilde" | "dot" | "ddot" | "check" | "breve" => {
            let argument = read_group(chars, cursor);
            let mark = match name.as_str() {
                "hat" => '\u{0302}',
                "bar" | "overline" => '\u{0304}',
                "vec" => '\u{20d7}',
                "tilde" => '\u{0303}',
                "dot" => '\u{0307}',
                "ddot" => '\u{0308}',
                "check" => '\u{030c}',
                _ => '\u{0306}',
            };
            // 组合附标跟在末字符后;单字符最自然,多字符也可读。
            format!("{argument}{mark}")
        }
        "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "displaystyle" | "textstyle"
        | "limits" | "nolimits" => String::new(),
        "quad" | "qquad" => " ".to_string(),
        other => symbol_for(other).map(str::to_string).unwrap_or_else(|| format!("\\{other}")),
    }
}

/// 单 token(字母数字或已括)不再加括号。
fn parenthesize(text: &str) -> String {
    let trimmed = text.trim();
    let simple = trimmed.chars().count() <= 1
        || trimmed.chars().all(|ch| ch.is_alphanumeric() || ch == '.' || ch == '′')
        || fully_parenthesized(trimmed);
    if simple { trimmed.to_string() } else { format!("({trimmed})") }
}

/// 整体被同一对匹配括号包裹才算"已括":`(a)+(b)` 两端虽是括号但
/// 首括号在中途就闭合,仍需外层加括号,否则 `\frac{(a)+(b)}{c}`
/// 会转写成 `(a)+(b)/c`,数学语义反转。
fn fully_parenthesized(text: &str) -> bool {
    if !text.starts_with('(') || !text.ends_with(')') {
        return false;
    }
    let mut depth = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index + ch.len_utf8() == text.len();
                }
            }
            _ => {}
        }
    }
    false
}

const SUPERSCRIPTS: &[(char, char)] = &[
    ('0', '⁰'), ('1', '¹'), ('2', '²'), ('3', '³'), ('4', '⁴'), ('5', '⁵'), ('6', '⁶'),
    ('7', '⁷'), ('8', '⁸'), ('9', '⁹'), ('+', '⁺'), ('-', '⁻'), ('−', '⁻'), ('=', '⁼'),
    ('(', '⁽'), (')', '⁾'), ('n', 'ⁿ'), ('i', 'ⁱ'), ('T', 'ᵀ'), ('t', 'ᵗ'), ('k', 'ᵏ'),
    ('m', 'ᵐ'), ('a', 'ᵃ'), ('b', 'ᵇ'), ('c', 'ᶜ'), ('d', 'ᵈ'), ('e', 'ᵉ'), ('x', 'ˣ'),
    ('y', 'ʸ'), ('p', 'ᵖ'), ('r', 'ʳ'), ('s', 'ˢ'), ('u', 'ᵘ'), ('v', 'ᵛ'), ('*', '*'),
    ('′', '′'), ('⊤', 'ᵀ'),
];
const SUBSCRIPTS: &[(char, char)] = &[
    ('0', '₀'), ('1', '₁'), ('2', '₂'), ('3', '₃'), ('4', '₄'), ('5', '₅'), ('6', '₆'),
    ('7', '₇'), ('8', '₈'), ('9', '₉'), ('+', '₊'), ('-', '₋'), ('−', '₋'), ('=', '₌'),
    ('(', '₍'), (')', '₎'), ('a', 'ₐ'), ('e', 'ₑ'), ('h', 'ₕ'), ('i', 'ᵢ'), ('j', 'ⱼ'),
    ('k', 'ₖ'), ('l', 'ₗ'), ('m', 'ₘ'), ('n', 'ₙ'), ('o', 'ₒ'), ('p', 'ₚ'), ('r', 'ᵣ'),
    ('s', 'ₛ'), ('t', 'ₜ'), ('u', 'ᵤ'), ('v', 'ᵥ'), ('x', 'ₓ'),
];

/// 上/下标转换:内容全部有对应字符才转,否则退化为 ^(x)/_(x)。
fn to_script(content: &str, table: &[(char, char)], marker: char) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut converted = String::new();
    for ch in trimmed.chars() {
        match table.iter().find(|(from, _)| *from == ch) {
            Some((_, to)) => converted.push(*to),
            None => return format!("{marker}{}", parenthesize(trimmed)),
        }
    }
    converted
}

fn symbol_for(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "α", "beta" => "β", "gamma" => "γ", "delta" => "δ", "epsilon" => "ε",
        "varepsilon" => "ε", "zeta" => "ζ", "eta" => "η", "theta" => "θ", "vartheta" => "ϑ",
        "iota" => "ι", "kappa" => "κ", "lambda" => "λ", "mu" => "μ", "nu" => "ν", "xi" => "ξ",
        "pi" => "π", "rho" => "ρ", "sigma" => "σ", "tau" => "τ", "upsilon" => "υ", "phi" => "φ",
        "varphi" => "φ", "chi" => "χ", "psi" => "ψ", "omega" => "ω",
        "Gamma" => "Γ", "Delta" => "Δ", "Theta" => "Θ", "Lambda" => "Λ", "Xi" => "Ξ",
        "Pi" => "Π", "Sigma" => "Σ", "Upsilon" => "Υ", "Phi" => "Φ", "Psi" => "Ψ",
        "Omega" => "Ω",
        "infty" => "∞", "partial" => "∂", "nabla" => "∇", "pm" => "±", "mp" => "∓",
        "times" => "×", "div" => "÷", "cdot" => "·", "bullet" => "•", "circ" => "∘",
        "approx" => "≈", "neq" => "≠", "ne" => "≠", "leq" => "≤", "le" => "≤",
        "geq" => "≥", "ge" => "≥", "ll" => "≪", "gg" => "≫", "sim" => "∼", "simeq" => "≃",
        "equiv" => "≡", "propto" => "∝", "to" => "→", "gets" => "←", "mapsto" => "↦",
        "Rightarrow" => "⇒", "Leftarrow" => "⇐", "Leftrightarrow" => "⇔",
        "rightarrow" => "→", "leftarrow" => "←", "leftrightarrow" => "↔",
        "uparrow" => "↑", "downarrow" => "↓",
        "in" => "∈", "notin" => "∉", "ni" => "∋", "subset" => "⊂", "supset" => "⊃",
        "subseteq" => "⊆", "supseteq" => "⊇", "cup" => "∪", "cap" => "∩",
        "emptyset" => "∅", "varnothing" => "∅", "setminus" => "∖",
        "forall" => "∀", "exists" => "∃", "nexists" => "∄", "neg" => "¬", "lnot" => "¬",
        "land" => "∧", "wedge" => "∧", "lor" => "∨", "vee" => "∨",
        "sum" => "Σ", "prod" => "Π", "int" => "∫", "iint" => "∬", "iiint" => "∭",
        "oint" => "∮", "bigcup" => "⋃", "bigcap" => "⋂",
        // 文字函数:两侧留空隙,对应 TeX 的算子间距。
        "sin" => " sin ", "cos" => " cos ", "tan" => " tan ", "log" => " log ", "ln" => " ln ",
        "exp" => " exp ", "lim" => " lim ", "max" => " max ", "min" => " min ", "sup" => " sup ",
        "inf" => " inf ", "arg" => " arg ", "det" => " det ", "gcd" => " gcd ", "mod" => " mod ",
        "bmod" => " mod ", "pmod" => " mod ",
        "ldots" => "…", "cdots" => "⋯", "dots" => "…", "vdots" => "⋮", "ddots" => "⋱",
        "prime" => "′", "dagger" => "†", "ddagger" => "‡", "star" => "⋆", "ast" => "*",
        "oplus" => "⊕", "otimes" => "⊗", "ominus" => "⊖", "odot" => "⊙",
        "perp" => "⊥", "parallel" => "∥", "angle" => "∠", "triangle" => "△",
        "top" => "⊤", "bot" => "⊥", "vdash" => "⊢", "dashv" => "⊣", "models" => "⊨",
        "hbar" => "ℏ", "ell" => "ℓ", "Re" => "ℜ", "Im" => "ℑ", "aleph" => "ℵ",
        "wp" => "℘", "degree" => "°", "prec" => "≺", "succ" => "≻",
        "langle" => "⟨", "rangle" => "⟩", "lceil" => "⌈", "rceil" => "⌉",
        "lfloor" => "⌊", "rfloor" => "⌋", "|" => "‖", "colon" => ":",
        _ => return None,
    })
}

#[cfg(test)]
mod unicode_tests {
    use super::unicode_math;

    #[test]
    fn converts_common_inline_formulas() {
        assert_eq!(unicode_math(r"E=mc^2"), "E=mc²");
        assert_eq!(unicode_math(r"x_{n+1}=x_n-\frac{f(x_n)}{f'(x_n)}"), "xₙ₊₁=xₙ-(f(xₙ))/(f′(xₙ))");
        assert_eq!(unicode_math(r"q=\frac{1+\sqrt5}{2}\approx 1.618"), "q=(1+√5)/2≈1.618");
        assert_eq!(unicode_math(r"\alpha\in(0,1)"), "α∈(0,1)");
        assert_eq!(unicode_math(r"\sqrt{d_k}"), "√dₖ");
        assert_eq!(unicode_math(r"O(n\log n)"), "O(n log n)");
        assert_eq!(unicode_math(r"\operatorname{softmax}(z)_i"), "softmax(z)ᵢ");
        assert_eq!(unicode_math(r"a^{-1}b^{2n}"), "a⁻¹b²ⁿ");
        assert_eq!(unicode_math(r"\sum_{i=1}^{N} x_i"), "Σᵢ₌₁^N xᵢ");
        assert_eq!(unicode_math(r"90\%"), "90%");
    }

    #[test]
    fn unknown_commands_stay_verbatim() {
        assert_eq!(unicode_math(r"\weirdcmd{x}"), "\\weirdcmdx");
        assert_eq!(unicode_math(r"AT^{\top}"), "ATᵀ");
    }
}

#[cfg(all(test, unix))]
mod pty_tests {
    use super::*;

    /// 备忘录规矩:终端输出必须过 PTY 测具。半块画只该含 SGR 与文本,
    /// 写入真 PTY 后 termios 四组模式标志与本地控制字符必须原封不动。
    #[test]
    fn halfblock_output_preserves_pty_termios() {
        unsafe {
            let mut master: libc::c_int = 0;
            let mut slave: libc::c_int = 0;
            let ok = libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            assert_eq!(ok, 0, "openpty failed");

            let mut before: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(slave, &mut before), 0);

            let art = render_math(
                r"\int_{-\infty}^{\infty} e^{-x^2}\,dx=\sqrt{\pi}",
                MathMode::Block,
                8,
                80,
            )
            .expect("gauss integral renders");
            let payload = art.lines.join("\r\n") + "\r\n";
            // 非阻塞排空 master,防止写满 PTY 缓冲。
            let flags = libc::fcntl(master, libc::F_GETFL);
            libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
            let bytes = payload.as_bytes();
            let written = libc::write(slave, bytes.as_ptr().cast(), bytes.len());
            assert!(written > 0, "pty write failed");
            let mut sink = [0u8; 65536];
            while libc::read(master, sink.as_mut_ptr().cast(), sink.len()) > 0 {}

            let mut after: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(slave, &mut after), 0);
            assert_eq!(before.c_iflag, after.c_iflag, "input modes changed");
            assert_eq!(before.c_oflag, after.c_oflag, "output modes changed");
            assert_eq!(before.c_cflag, after.c_cflag, "control modes changed");
            assert_eq!(before.c_lflag, after.c_lflag, "local modes changed");
            assert_eq!(before.c_cc, after.c_cc, "control characters changed");

            libc::close(master);
            libc::close(slave);
        }
    }

    /// 半块画不得夹带任何 CSI 私有模式/终端状态序列:
    /// 只允许 SGR(以 m 结尾的 CSI)。
    #[test]
    fn halfblock_output_contains_only_sgr_escapes() {
        let art = render_math(r"E=mc^2", MathMode::Block, 6, 60).expect("renders");
        for line in &art.lines {
            let bytes: Vec<char> = line.chars().collect();
            let mut index = 0;
            while index < bytes.len() {
                if bytes[index] == '\u{1b}' {
                    assert_eq!(bytes.get(index + 1), Some(&'['), "non-CSI escape found");
                    let mut probe = index + 2;
                    while probe < bytes.len()
                        && (bytes[probe].is_ascii_digit() || bytes[probe] == ';')
                    {
                        probe += 1;
                    }
                    assert_eq!(bytes.get(probe), Some(&'m'), "non-SGR CSI found: {line}");
                    index = probe + 1;
                } else {
                    index += 1;
                }
            }
        }
    }
}

// ─────────────── 块级高清:复用 print_image 的 Kitty 管线 ───────────────
// tools/kitty_image 的 Unicode-placeholder 模式(U=1)已在生产使用:
// 占位行就是真文本行,与流式重绘/tmux 天然兼容。公式 PNG 直接走它。

pub(crate) struct KittyMath {
    pub sequence: String,
}

/// kitty 家族终端(原生 kitty / ghostty)才用图形协议;其余走半块。
pub(crate) fn kitty_graphics_supported() -> bool {
    crate::tools::kitty_image::is_native_kitty_terminal()
        || std::env::var("TERM").map(|term| term.contains("ghostty")).unwrap_or(false)
        || std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
}

/// 块级公式 → Kitty 序列(占位行自带换行)。失败回 None 走半块/原文。
///
/// 尺寸走"自然大小"而非看图的撑满语义:公式以 2x 密度渲染,按
/// retina 语义折半放置(清晰且只占 2~6 行);超宽才等比缩小。
pub(crate) fn render_math_kitty(tex: &str, max_cols: usize) -> Option<KittyMath> {
    let png = ratex_png(tex, MathMode::Block)?;
    let raster = decode_and_trim(&png)?;
    let (cell_w, cell_h) = crate::tools::kitty_image::cell_pixel_size();
    let (cell_w, cell_h) = (usize::from(cell_w.max(1)), usize::from(cell_h.max(1)));
    let max_cols = max_cols.clamp(8, 200);
    // 纯 retina 语义:显示尺寸 = 内容像素 ÷ 2,行数随内容高低自然分配
    // (简单公式 1~2 行,积分/矩阵自然更高);超宽才等比缩,上限 8 行防爆。
    let display_w = raster.width.div_ceil(2);
    let display_h = raster.height.div_ceil(2);
    let mut cols = display_w.div_ceil(cell_w).max(1);
    let mut rows = display_h.div_ceil(cell_h).clamp(1, 8);
    if cols > max_cols {
        rows = (rows * max_cols).div_ceil(cols).max(1);
        cols = max_cols;
    }
    // 画布 = 2x 网格像素,内容居中不拉伸;传输层 thumbnail 恰好折半,
    // kitty 放置端图与网格像素一致,不再二次缩放。
    let grid_w = cols * cell_w * 2;
    let grid_h = rows * cell_h * 2;
    let scale = (grid_w as f64 / raster.width as f64)
        .min(grid_h as f64 / raster.height as f64)
        .min(1.0);
    let draw_w = ((raster.width as f64 * scale) as usize).max(1);
    let draw_h = ((raster.height as f64 * scale) as usize).max(1);
    let offset_x = 0usize; // 水平靠左(调用方已缩进)
    let offset_y = (grid_h - draw_h) / 2;
    let mut padded = image::RgbaImage::new(grid_w as u32, grid_h as u32);
    for y in 0..draw_h {
        for x in 0..draw_w {
            let pixel = sample(&raster, x, y, draw_w, draw_h);
            padded.put_pixel((offset_x + x) as u32, (offset_y + y) as u32, image::Rgba(pixel));
        }
    }
    let sequence = crate::tools::kitty_image::kitty_sequence_with_grid(
        &image::DynamicImage::ImageRgba8(padded),
        u16::try_from(cols).unwrap_or(80),
        u16::try_from(rows).unwrap_or(4),
    )
    .ok()?;
    Some(KittyMath { sequence })
}

// ─────────────── 表格/多行:二维文本分式(上·横线·下) ───────────────
// 表格单元格支持多行,分式排成真正的上下结构:
//   ∂f
//   ──   其余元素按基线(横线行)对齐水平拼接;嵌套分式递归。
//   ∂x

struct MathTextBox {
    lines: Vec<String>,
    baseline: usize,
    width: usize,
}

impl MathTextBox {
    fn text(content: &str) -> Self {
        let width = text_display_width(content);
        Self { lines: vec![content.to_string()], baseline: 0, width }
    }

    fn empty() -> Self {
        Self { lines: vec![String::new()], baseline: 0, width: 0 }
    }
}

fn text_display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| if ch.is_ascii() { 1 } else if (ch as u32) >= 0x2e80 { 2 } else { 1 })
        .sum()
}

fn pad_to_width(line: &str, width: usize) -> String {
    let current = text_display_width(line);
    format!("{line}{}", " ".repeat(width.saturating_sub(current)))
}

fn center_to_width(line: &str, width: usize) -> String {
    let current = text_display_width(line);
    let total = width.saturating_sub(current);
    let left = total / 2;
    format!("{}{}{}", " ".repeat(left), line, " ".repeat(total - left))
}

fn hcat(left: MathTextBox, right: MathTextBox) -> MathTextBox {
    if left.width == 0 && left.lines.len() == 1 && left.lines[0].is_empty() {
        return right;
    }
    let above = left.baseline.max(right.baseline);
    let below = (left.lines.len() - left.baseline).max(right.lines.len() - right.baseline);
    let mut lines = Vec::with_capacity(above + below);
    for row in 0..(above + below) {
        let left_row = (row + left.baseline).checked_sub(above).and_then(|i| left.lines.get(i));
        let right_row = (row + right.baseline).checked_sub(above).and_then(|i| right.lines.get(i));
        let mut line = pad_to_width(left_row.map(String::as_str).unwrap_or(""), left.width);
        line.push_str(&pad_to_width(right_row.map(String::as_str).unwrap_or(""), right.width));
        lines.push(line);
    }
    MathTextBox { lines, baseline: above, width: left.width + right.width }
}

fn frac_box(numerator: MathTextBox, denominator: MathTextBox) -> MathTextBox {
    let width = numerator.width.max(denominator.width).max(1) + 2;
    let mut lines = Vec::new();
    for line in &numerator.lines {
        lines.push(center_to_width(line, width));
    }
    let baseline = lines.len();
    lines.push("─".repeat(width));
    for line in &denominator.lines {
        lines.push(center_to_width(line, width));
    }
    MathTextBox { lines, baseline, width }
}

/// 多行转写入口:含 `\frac` 的公式排成上下结构,单行公式与
/// [`unicode_math`] 输出一致。返回 lines(尾空格已修剪)。
pub(crate) fn unicode_math_lines(tex: &str) -> Vec<String> {
    let chars: Vec<char> = tex.chars().collect();
    if max_brace_depth(&chars) > MAX_MATH_NESTING {
        return tex.lines().map(|line| line.to_string()).collect();
    }
    let mut cursor = 0usize;
    let boxed = sequence_box(&chars, &mut cursor, None);
    boxed
        .lines
        .into_iter()
        .map(|line| line.trim_end().to_string())
        .collect()
}

fn sequence_box(chars: &[char], cursor: &mut usize, stop: Option<char>) -> MathTextBox {
    let mut result = MathTextBox::empty();
    let mut run = String::new();
    macro_rules! flush_run {
        () => {
            if !run.is_empty() {
                let collapsed = run.split_whitespace().collect::<Vec<_>>().join(" ");
                let piece = if run.starts_with(' ') && !collapsed.is_empty() {
                    format!(" {collapsed}")
                } else {
                    collapsed
                };
                let piece = if run.ends_with(' ') && !piece.is_empty() {
                    format!("{piece} ")
                } else {
                    piece
                };
                result = hcat(result, MathTextBox::text(&piece));
                run.clear();
            }
        };
    }
    while *cursor < chars.len() {
        let ch = chars[*cursor];
        if Some(ch) == stop {
            *cursor += 1;
            flush_run!();
            return result;
        }
        match ch {
            '\\' => {
                let saved = *cursor;
                *cursor += 1;
                if let Some(name) = peek_command_name(chars, cursor) {
                    if name == "frac" || name == "dfrac" || name == "tfrac" {
                        flush_run!();
                        let numerator = group_box(chars, cursor);
                        let denominator = group_box(chars, cursor);
                        result = hcat(result, frac_box(numerator, denominator));
                        continue;
                    }
                }
                *cursor = saved + 1;
                run.push_str(&convert_command(chars, cursor));
            }
            '^' => {
                *cursor += 1;
                let script = read_group(chars, cursor);
                run.push_str(&to_script(&script, SUPERSCRIPTS, '^'));
            }
            '_' => {
                *cursor += 1;
                let script = read_group(chars, cursor);
                run.push_str(&to_script(&script, SUBSCRIPTS, '_'));
            }
            '{' => {
                *cursor += 1;
                flush_run!();
                let inner = sequence_box(chars, cursor, Some('}'));
                result = hcat(result, inner);
            }
            '\'' => {
                *cursor += 1;
                run.push('′');
            }
            '~' => {
                *cursor += 1;
                run.push(' ');
            }
            _ => {
                *cursor += 1;
                run.push(ch);
            }
        }
    }
    flush_run!();
    result
}

/// 只窥探命令名(字母串+吃尾空格),游标停在参数处。
fn peek_command_name(chars: &[char], cursor: &mut usize) -> Option<String> {
    let start = *cursor;
    while chars.get(*cursor).is_some_and(|ch| ch.is_ascii_alphabetic()) {
        *cursor += 1;
    }
    if *cursor == start {
        return None;
    }
    let name: String = chars[start..*cursor].iter().collect();
    if chars.get(*cursor) == Some(&' ') {
        *cursor += 1;
    }
    Some(name)
}

fn group_box(chars: &[char], cursor: &mut usize) -> MathTextBox {
    match chars.get(*cursor) {
        Some('{') => {
            *cursor += 1;
            sequence_box(chars, cursor, Some('}'))
        }
        Some('\\') => {
            *cursor += 1;
            MathTextBox::text(&convert_command(chars, cursor))
        }
        Some(ch) => {
            let piece = ch.to_string();
            *cursor += 1;
            MathTextBox::text(&piece)
        }
        None => MathTextBox::empty(),
    }
}

#[cfg(test)]
mod box_tests {
    use super::*;

    #[test]
    fn deep_nesting_falls_back_to_raw_text() {
        // 10 万层 `{` 的递归转写会栈溢出 abort;超过深度上限必须原样返回。
        let bomb = "{".repeat(100_000);
        assert_eq!(unicode_math(&bomb), bomb);
        assert_eq!(unicode_math_lines(&bomb), vec![bomb.clone()]);
        // 上限内的正常嵌套不受影响。
        assert_eq!(unicode_math("{{{x}}}"), "x");
    }

    #[test]
    fn frac_parenthesizes_compound_numerators() {
        // `(a)+(b)` 两端恰是括号但不是一对:必须整体加括号。
        assert_eq!(unicode_math(r"\frac{(a)+(b)}{c}"), "((a)+(b))/c");
        // 真正整体被括号包裹的不重复加括号。
        assert_eq!(unicode_math(r"\frac{(a+b)}{c}"), "(a+b)/c");
    }

    #[test]
    fn frac_stacks_vertically_with_rule() {
        let lines = unicode_math_lines(r"\frac{\partial f}{\partial x} = 0");
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains("∂f"));
        assert!(lines[1].starts_with('─'));
        assert!(lines[1].contains("─── ="), "baseline row carries the rest: {lines:?}");
        assert!(lines[2].contains("∂x"));
    }

    #[test]
    fn nested_frac_and_plain_formula() {
        let lines = unicode_math_lines(r"x=\frac{-b\pm\sqrt{b^2-4ac}}{2a}");
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains("-b±√(b²-4ac)"), "{lines:?}");
        assert!(lines[2].contains("2a"));
        let single = unicode_math_lines(r"E=mc^2");
        assert_eq!(single, vec!["E=mc²".to_string()]);
    }
}
