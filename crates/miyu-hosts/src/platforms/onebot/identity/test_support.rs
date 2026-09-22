//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/platforms/onebot/identity.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

/// A conversation no other test shares. The delivered-image ledger is
/// process-global and keyed by conversation, so tests that reuse one account id
/// leak digests into each other and fail depending on scheduling order.
pub(in crate::platforms::onebot) fn unique_test_conversation(
    target: Target,
) -> PlatformConversation {
    static NEXT_ACCOUNT: AtomicI64 = AtomicI64::new(10_000);
    platform_conversation(target, NEXT_ACCOUNT.fetch_add(1, AtomicOrdering::Relaxed))
}
