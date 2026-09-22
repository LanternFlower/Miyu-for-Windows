//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/render/math/raster.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

/// 半块化:目标高 `target_rows` 字符行(=2×像素行),宽等比、封顶 `max_cols`。
pub(crate) fn halfblock_art(png: &[u8], target_rows: usize, max_cols: usize) -> Option<MathArt> {
    let raster = decode_and_trim(png)?;
    halfblock_from_raster(&raster, target_rows, max_cols)
}
