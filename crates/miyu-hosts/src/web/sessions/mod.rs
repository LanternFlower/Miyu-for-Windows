//! 会话的增删改查与解析。
//!
//! 「会话引用」不等于会话 ID：前端可以传 ID、也可以传 `current` 这类别名，
//! 还要按 kind 过滤（有些接口只接受能承载回合的会话）。`resolve_local_session_ref*`
//! 这一族就是把这些形态归一到一个真实会话上，失败时给出前端能理解的错误。
//!
//! 自动命名（`maybe_auto_name_session`）放在这里而不是回合模块：它是会话的属
//! 性变更，只是恰好由第一条消息触发。

mod http;
mod state;

use crate::web::*;

pub(in crate::web) use http::*;
pub(in crate::web) use state::*;

/// 「当前会话」:管理员是 daemon 的全局指针(与 REPL 共用);成员没有全局
/// 指针,拿名下最近活跃的一条。
pub(in crate::web) fn current_session_for(
    state: &DaemonState,
    identity: &WebIdentity,
    sessions: &[miyu_core::state::SessionOverview],
) -> String {
    if identity.admin {
        return state.state_store.session_id().to_string();
    }
    sessions
        .iter()
        .max_by_key(|overview| overview.record.updated_at.clone())
        .map(|overview| overview.record.session_id.clone())
        .unwrap_or_default()
}

/// 成员的当前会话 id:没有就建一条(自动按第一句话命名)。
pub(in crate::web) fn member_current_session(
    state: &DaemonState,
    owner: &str,
) -> std::result::Result<String, String> {
    let store = state
        .stores
        .for_owner(owner)
        .map_err(|error| safe_error_message(&error))?;
    let sessions = store
        .list_owner_sessions(owner)
        .map_err(|error| safe_error_message(&error))?;
    if let Some(overview) = sessions
        .iter()
        .max_by_key(|overview| overview.record.updated_at.clone())
    {
        return Ok(overview.record.session_id.clone());
    }
    let persona = member_session_persona(state, owner);
    let record = store
        .create_session_for_owner(
            &persona,
            "",
            miyu_core::state::USER_SESSION_KIND,
            None,
            owner,
        )
        .map_err(|error| safe_error_message(&error))?;
    state.stores.note_session_owner(&record.session_id, owner);
    publish_session_created(state, &record);
    Ok(record.session_id)
}

/// 成员新会话挂哪个人格:settings 里指着自己的私有人格就用它的 scope,否则共享 Miyu。
pub(in crate::web) fn member_session_persona(state: &DaemonState, owner: &str) -> String {
    let username = state
        .state_store
        .account_by_id(owner)
        .ok()
        .flatten()
        .map(|account| account.username);
    if let Some(username) = username {
        if let Some(persona) = member_persona::active_persona(&state.paths, &username) {
            return persona.scope();
        }
    }
    active_persona_scope(state)
}

pub(in crate::web) fn publish_session_created(
    state: &DaemonState,
    record: &miyu_core::state::SessionRecord,
) {
    state.events.publish(
        "session.created",
        json!({
            "session_id": record.session_id,
            "name": record.name,
            "mode": session_mode_label(record),
        }),
    );
}

#[derive(Deserialize)]
pub(in crate::web) struct ResetConversationRequest {
    pub(in crate::web) session_id: Option<String>,
}

pub(in crate::web) fn resolve_local_session_ref(
    state: &DaemonState,
    target: &ipc::SessionRef,
) -> std::result::Result<miyu_core::state::SessionRecord, String> {
    resolve_local_session_ref_with_kinds(
        state,
        target,
        &[miyu_core::state::USER_SESSION_KIND],
        None,
    )
}

/// 桥与工具目录用的配置:与 turns/task.rs 的成员回合同源——会话归成员就把
/// 家目录(知识库/账本按人分家)与私有人格(提示词/清单/脚本白名单)套上,
/// 否则中转线(claude-code/codex/agy 只能从 MCP 桥拿工具)看到的是管理员的全量
/// 工具面:人格没勾记账也列出 ledger,勾了表情包也用不了。
pub(in crate::web) fn session_scoped_config(state: &DaemonState, session_id: &str) -> AppConfig {
    let mut config = state.manager.lock().unwrap().config.clone();
    let Some(owner) = state.stores.owner_of_session(session_id) else {
        return config;
    };
    if owner.is_empty() {
        return config;
    }
    let Ok(Some(account)) = state.state_store.account_by_id(&owner) else {
        return config;
    };
    config.accounts.home_dir = Some(
        state
            .paths
            .user_home_dir(&account.username)
            .display()
            .to_string(),
    );
    let scope = state
        .stores
        .for_session(session_id)
        .session_record(session_id)
        .ok()
        .flatten()
        .map(|record| record.persona)
        .unwrap_or_default();
    if let Some(persona) =
        member_persona::persona_for_scope(&state.paths, &account.username, &scope)
    {
        member_persona::apply_to_config(&mut config, &persona);
    }
    config
}

