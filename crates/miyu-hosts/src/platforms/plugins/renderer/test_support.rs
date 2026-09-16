//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/platforms/plugins/renderer/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub(crate) fn render_in_process_for_test(
    markdown: &str,
    raw_config: &RenderConfig,
) -> Result<Vec<RenderedImage>> {
    static RENDERER: std::sync::OnceLock<std::sync::Mutex<RendererState>> =
        std::sync::OnceLock::new();
    let renderer = RENDERER.get_or_init(|| std::sync::Mutex::new(RendererState::new().unwrap()));
    let mut renderer = renderer.lock().unwrap();
    validate_markdown(markdown)?;
    let config = NormalizedConfig::new(raw_config);
    let blocks = collect_blocks(markdown);
    let palette = Palette::for_theme(&config.theme);
    renderer.render(blocks, &config, palette, markdown_contains_emoji(markdown))
}
