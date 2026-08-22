//! 会话级操作：清空内容、重置人格状态。
//!
//! 这些操作要同时动状态库、记忆、平台侧的会话映射，所以既不属于 web 也
//! 不属于某个平台。
// 兄弟模块的类型互相引用（DaemonState 持有 EventHub、run 记录引用
// ManagerState 等），统一从 mod.rs 的再导出取，免得每个文件维护一份
// 交叉导入清单。
use super::*;
use crate::config::AppConfig;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

// ── clear_platform_session_content / reset_platform_persona_state ──
pub(crate) async fn clear_platform_session_content(
    state: &DaemonState,
    session_id: Arc<str>,
) -> std::result::Result<(), PlatformSessionResetError> {
    state
        .state_store
        .recover_stale_turns()
        .map_err(|error| PlatformSessionResetError::Internal(safe_error_message(error)))?;
    {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_busy || manager.session_has_runs(&session_id) {
            return Err(PlatformSessionResetError::Busy);
        }
        manager.admin_busy = true;
    }

    let target = state.state_store.pinned(&session_id);
    match target.has_running_turns() {
        Ok(false) => {}
        Ok(true) => {
            release_admin(&state.manager);
            return Err(PlatformSessionResetError::Busy);
        }
        Err(error) => {
            release_admin(&state.manager);
            return Err(PlatformSessionResetError::Internal(safe_error_message(
                error,
            )));
        }
    }

    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::ClearSessionContent { session_id, reply })
        .is_err()
    {
        release_admin(&state.manager);
        return Err(PlatformSessionResetError::Unavailable);
    }
    match receiver.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => {
            Err(PlatformSessionResetError::Internal(message))
        }
        Err(_) => {
            release_admin(&state.manager);
            Err(PlatformSessionResetError::Unavailable)
        }
    }
}

pub(crate) async fn reset_platform_persona_state(
    state: &DaemonState,
    config: &AppConfig,
) -> std::result::Result<usize, PlatformPersonaResetError> {
    let persona = config.active_persona_scope();
    let session_ids = state
        .state_store
        .persona_reset_session_ids(&persona, "onebot")
        .map_err(|error| PlatformPersonaResetError::Internal(safe_error_message(error)))?;
    let bindings = state
        .state_store
        .platform_session_bindings(&persona, "onebot")
        .map_err(|error| PlatformPersonaResetError::Internal(safe_error_message(error)))?;
    let targets = session_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();

    {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_busy {
            return Err(PlatformPersonaResetError::Busy);
        }
        manager.admin_busy = true;
        for run in manager
            .active_runs
            .values()
            .filter(|run| targets.contains(&*run.session_id))
        {
            run.request_cancel();
        }
    }

    let tickets = session_ids
        .iter()
        .map(|session_id| state.platforms.preempt_session_turns(session_id))
        .collect::<Vec<_>>();
    let leases = match tokio::time::timeout(Duration::from_secs(10), async {
        let mut leases = Vec::with_capacity(tickets.len());
        for ticket in tickets {
            leases.push(ticket.acquire().await.expect("exclusive platform ticket"));
        }
        leases
    })
    .await
    {
        Ok(leases) => leases,
        Err(_) => {
            release_admin(&state.manager);
            return Err(PlatformPersonaResetError::Busy);
        }
    };

    for _ in 0..200 {
        let running = state
            .manager
            .lock()
            .unwrap()
            .active_runs
            .values()
            .any(|run| targets.contains(&*run.session_id));
        if !running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    if state
        .manager
        .lock()
        .unwrap()
        .active_runs
        .values()
        .any(|run| targets.contains(&*run.session_id))
    {
        drop(leases);
        release_admin(&state.manager);
        return Err(PlatformPersonaResetError::Busy);
    }

    let plugins = match state.platforms.plugins() {
        Ok(plugins) => plugins,
        Err(error) => {
            drop(leases);
            release_admin(&state.manager);
            return Err(PlatformPersonaResetError::Internal(safe_error_message(
                error,
            )));
        }
    };
    let reset_context = crate::platforms::plugins::PlatformPersonaResetContext {
        config,
        paths: &state.paths,
        bindings: &bindings,
    };
    if let Err(error) = plugins.after_persona_reset(&reset_context).await {
        drop(leases);
        release_admin(&state.manager);
        return Err(PlatformPersonaResetError::Internal(safe_error_message(
            error,
        )));
    }

    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::ResetPersonaState {
            config: Box::new(config.clone()),
            reply,
        })
        .is_err()
    {
        drop(leases);
        release_admin(&state.manager);
        return Err(PlatformPersonaResetError::Unavailable);
    }
    let result = match receiver.await {
        Ok(Ok(())) => Ok(session_ids.len()),
        Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => {
            Err(PlatformPersonaResetError::Internal(message))
        }
        Err(_) => {
            release_admin(&state.manager);
            Err(PlatformPersonaResetError::Unavailable)
        }
    };
    drop(leases);
    result
}
