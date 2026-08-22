//! 终端 LaTeX 公式渲染:RaTeX 排版成 PNG,再压成半块(▀/▄)真彩行。
//!
//! 走半块而不是 kitty 图形协议:半块就是普通文本行,与流式渲染器的
//! 行重绘、tmux、滚动回看天然兼容,不碰终端模式。RaTeX 解析不了的
//! 公式返回 None,调用方回退到样式化源码,永不阻断输出。

mod raster;
mod unicode;
pub(crate) use raster::*;
pub(crate) use unicode::*;

use ratex_layout::{layout, to_display_list, LayoutOptions};
use ratex_parser::parser::parse;
use ratex_render::{render_to_png, RenderOptions};
use ratex_types::color::Color;
use ratex_types::math_style::MathStyle;

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

    /// 回归：块级公式此前行数写死为 9，简单式子也被撑满，而且垂直方向
    /// 没有任何上限。现在随内容自然分配，`max_rows` 只是天花板。
    #[test]
    fn block_math_rows_follow_content_and_respect_the_cap() {
        let simple = render_block_math(r"E=mc^2", 100, 8).expect("renders");
        let tall = render_block_math(
            r"\int_{0}^{\infty} \frac{x^{2}}{\sqrt{1+x^{4}}}\,dx",
            100,
            8,
        )
        .expect("renders");
        println!(
            "  E=mc^2 → {} 行 / {} 列；积分 → {} 行 / {} 列",
            simple.lines.len(),
            simple.cols,
            tall.lines.len(),
            tall.cols
        );
        // 简单式子远比写死的 9 行矮。
        assert!(
            simple.lines.len() < 5,
            "E=mc^2 占了 {} 行",
            simple.lines.len()
        );
        // 复杂式子该更高，但不越过上限。
        assert!(tall.lines.len() >= simple.lines.len());
        assert!(tall.lines.len() <= 8);
        // 上限确实是上限。
        let capped = render_block_math(
            r"\int_{0}^{\infty} \frac{x^{2}}{\sqrt{1+x^{4}}}\,dx",
            100,
            2,
        )
        .expect("renders");
        assert!(capped.lines.len() <= 2);
    }

    /// chafa 出的行数必须精确等于请求行数——渲染层按行记账，少算一行整段
    /// 布局就错位。图形格式下 chafa 用 IND(`ESC D`) 推行而不是换行，靠换回
    /// 换行才对得上。
    ///
    /// 需要真终端 + chafa，默认 ignore：
    /// `cargo test --release chafa -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a real terminal and chafa"]
    fn chafa_math_row_count_matches_the_request() {
        for max_rows in [2usize, 4, 8] {
            let art = render_math_chafa(r"\int_{0}^{\infty} e^{-x^2}\,dx", 60, max_rows)
                .expect("chafa 应当渲染成功");
            println!("  上限 {max_rows} → {} 行 / {} 列", art.lines.len(), art.cols);
            assert!(
                art.lines.len() <= max_rows,
                "上限 {max_rows} 却出了 {} 行",
                art.lines.len()
            );
            assert!(!art.lines.is_empty());
            // 光标不该被乱动:polite 模式已经掐掉隐藏/显示光标的转义。
            let joined = art.lines.join("\n");
            assert!(!joined.contains("\u{1b}[?25l"), "混进了隐藏光标");
            assert!(!joined.contains("\u{1b}D"), "还有没换掉的 IND");
        }
    }

    /// 回归：带 CJK 的公式会让 RaTeX 加载 Unicode 回退字体，而那个 crate
    /// 找到字体就无条件 `eprintln!`。预热必须用带 CJK 的公式——第一版写的
    /// 是 `parse("x")`，纯 ASCII 用不上回退字体，缓存没填上，真正带中文的
    /// 公式一来照样打印两行。
    ///
    /// 这里只断言预热本身能跑通；噪声有没有被吞掉要看 stderr，用
    /// `cargo test --release tmp -- --ignored` 那类手工对照验（实测无抑制
    /// 2 行、有抑制 0 行）。
    #[test]
    fn font_warm_up_uses_a_cjk_formula() {
        assert!(
            warm_up_font_caches().is_some(),
            "预热公式渲染失败,字体缓存填不上"
        );
        // 预热之后再渲染带中文的公式不该再触发加载。
        assert!(ratex_png(r"\text{中文}", MathMode::Block).is_some());
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
    crate::terminal::kitty::is_native_kitty_terminal()
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
    let (cell_w, cell_h) = crate::terminal::kitty::cell_pixel_size();
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
    let sequence = crate::terminal::kitty::kitty_sequence_with_grid(
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
