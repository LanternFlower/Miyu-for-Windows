//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/cli/repl/tail/screen/ansi.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl AnsiSpan {
    pub(in crate::cli) fn raw(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::new(),
            link: None,
        }
    }
}
