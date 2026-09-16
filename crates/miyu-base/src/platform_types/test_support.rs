//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/platform_types.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ResponseTarget {
    pub fn quoted(message_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            user_id: user_id.into(),
            quote: true,
            mention: false,
            explicit_mention_user_ids: Vec::new(),
        }
    }
}
