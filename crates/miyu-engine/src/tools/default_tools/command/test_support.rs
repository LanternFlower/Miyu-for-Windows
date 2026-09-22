//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/default_tools/command.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl CommandTimeout {
    /// 测试用：请求多少就生效多少，不经过上限——把预算表与命令执行解耦。
    pub fn fixed(seconds: u64) -> Self {
        Self {
            requested: seconds,
            effective: seconds,
        }
    }
}
