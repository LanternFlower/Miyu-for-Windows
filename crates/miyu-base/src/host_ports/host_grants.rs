//! 宿主能力授权(09-16 接口治理):脚本这类进程外扩展要拿宿主信息,不能拿 WebUI
//! 管理员 token,也不能自报身份——宿主在拉起它时签一张一次性令牌写进环境变量,
//! 令牌只在这一次运行期间有效,进程退出(守卫掉落)即作废。daemon 收到
//! `HostQuery` 时按令牌找授权集,方法要的能力不在集里就是 permission_denied。
//!
//! 只有 daemon 签发(`enable_host_grants` 在 `web::server` 启动时调):REPL 直连、
//! `miyu run` 这类非 daemon 进程里 `issue_host_grant` 恒为 `None`,脚本拿不到令牌,
//! 它的 `miyu host` 调用也就明确失败,而不是连到一个不认识令牌的 daemon 上。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// 宿主能向扩展授予的能力 id。包管理器的 `requires-capabilities` 预检与脚本头部
/// 的 `Capabilities:` 都只认这张表。
pub const HOST_CAPABILITIES: &[&str] = &["host.info", "providers.read", "subsystems.read"];

pub fn is_known_capability(id: &str) -> bool {
    HOST_CAPABILITIES.contains(&id)
}

static ISSUING: AtomicBool = AtomicBool::new(false);

fn grants() -> &'static Mutex<HashMap<String, Vec<String>>> {
    static GRANTS: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
    GRANTS.get_or_init(Mutex::default)
}

/// daemon 启动时打开签发;之前 / 之外一律不签。
pub fn enable_host_grants() {
    ISSUING.store(true, Ordering::Release);
}

/// 一次运行的授权守卫:掉落即作废令牌。
pub struct HostGrantGuard {
    token: String,
}

impl HostGrantGuard {
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for HostGrantGuard {
    fn drop(&mut self) {
        grants().lock().unwrap().remove(&self.token);
    }
}

/// 给一次扩展运行签令牌。能力集为空或不在 daemon 里 → `None`。
pub fn issue_host_grant(capabilities: &[String]) -> Option<HostGrantGuard> {
    if capabilities.is_empty() || !ISSUING.load(Ordering::Acquire) {
        return None;
    }
    let token = super::random_token(24);
    grants()
        .lock()
        .unwrap()
        .insert(token.clone(), capabilities.to_vec());
    Some(HostGrantGuard { token })
}

/// 令牌对应的能力集;不认识或已作废 → `None`。
pub fn host_grant_capabilities(token: &str) -> Option<Vec<String>> {
    grants().lock().unwrap().get(token).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_live_exactly_as_long_as_the_guard() {
        enable_host_grants();
        assert!(issue_host_grant(&[]).is_none(), "空能力集不签");
        let guard = issue_host_grant(&["providers.read".to_string()]).expect("daemon 内签发");
        let token = guard.token().to_string();
        assert_eq!(
            host_grant_capabilities(&token).as_deref(),
            Some(&["providers.read".to_string()][..])
        );
        drop(guard);
        assert!(host_grant_capabilities(&token).is_none(), "守卫掉落即作废");
        assert!(host_grant_capabilities("made-up").is_none());
    }

    #[test]
    fn capability_vocabulary_is_closed() {
        assert!(is_known_capability("providers.read"));
        assert!(!is_known_capability("providers.write"));
    }
}