/// 工具桥专用的会话寻址:在本地会话之外**额外**放行"正有回合在跑"的平台
/// 会话。MCP 桥(claude-code 供应商唯一的工具通道)带的就是平台会话 id,被
/// 本地解析一律挡掉时,群聊里整套平台工具都调不到(08-26 实测 `tool-call
/// --list` 报"找不到该会话")。放行窗口卡在活回合上:回合结束登记即注销,
/// 桥也随之失去这条会话的寻址能力。
pub(in crate::web) fn resolve_tool_bridge_session_ref(
    state: &DaemonState,
    target: &ipc::SessionRef,
) -> std::result::Result<miyu_core::state::SessionRecord, String> {
    match resolve_local_session_ref_with_kinds(state, target, TURN_TARGET_KINDS, None) {
        Ok(record) => Ok(record),
        Err(error) => {
            let ipc::SessionRef::Id { id } = target else {
                return Err(error);
            };
            if crate::platforms::live_turn_context(id).is_none() {
                return Err(error);
            }
            state
                .state_store
                .session_record(id)
                .map_err(|error| safe_error_message(&error))?
                .ok_or(error)
        }
    }
}

pub(in crate::web) fn resolve_available_local_session_ref(
    state: &DaemonState,
    target: &ipc::SessionRef,
) -> std::result::Result<miyu_core::state::SessionRecord, String> {
    resolve_local_session_ref(state, target)
}

/// Turn targets and deletions additionally accept one-shot `ask` sessions.
pub(in crate::web) const TURN_TARGET_KINDS: &[&str] = &[
    miyu_core::state::USER_SESSION_KIND,
    miyu_core::state::ASK_SESSION_KIND,
    miyu_core::state::VOICE_SESSION_KIND,
];

/// Most recently updated other user session, or a fresh default session when
/// none is left.
pub(in crate::web) fn fallback_session_id(
    state: &DaemonState,
    exclude: &str,
) -> std::result::Result<String, String> {
    let persona = active_persona_scope(state);
    // 全局指针只在管理员名下的会话里挪,不能落到成员的会话上。
    let sessions = state
        .state_store
        .list_local_sessions_for_owner(&persona, "")
        .map_err(|error| safe_error_message(&error))?;
    if let Some(overview) = sessions
        .iter()
        .find(|overview| overview.record.session_id != exclude)
    {
        return Ok(overview.record.session_id.clone());
    }
    let record = state
        .state_store
        .create_session(
            &persona,
            t("Terminal session", "终端集成会话"),
            "user",
            None,
        )
        .map_err(|error| safe_error_message(&error))?;
    state.events.publish(
        "session.created",
        json!({ "session_id": record.session_id, "name": record.name }),
    );
    Ok(record.session_id)
}

/// 普通人格 + dev 保留人格的本地会话合并,按更新时间排。WebUI 侧栏与
/// `miyu session` 管理面共用:mode 字段(session_record_json)区分分组。
pub(in crate::web) fn sessions_with_dev(
    store: &StateStore,
    persona: &str,
    owner: &str,
) -> anyhow::Result<Vec<miyu_core::state::SessionOverview>> {
    // 成员(owner 非空)名下不分人格:他的会话可能挂在自己的私有人格上。
    let mut rows = if owner.is_empty() {
        store.list_local_sessions_for_owner(persona, owner)?
    } else {
        store.list_owner_sessions(owner)?
    };
    if owner.is_empty() && persona != miyu_core::state::DEV_PERSONA {
        rows.extend(store.list_local_sessions_for_owner(miyu_core::state::DEV_PERSONA, owner)?);
    }
    // 手动排序键优先(v28,越小越靠前);同键退回最近活跃。
    rows.sort_by(|a, b| {
        a.record
            .sort_key
            .cmp(&b.record.sort_key)
            .then_with(|| b.record.updated_at.cmp(&a.record.updated_at))
    });
    Ok(rows)
}

