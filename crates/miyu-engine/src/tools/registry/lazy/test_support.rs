//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/registry/lazy.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub fn empty_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    })
}
