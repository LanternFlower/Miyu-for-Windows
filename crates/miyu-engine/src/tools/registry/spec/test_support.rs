//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/registry/spec.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ToolSpec {
    pub fn with_timeout_seconds(mut self, secs: u64) -> Self {
        self.timeout_seconds = Some(secs);
        self
    }
}
