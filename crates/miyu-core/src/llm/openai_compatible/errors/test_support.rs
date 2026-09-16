//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/llm/openai_compatible/errors.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub const HTTP_STATUS_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(10);

pub const HTTP_STATUS_RETRY_MAX_DELAY: Duration = Duration::from_millis(120);
