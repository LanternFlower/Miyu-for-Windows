//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/cli/repl/tail/screen/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl Screen {
    /// 不碰终端的构造，只给测试用。视图映射那套行号算术值得单独钉住——
    /// 它算错一行的表现是「点哪儿选中的都是上一行」，从画面上很难看出来。
    pub(in crate::cli) fn detached(cols: u16, rows: u16) -> Self {
        Self {
            term: Term::default(),
            scroll: 0,
            follow: true,
            painted: Vec::new(),
            cols,
            rows,
            suspended: false,
            selection: None,
            pending_copy: None,
            needs_clear: true,
            overlay_spinner_started: None,
            body: None,
            expanded: std::collections::HashMap::new(),
            overlay: None,
            hover: None,
            input_rows: Vec::new(),
            input_selection: None,
            input_dragging: false,
            toast: None,
            floor: 0,
            row_keys: Vec::new(),
            command_hint: Vec::new(),
            hint_dismissed: false,
            force: true,
            banner: None,
            float_anchor: None,
        }
    }

    /// 测试入口：走的是和实况**同一条**路（含图形分流、行数封顶）。
    pub(in crate::cli) fn feed_for_test(&mut self, bytes: &[u8]) {
        self.feed(bytes);
    }
}
