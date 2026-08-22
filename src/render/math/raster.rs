//! 公式图片的栅格化与半块渲染。
//!
//! `halfblock_*` 用 ▀ 加前景/背景色，一个字符格放两行像素——终端不支持图形协议
//! 时这是最接近图片的表现。
//!
//! `silence_first_font_load` 抑制字体库首次加载往 stderr 打的噪声。验证这条修复
//! 时要用**带中文的公式**：纯数学公式根本不会加载会说话的那个回退字体，拿它测
//! 等于拿一个恒为绿的用例发合格证（见 AGENTS.md 2.3）。

use crate::render::math::*;

/// 公式的半块画:每行等显示宽(`cols` 列),行内含 24-bit ANSI 与末尾复位。
pub(crate) struct MathArt {
    pub lines: Vec<String>,
    pub cols: usize,
}

/// 公式字色:适配深色终端的雾蓝(与 WebUI 主题同源);亮色终端下亦可辨。
pub(crate) const MATH_COLOR: (f32, f32, f32) = (0.843, 0.890, 1.0); // #d7e3ff

/// alpha 低于此值的像素视为背景,不上色。
pub(crate) const ALPHA_THRESHOLD: u8 = 24;

/// RaTeX 纯 Rust 管线:解析→排版→PNG(透明底)。解析/渲染失败返回 None。
pub(crate) fn ratex_png(tex: &str, mode: MathMode) -> Option<Vec<u8>> {
    let normalized = tex.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    silence_first_font_load();
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

/// 吞掉 RaTeX 首次加载字体时的噪声。
///
/// `ratex-unicode-font` 0.1.14 找到字体就无条件 `eprintln!`，没有 env 开关
/// 也没有 feature 门——连读 `RATEX_UNICODE_FONT` 那条分支也照打。字体是
/// `OnceLock` 缓存的、每进程只加载一次，于是进 REPL 回放带公式的历史时，
/// 屏幕上就会冒出两行 `[ratex-unicode-font] found via system-fonts: …`
/// （daemon 与 CLI 各一次，两个进程共用同一个终端）。
///
/// 做法是趁第一次渲染，把 fd 2 临时指向 /dev/null 让它把话说完，之后缓存
/// 已经填上，再也不会响。重定向 fd 是进程级副作用，所以只在这一次、且只
/// 在这一小段窗口里做；后续每一次渲染都不再碰 fd 2。
pub(crate) fn silence_first_font_load() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        #[cfg(unix)]
        unsafe {
            let saved = libc::dup(libc::STDERR_FILENO);
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
            if null >= 0 {
                libc::dup2(null, libc::STDERR_FILENO);
            }
            // 一个最小公式足以把三张字体缓存都填上。
            let _ = warm_up_font_caches();
            if saved >= 0 {
                libc::dup2(saved, libc::STDERR_FILENO);
                libc::close(saved);
            }
            if null >= 0 {
                libc::close(null);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = warm_up_font_caches();
        }
    });
}

/// 渲染一个最小公式，只为触发字体加载；产物丢弃。
///
/// 必须带 CJK。会说话的是**Unicode 回退字体**，纯 ASCII 公式压根用不上它
/// ——第一版预热写的是 `parse("x")`，缓存没填上，真正带中文的公式一来照样
/// 打印两行。实测只有 `\text{中文}` 这条会触发（两张缓存各打一次）。
pub(crate) fn warm_up_font_caches() -> Option<Vec<u8>> {
    let ast = parse(r"\text{中}").ok()?;
    let color = Color {
        r: MATH_COLOR.0,
        g: MATH_COLOR.1,
        b: MATH_COLOR.2,
        a: 1.0,
    };
    let layout_opts = LayoutOptions::default()
        .with_style(MathStyle::Text)
        .with_color(color);
    let render_opts = RenderOptions {
        font_size: 12.0,
        padding: 0.0,
        background_color: Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        },
        font_dir: String::new(),
        device_pixel_ratio: 1.0,
    };
    let layout_box = layout(&ast, &layout_opts);
    render_to_png(&to_display_list(&layout_box), &render_opts).ok()
}

pub(crate) struct Raster {
    pub(crate) pixels: Vec<[u8; 4]>,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

impl Raster {
    pub(crate) fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        self.pixels[y * self.width + x]
    }
}

