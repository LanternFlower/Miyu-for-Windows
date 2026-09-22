//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/platforms/plugins/renderer/layout.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub(in crate::platforms::plugins::renderer) fn plan_columns(
    layouts: &[LayoutBlock],
    config: &NormalizedConfig,
) -> Result<Vec<ColumnPlan>> {
    let usable_height = config
        .max_height
        .saturating_sub(config.padding.saturating_mul(2));
    plan_columns_with_height(layouts, usable_height)
}
