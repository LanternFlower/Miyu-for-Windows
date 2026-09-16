//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/embedding/manifest.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]

pub const DEFAULT_LOCAL_MODEL: &str = "bge-small-zh-v1.5-int8";
