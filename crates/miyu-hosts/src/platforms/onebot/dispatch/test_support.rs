//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/platforms/onebot/dispatch.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub(in crate::platforms::onebot) fn message_event(
    target: Target,
    event: &Value,
    parsed: &InboundMessage,
) -> PlatformInboundEvent {
    message_event_at(target, event, parsed, Instant::now(), None)
}

pub(in crate::platforms::onebot) async fn handle_message(
    state: DaemonState,
    conn: ConnectionHandle,
    event: Value,
    ingress_order: i64,
) {
    let self_id = event.get("self_id").and_then(Value::as_i64).unwrap_or(0);
    let activity = observe_message_activity(&state, &event, self_id, Instant::now());
    handle_message_with_activity(state, conn, event, ingress_order, activity).await;
}
