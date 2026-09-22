//! Minimal live-turn capabilities shared across process adapters.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn registry() -> &'static Mutex<HashMap<String, bool>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

pub struct LiveTurnHostToolsGuard {
    session_id: String,
}

impl LiveTurnHostToolsGuard {
    pub fn register(session_id: &str, allowed: bool) -> Self {
        if !session_id.is_empty() {
            registry()
                .lock()
                .unwrap()
                .insert(session_id.to_string(), allowed);
        }
        Self {
            session_id: session_id.to_string(),
        }
    }
}

impl Drop for LiveTurnHostToolsGuard {
    fn drop(&mut self) {
        if !self.session_id.is_empty() {
            registry().lock().unwrap().remove(&self.session_id);
        }
    }
}

pub fn live_turn_host_tools_allowed(session_id: &str) -> Option<bool> {
    registry().lock().unwrap().get(session_id).copied()
}

#[cfg(test)]
mod tests {
    use super::{live_turn_host_tools_allowed, LiveTurnHostToolsGuard};

    #[test]
    fn host_tool_capability_is_scoped_to_guard_lifetime() {
        assert_eq!(live_turn_host_tools_allowed("runtime-test"), None);
        let guard = LiveTurnHostToolsGuard::register("runtime-test", false);
        assert_eq!(live_turn_host_tools_allowed("runtime-test"), Some(false));
        drop(guard);
        assert_eq!(live_turn_host_tools_allowed("runtime-test"), None);
    }
}
