//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/render/math/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

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