/// 会话模式由人格推导（创建时定死）。
///
/// 单独一个函数是因为它有两个发布口——REST 的会话对象和 `session.created`
/// 事件——而前端两条路都要用它分组。之前只有 REST 那份带上了，事件那份漏了，
/// 结果新建的 dev 会话在刷新之前一直显示在「普通模式」组里。
pub(in crate::web) fn session_mode_label(record: &miyu_core::state::SessionRecord) -> &'static str {
    if record.persona == miyu_core::state::DEV_PERSONA {
        "dev"
    } else {
        "normal"
    }
}

pub(in crate::web) fn session_record_json(record: &miyu_core::state::SessionRecord) -> Value {
    json!({
        "session_id": record.session_id,
        "name": record.name,
        "kind": record.kind,
        "sandbox": record.sandbox,
        "sandbox_read_all": record.sandbox_read_all,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
        "mode": session_mode_label(record),
    })
}

pub(in crate::web) fn session_overview_json(
    overview: &miyu_core::state::SessionOverview,
    current: &str,
) -> Value {
    let mut value = session_record_json(&overview.record);
    value["turn_count"] = json!(overview.turn_count);
    value["last_user_content"] = json!(overview.last_user_content);
    value["is_current"] = json!(overview.record.session_id == current);
    value
}

/// Resolves an optional turn-target session id: validates existence and that
/// it is a user or one-shot session; `None` falls back to the global current
/// session.
/// 会话模式创建时定死:dev 人格(DEV_PERSONA)会话永远 Dev,其余永远
/// Normal——客户端传什么都不构成中途切换路径。
pub(in crate::web) fn turn_mode_for_session(
    store: &StateStore,
    session_id: &str,
    requested: AgentMode,
) -> AgentMode {
    match store.session_record(session_id) {
        Ok(Some(record)) if record.persona == miyu_core::state::DEV_PERSONA => AgentMode::Dev,
        _ => {
            if requested == AgentMode::Dev {
                tracing::debug!(%session_id, "client asked for dev mode on a non-dev session; forcing normal");
            }
            AgentMode::Normal
        }
    }
}

