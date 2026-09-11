//! 进行中平台回合的上下文登记(08-26)。
//!
//! claude-code 供应商忽略请求里的 tools 数组,Miyu 的工具靠 MCP 桥
//! (`miyu mcp-serve` 子进程)暴露。桥只带一个 session id 回 daemon 要目录,
//! daemon 便只能给出所有者侧工具——群管理、撤回、艾特、发送这些**按平台
//! 回合注册**的工具需要一个活的 `PlatformTurnContext`(会话身份、发送者、
//! 管理员标志、适配器句柄),桥拿不到,于是在群聊里整套平台工具都不可见。
//!
//! 这张表把正在跑的平台回合的上下文按会话登记下来,桥回来问工具时能取到
//! 同一个上下文——权限判定与主线回合完全同源(同一发送者、同一会话、同一
//! 管理员状态),不是另造一套。
//!
//! 生命周期靠 RAII:守卫随回合任务存活,回合结束(含 panic 路径)即注销;
//! 存 Weak,上下文本体先掉也不会拖住内存。

use super::PlatformTurnContext;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

type Registry = Mutex<HashMap<String, Weak<PlatformTurnContext>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// 登记守卫:构造即登记,掉落即注销(只注销自己那一份)。
pub(crate) struct LiveTurnGuard {
    session_id: String,
    context: Weak<PlatformTurnContext>,
}

impl LiveTurnGuard {
    pub(crate) fn register(session_id: &str, context: &Arc<PlatformTurnContext>) -> Self {
        let weak = Arc::downgrade(context);
        if !session_id.is_empty() {
            registry()
                .lock()
                .unwrap()
                .insert(session_id.to_string(), weak.clone());
        }
        Self {
            session_id: session_id.to_string(),
            context: weak,
        }
    }
}

impl Drop for LiveTurnGuard {
    fn drop(&mut self) {
        let mut map = registry().lock().unwrap();
        // 并行回合下后来者会覆盖登记;只在这一份还是自己时才摘除,免得把
        // 后一个回合的登记误删。
        if map
            .get(&self.session_id)
            .is_some_and(|current| Weak::ptr_eq(current, &self.context))
        {
            map.remove(&self.session_id);
        }
    }
}

/// 取这条会话正在跑的平台回合上下文;没有进行中的回合返回 None。
pub(crate) fn live_turn_context(session_id: &str) -> Option<Arc<PlatformTurnContext>> {
    let mut map = registry().lock().unwrap();
    match map.get(session_id).and_then(Weak::upgrade) {
        Some(context) => Some(context),
        None => {
            map.remove(session_id);
            None
        }
    }
}
