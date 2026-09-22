//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/render/command.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl CommandLiveDisplay {
    pub(crate) fn tick_changes_layout_at_width(&self, width: usize) -> bool {
        let next_widths = self
            .rendered_lines(width, true)
            .iter()
            .map(|line| command_ansi_width(line))
            .collect::<Vec<_>>();
        rendered_physical_rows(&self.rendered_line_widths, width)
            != rendered_physical_rows(&next_widths, width)
    }
}
