//! 同会话"正在处理中"的消息登记(08-25)。
//!
//! 同群回合并发时,先到消息的回合还没答完,后到回合的历史块里就躺着一条
//! "@机器人且无人应答"的消息——这是跨线代答的最强诱饵(两天两起实锤:
//! "看不到你的截图"、拯救者重复答题)。登记表把"那条已有回合在处理"作
//! 为事实注入后到回合,拆掉诱饵而不牺牲并发。
//!
//! 生命周期用 RAII 保证:守卫挂在 PlatformTurnContext 上,回合结束(含
//! panic 路径)context 掉落即注销;TTL 兜底清理防御极端泄漏。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 单条登记的最长存活;超时视为回合已死,读取时剪除。
const INFLIGHT_TTL: Duration = Duration::from_secs(10 * 60);

fn registry() -> &'static Mutex<HashMap<String, Vec<(String, Instant)>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Vec<(String, Instant)>>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// 登记守卫:构造即登记,掉落即注销。
pub(crate) struct InflightGuard {
    scope: String,
    message_id: String,
}

impl InflightGuard {
    pub(crate) fn register(scope: String, message_id: String) -> Self {
        if !message_id.is_empty() {
            let mut map = registry().lock().unwrap();
            map.entry(scope.clone())
                .or_default()
                .push((message_id.clone(), Instant::now()));
        }
        Self { scope, message_id }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let mut map = registry().lock().unwrap();
        if let Some(entries) = map.get_mut(&self.scope) {
            if let Some(index) = entries.iter().position(|(id, _)| id == &self.message_id) {
                entries.remove(index);
            }
            if entries.is_empty() {
                map.remove(&self.scope);
            }
        }
    }
}

/// 同会话里除 `exclude_message_id` 外、仍在处理中的消息 id 列表。
pub(crate) fn other_inflight_messages(scope: &str, exclude_message_id: &str) -> Vec<String> {
    let now = Instant::now();
    let mut map = registry().lock().unwrap();
    let Some(entries) = map.get_mut(scope) else {
        return Vec::new();
    };
    entries.retain(|(_, at)| now.duration_since(*at) < INFLIGHT_TTL);
    entries
        .iter()
        .filter(|(id, _)| id != exclude_message_id)
        .map(|(id, _)| id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 守卫掉落即注销;同会话互见、跨会话隔离;自身被排除。
    #[test]
    fn guard_registers_and_unregisters_on_drop() {
        let guard_a = InflightGuard::register("scope-x".into(), "m1".into());
        let _guard_b = InflightGuard::register("scope-x".into(), "m2".into());
        let _other = InflightGuard::register("scope-y".into(), "m9".into());
        assert_eq!(other_inflight_messages("scope-x", "m2"), vec!["m1"]);
        assert!(other_inflight_messages("scope-x", "m1") == vec!["m2".to_string()]);
        drop(guard_a);
        assert!(other_inflight_messages("scope-x", "m2").is_empty());
        assert_eq!(other_inflight_messages("scope-y", ""), vec!["m9"]);
    }
}
