//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/cli/repl/tail/screen/overlay.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl Screen {
    /// 开合面板里第 `index` 行挂着的那一块（测试用）。
    ///
    /// `overlay_click` 要的是屏幕行，而屏幕行要等面板画过一次才算得准
    /// （高度只涨不缩，见 `Overlay::height`）；测试里没有那一次绘制。
    /// 这条走的是同一段命中与开合逻辑，只是省掉了几何换算。
    pub(in crate::cli) fn overlay_toggle(&mut self, index: usize) -> bool {
        let Some(panel) = &mut self.overlay else {
            return false;
        };
        let Some((id, _)) = layer_hit(&Layer::Body(&panel.body), &panel.expanded, index) else {
            return false;
        };
        super::super::expand::toggle_in(&mut panel.expanded, id, panel.cols)
    }

    /// 按新内容刷一遍面板（测试用）。产品里这一步在 `paint_overlay` 里做。
    pub(in crate::cli) fn overlay_refresh(&mut self) {
        if let Some(panel) = &mut self.overlay {
            panel.refresh();
        }
    }

    /// 面板里现在是哪几行，**带转义**（测试用）：颜色也要能断言。
    pub(in crate::cli) fn overlay_rows_ansi(&self) -> Vec<String> {
        let Some(panel) = &self.overlay else {
            return Vec::new();
        };
        (0..panel.len())
            .map(|index| spans_to_ansi(&panel.row(index)))
            .collect()
    }

    /// 面板里现在是哪几行（测试用）。
    pub(in crate::cli) fn overlay_rows(&self) -> Vec<String> {
        let Some(panel) = &self.overlay else {
            return Vec::new();
        };
        (0..panel.len())
            .map(|index| super::super::ansi::spans_text(&panel.row(index)))
            .collect()
    }
}

impl Screen {
    /// 在面板里拖一个选区（测试用）：`(内容行, 显示列)` 两头直接给，省掉屏幕行
    /// 到内容行的几何换算——那套换算要面板画过一次才算得准（高度只涨不缩）。
    pub(in crate::cli) fn overlay_select_span(
        &mut self,
        anchor: (usize, u16),
        cursor: (usize, u16),
    ) {
        if let Some(panel) = &mut self.overlay {
            panel.selection = Some(super::super::select::Selection {
                anchor,
                cursor,
                dragging: true,
            });
        }
    }

    /// 松手之后待写进剪贴板的那段文字（测试用）。
    pub(in crate::cli) fn take_pending_copy(&mut self) -> Option<String> {
        self.pending_copy.take()
    }
}