/// `owner` 同 [`resolve_local_session_ref_with_kinds`]:HTTP 路径传登录者的
/// 归属键,IPC 传 None。没给会话 id 时,管理员/IPC 落到全局当前会话,成员落到
/// 自己名下最近的一条(没有就建)。
pub(in crate::web) fn resolve_turn_session(
    state: &DaemonState,
    owner: Option<&str>,
    session_id: Option<String>,
) -> std::result::Result<Arc<str>, String> {
    match session_id {
        None => match owner {
            Some(owner) if !owner.is_empty() => Ok(member_current_session(state, owner)?.into()),
            _ => Ok(state.state_store.session_id()),
        },
        Some(session_id) => {
            let record = resolve_local_session_ref_with_kinds(
                state,
                &ipc::SessionRef::Id { id: session_id },
                TURN_TARGET_KINDS,
                owner,
            )?;
            Ok(record.session_id.into())
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::web) fn session_for_persona(
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    persona: &str,
) -> Result<String> {
    if let Some(session_id) = state_store.persona_current_session(persona)? {
        if is_available_local_session(state_store, &session_id, persona)? {
            return Ok(session_id);
        }
    }
    let remembered = manager
        .lock()
        .unwrap()
        .persona_session_ids
        .get(persona)
        .cloned();
    if let Some(session_id) = remembered {
        if is_available_local_session(state_store, &session_id, persona)? {
            return Ok(session_id);
        }
    }
    if let Some(overview) = state_store
        .list_local_sessions_for_owner(persona, "")?
        .into_iter()
        .next()
    {
        return Ok(overview.record.session_id);
    }
    Ok(state_store
        .create_session(persona, "", "user", None)?
        .session_id)
}

/// Auto-names a still-unnamed session from its first prompt once a turn has
/// run in it. Explicit names (given at creation or via rename) are never
/// overwritten.
pub(in crate::web) fn maybe_auto_name_session(
    state_store: &StateStore,
    events: &EventHub,
    seed: &str,
) -> Option<String> {
    let session_id = state_store.session_id();
    let record = state_store.session_record(&session_id).ok().flatten()?;
    if !record.name.trim().is_empty() {
        return None;
    }
    let title = session_title_from_prompt(seed);
    if title.is_empty() {
        return None;
    }
    if state_store
        .rename_session(&record.session_id, &title)
        .is_ok()
    {
        events.publish(
            "session.renamed",
            json!({ "session_id": record.session_id, "name": title }),
        );
        return Some(title);
    }
    None
}

pub(in crate::web) fn session_title_from_prompt(prompt: &str) -> String {
    let cleaned = prompt
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut title: String = cleaned.chars().take(20).collect();
    if cleaned.chars().count() > 20 {
        title.push('…');
    }
    title
}

/// 把目标会话钉的模型池套到 `config` 上。回合路与压缩路共用同一条规则，
/// 否则摘要会被路由到全局池里的另一家供应商，拿不到该会话的前缀缓存。
pub(in crate::web) fn apply_session_model_override_to(
    config: &mut AppConfig,
    store: &StateStore,
    session_id: &str,
) {
    match store.session_model_override(session_id) {
        Ok(Some(models)) => config.active_provider_models = Some(models),
        Ok(None) => {}
        Err(error) => tracing::warn!(
            error = %error,
            session_id,
            "{}",
            t(
                "loading the session model override failed",
                "读取会话模型覆盖失败"
            )
        ),
    }
}

pub(in crate::web) fn build_session_agent(
    config: &AppConfig,
    paths: &MiyuPaths,
    state: &StateStore,
    mode: AgentMode,
) -> Result<Agent> {
    miyu_base::models_cache::ensure_active_metadata(paths, config);
    let client = OpenAiCompatibleClient::from_config(config, paths)?;
    let registry = build_tool_registry(config, paths, mode, true)?;
    Ok(
        Agent::new(config.clone(), paths, state.clone(), client, registry, mode)?
            .with_headless_pacing(),
    )
}

pub(in crate::web) fn session_state(
    manager: &Arc<Mutex<ManagerState>>,
    state_store: &StateStore,
) -> Result<ipc::SessionState> {
    let context = manager.lock().unwrap().context;
    let session_id = state_store.session_id();
    let record = state_store.session_record(&session_id)?;
    Ok(ipc::SessionState {
        context_tokens: context.tokens,
        context_window: context.window,
        context_window_assumed: context.window_assumed,
        cumulative_tokens: context.cumulative_tokens,
        cumulative_prompt_tokens: context.cumulative_prompt_tokens,
        cumulative_cache_read_tokens: context.cumulative_cache_read_tokens,
        session_id: session_id.to_string(),
        session_name: record
            .as_ref()
            .map(|record| record.name.clone())
            .unwrap_or_default(),
        sandbox: record.and_then(|record| record.sandbox),
        sandbox_writable: Vec::new(),
        sandbox_readable: Vec::new(),
    })
}

/// Global admin reservation (config/model changes): requires that no turn is
/// running in any session.
pub(in crate::web) fn reserve_admin(
    manager: &Arc<Mutex<ManagerState>>,
) -> std::result::Result<(), ApiError> {
    let mut manager = manager.lock().unwrap();
    if !manager.active_runs.is_empty() || manager.admin_busy {
        return Err(ApiError::new(StatusCode::CONFLICT, ipc::ADMIN_BUSY_MESSAGE));
    }
    manager.admin_busy = true;
    manager.admin_session = None;
    Ok(())
}

/// Per-session admin reservation (reset/undo/pop/compact/delete/archive):
/// only the target session must be idle; turns in other sessions keep
/// running.
pub(in crate::web) fn reserve_admin_for_session(
    manager: &Arc<Mutex<ManagerState>>,
    session_id: &str,
) -> std::result::Result<(), ApiError> {
    let mut manager = manager.lock().unwrap();
    if manager.admin_busy || manager.session_has_runs(session_id) {
        return Err(ApiError::new(StatusCode::CONFLICT, ipc::ADMIN_BUSY_MESSAGE));
    }
    manager.admin_busy = true;
    // 预约限定到这个会话:压缩/pop/undo 重写的是它自己的消息数组,别的
    // 会话该照常开回合。以前这里只置全局位,压一个会话等于停掉整台机器。
    manager.admin_session = Some(session_id.to_string());
    Ok(())
}

/// Light admin reservation (session/model updates): serializes against other
/// admin operations but is allowed while turns are running.
pub(in crate::web) fn reserve_admin_light(
    manager: &Arc<Mutex<ManagerState>>,
) -> std::result::Result<(), ApiError> {
    let mut manager = manager.lock().unwrap();
    if manager.admin_busy {
        return Err(ApiError::new(StatusCode::CONFLICT, ipc::ADMIN_BUSY_MESSAGE));
    }
    manager.admin_busy = true;
    manager.admin_session = None;
    Ok(())
}
