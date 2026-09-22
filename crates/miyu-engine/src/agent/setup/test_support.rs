//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/agent/setup.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl Agent {
    /// Stops the idle cache-keepalive loop (called whenever a new request is
    /// about to change the context, and before dropping the agent).
    /// 测试用：塞一份请求快照，好让 `start_cache_keepalive` 真的起得来
    /// （它没有快照就直接返回）。
    pub fn seed_request_snapshot_for_test(&mut self) {
        self.runtime.last_request_snapshot = Some((vec![ChatMessage::system("probe")], Vec::new()));
    }

    /// 测试用：拿到取消标志，好在 `Agent` 被丢掉之后验证它确实被翻了。
    pub fn keepalive_cancel_flag(&self) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        self.runtime.keepalive_cancel.clone()
    }
}
