//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/platforms/turn_context.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl PlatformTurnContext {
    pub(crate) fn response_target(&self) -> Option<ResponseTarget> {
        self.response_target
            .lock()
            .unwrap()
            .as_ref()
            .map(|pending| pending.target.clone())
    }

    pub(crate) fn take_final_reply_suppression(&self) -> bool {
        let suppress = self
            .pending_final_reply_suppression
            .swap(false, Ordering::AcqRel);
        self.pending_prior_reply_suppression
            .store(false, Ordering::Release);
        suppress
    }
}