pub(crate) fn decode_and_trim(png: &[u8]) -> Option<Raster> {
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
pub(crate) fn sample(raster: &Raster, x: usize, y: usize, target_width: usize, target_height: usize) -> [u8; 4] {
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

/// 非 kitty 终端交给 chafa 渲染公式。
///
/// 半块渲染只有 `▀ ▄ 空格` 三个字形、每格两个垂直采样，把高清 PNG 压进去
/// 必然碎成像素块。而 chafa 认得的终端多得多——Konsole、WezTerm、foot、
/// iTerm2 之流都能出真图，图片工具一直走的就是这条路（「Konsole 明明能显
/// 示图片」正是这么来的），公式却一直没走。
///
/// 关键是 `--probe-mode ctty`：chafa 经**控制终端**探测能力，所以这里把它
/// 的 stdout 捕获下来也不影响判断——原先以为「一捕获就探测不到」是错的。
///
/// `--polite on` 去掉隐藏/显示光标的转义（要嵌进流里，不能乱动光标）；
/// 图形格式下 chafa 用 IND(`ESC D`) 推行而不是换行，渲染层按行记账会少算
/// N-1 行，所以换回换行——两者都是「下移一行」，IND 前面那个 `ESC[nD` 回
/// 到行首的动作留着也无害。
pub(crate) fn render_math_chafa(tex: &str, max_cols: usize, max_rows: usize) -> Option<MathArt> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    // 输出不是终端就别起进程:管道、重定向、测试都走不到真终端,chafa 探测
    // 不到能力只会退回符号画,还白付一次进程开销。
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        return None;
    }
    let png = ratex_png(tex, MathMode::Block)?;
    let rows = natural_block_rows(&decode_and_trim(&png)?, max_rows);
    let mut child = Command::new("chafa")
        .args([
            "--relative",
            "off",
            "--polite",
            "on",
            "--probe-mode",
            "ctty",
            // RaTeX 出的是透明底,阈值放高让背景真正透出去,别被合成成一块
            // 底板。代价是抗锯齿边缘会硬一点,公式笔画本身影响很小。
            "-t",
            "0.9",
            "--size",
            &format!("{max_cols}x{rows}"),
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(&png).ok()?;
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).replace("\u{1b}D", "\n");
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    // 末尾那个换行切出来的空段不算一行。
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }
    // 行推进随格式而变，必须补齐到实际占用的行数：
    //
    //   kitty    IND×(N-1) + 换行×1 = N   够了
    //   sixel    0                        整图一个 blob，一行都不推
    //   iterm    换行×1                   同上
    //   symbols  换行×N                   够了
    //
    // 不补的话渲染层以为公式只占一行，后续文字就直接写到图上——Konsole 里
    // 高的公式压住下一行标题正是这么来的。补空行让终端和渲染层的行数一致。
    while lines.len() < rows {
        lines.push(String::new());
    }
    Some(MathArt {
        lines,
        cols: max_cols,
    })
}

/// 块级公式的自然行数：与 kitty 那条路同一套 retina 语义（RaTeX 以 2x 密度
/// 出图，显示尺寸取内容的一半），`max_rows` 只是天花板。
pub(crate) fn natural_block_rows(raster: &Raster, max_rows: usize) -> usize {
    let (_, cell_h) = crate::terminal::kitty::cell_pixel_size();
    let cell_h = usize::from(cell_h.max(1));
    raster
        .height
        .div_ceil(2)
        .div_ceil(cell_h)
        .clamp(1, max_rows.max(1))
}

/// 块级公式:行数随内容自然分配，`max_rows` 只是上限。
///
/// 此前这里和表格单元格共用 `render_math`，行数由调用方写死为 9，于是
/// `E=mc^2` 这种两行就够的式子也被撑到 9 行——报的「大小限制好像消失了」
/// 就是这个。而且 `halfblock_art` 只约束宽度，垂直方向没有任何上限。
///
/// 用和 kitty 那条路同一套 retina 语义：RaTeX 以 2x 密度出图，显示尺寸取
/// 内容的一半，所以简单式子 1~2 行、积分矩阵自然更高。
pub(crate) fn render_block_math(
    tex: &str,
    max_cols: usize,
    max_rows: usize,
) -> Option<MathArt> {
    let png = ratex_png(tex, MathMode::Block)?;
    let raster = decode_and_trim(&png)?;
    let rows = natural_block_rows(&raster, max_rows);
    halfblock_from_raster(&raster, rows, max_cols)
}

/// 半块化:目标高 `target_rows` 字符行(=2×像素行),宽等比、封顶 `max_cols`。
pub(crate) fn halfblock_art(png: &[u8], target_rows: usize, max_cols: usize) -> Option<MathArt> {
    let raster = decode_and_trim(png)?;
    halfblock_from_raster(&raster, target_rows, max_cols)
}

pub(crate) fn halfblock_from_raster(
    raster: &Raster,
    target_rows: usize,
    max_cols: usize,
) -> Option<MathArt> {
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
            let top = sample(raster, x, row * 2, width_px, height_px);
            let bottom = sample(raster, x, row * 2 + 1, width_px, height_px);
            line.push_str(&halfblock_cell(top, bottom));
        }
        line.push_str("\x1b[0m");
        lines.push(line);
    }
    Some(MathArt { lines, cols: width_px })
}

pub(crate) fn halfblock_cell(top: [u8; 4], bottom: [u8; 4]) -> String {
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
