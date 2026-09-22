//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/llm/openai_compatible/endpoints.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ResponsesContinuationHealth {
    /// 测试用:无持久化、乐观放行。
    pub fn detached() -> Self {
        Self {
            unsupported: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            store: std::path::PathBuf::new(),
            base_url: String::new(),
            provider_id: String::new(),
        }
    }
}
