//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/vision/print.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]

/// 百分比按什么算。
///
/// 全屏下按**正文区**，不是整屏：正文左右有页边距、下边压着活动区，按整屏的
/// 百分比算出来的格子数会比实际能放的多，一张图就能把输入框顶出屏幕。
pub fn display_grid() -> Option<(u16, u16)> {
    // Unit tests run without a controlling terminal on CI.
    Some((80, 24))
}
