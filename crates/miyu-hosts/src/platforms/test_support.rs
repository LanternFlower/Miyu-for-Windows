//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/platforms/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl PlatformRuntime {
    pub(crate) async fn acquire_session_turn(
        &self,
        session_id: &str,
        limits: PlatformSessionLimits,
    ) -> std::result::Result<SessionTurnLease, SessionTurnAcquireError> {
        self.session_turn_ticket(session_id, limits).acquire().await
    }
}
