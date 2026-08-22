//! 渲染测试共用的 fixture。

use crate::platforms::plugins::renderer::*;

pub(super) fn render(markdown: &str, raw_config: &RenderConfig) -> Result<Vec<RenderedImage>> {
    render_in_process_for_test(markdown, raw_config)
}
