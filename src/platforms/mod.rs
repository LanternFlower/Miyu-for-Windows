//! IM platform bridges.
//!
//! This module is the platform-neutral core: turn driving against the
//! agent actor, session resolution, rate limiting and reply shaping.
//! Each protocol lives in its own submodule (`onebot` = NapCat / QQ);
//! later platforms (Telegram, QQ official, WeChat) add submodules and
//! reuse everything here without touching the web core.

mod activity;
mod logging;
mod reply;
mod scheduling;
mod turn_context;
mod turn_run;
pub(crate) use activity::*;
pub(crate) use logging::*;
pub(crate) use reply::*;
pub(crate) use scheduling::*;
pub(crate) use turn_context::*;
pub(crate) use turn_run::*;
mod access_control;
mod assets;
pub(crate) mod avatar;
pub(crate) mod commands;
pub(crate) mod file_reader;
pub(crate) mod onebot;
pub(crate) mod plugins;
mod tool;
// 平台层的纯数据类型下沉到 crate::platform_types：tools / memory / agent 都
// 要用它们（图片引用、主体身份、会话标识），但不该为此依赖整个平台运行时。
// 这里原样再导出，`platforms::PlatformPrincipal` 这类写法一个字都不用改。
mod tool_context;

pub(crate) use crate::platform_types::{
    BotGroupRole, BotSendAvailability, ConversationKind, ForwardNode, OutboundBody,
    OutboundMessage, OutboundOrigin, OutboundSegment, PartialSendError, PlatformAdapter,
    PlatformContextFileRef, PlatformContextImageRef, PlatformConversation, PlatformFileDownload,
    PlatformGroupMember, PlatformImageData, PlatformInboundEvent, PlatformInboundEventKind,
    PlatformInboundMedia, PlatformMediaKind, PlatformMention, PlatformMessageInfo,
    PlatformMessagePosition, PlatformPrincipal, ResponseTarget, SendReceipt, TriggerDecision,
};

use crate::agent::{AgentMode, QueueIngressBarrier, QueueIngressReservation};
use crate::config::{
    ActiveProviderModelConfig, AppConfig, PlatformRateLimit, PlatformSessionLimits, PromptAudience,
};
use crate::i18n::{text_for, Locale};
use crate::ipc::ImageAttachment;
use crate::paths::MiyuPaths;
use crate::state::{PlatformSessionBindingKey, StateStore};
use crate::runtime::{random_id, validate_content, ActorCommand, DaemonState, IpcRunGuard, RunInfo};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// Shared state for all IM bridges, hung off `DaemonState`. Cheap to clone;
/// everything inside is reference counted.
#[derive(Clone)]
pub(crate) struct PlatformRuntime {
    http: Arc<OnceLock<std::result::Result<reqwest::Client, String>>>,
    pub(crate) onebot: Arc<Mutex<onebot::ConnectionRegistry>>,
    pub(crate) qq_listener: onebot::QqListenerManager,
    pub(crate) rate: Arc<Mutex<RateWindow>>,
    plugins: Arc<OnceLock<std::result::Result<Arc<plugins::PlatformPluginRegistry>, String>>>,
    pub(crate) assets: assets::AssetLeaseStore,
    pub(crate) turn_permits: Arc<tokio::sync::Semaphore>,
    pub(crate) file_store_lock: Arc<tokio::sync::Mutex<()>>,
    pub(crate) message_activity: MessageActivityRegistry,
    session_turn_locks: Arc<Mutex<HashMap<String, Weak<SessionTurnState>>>>,
}

impl PlatformRuntime {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            http: Arc::new(OnceLock::new()),
            onebot: Arc::new(Mutex::new(onebot::ConnectionRegistry::default())),
            qq_listener: onebot::QqListenerManager::default(),
            rate: Arc::new(Mutex::new(RateWindow::new())),
            plugins: Arc::new(OnceLock::new()),
            assets: assets::AssetLeaseStore::new(),
            turn_permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PLATFORM_TURNS)),
            file_store_lock: Arc::new(tokio::sync::Mutex::new(())),
            message_activity: MessageActivityRegistry::default(),
            session_turn_locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub(crate) fn http_client(&self) -> Result<reqwest::Client> {
        self.http
            .get_or_init(|| {
                reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(10))
                    .build()
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .cloned()
            .map_err(|error| anyhow!("building the IM platform HTTP client: {error}"))
    }

    pub(crate) fn plugins(&self) -> Result<Arc<plugins::PlatformPluginRegistry>> {
        self.plugins
            .get_or_init(|| {
                plugins::PlatformPluginRegistry::built_in()
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .cloned()
            .map_err(|error| anyhow!("building the IM platform plugin registry: {error}"))
    }

    fn session_turn_ticket(
        &self,
        session_id: &str,
        limits: PlatformSessionLimits,
    ) -> SessionTurnTicket {
        let state = {
            let mut locks = self.session_turn_locks.lock().unwrap();
            match locks.get(session_id).and_then(Weak::upgrade) {
                Some(state) => state,
                None => {
                    let state = Arc::new(SessionTurnState::new(limits));
                    locks.insert(session_id.to_string(), Arc::downgrade(&state));
                    state
                }
            }
        };
        SessionTurnTicket {
            session_id: session_id.to_string(),
            generation: state.generation.load(Ordering::Acquire),
            state,
            states: self.session_turn_locks.clone(),
            exclusive: false,
        }
    }

    async fn acquire_session_turn(
        &self,
        session_id: &str,
        limits: PlatformSessionLimits,
    ) -> std::result::Result<SessionTurnLease, SessionTurnAcquireError> {
        self.session_turn_ticket(session_id, limits).acquire().await
    }

    pub(crate) fn preempt_session_turns(&self, session_id: &str) -> SessionTurnTicket {
        let mut ticket = self.session_turn_ticket(session_id, PlatformSessionLimits::default());
        ticket.generation = ticket
            .state
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        ticket.exclusive = true;
        ticket.state.preempting.store(true, Ordering::Release);
        ticket
    }

    pub(crate) fn queued_session_turns(&self, session_id: &str) -> usize {
        let locks = self.session_turn_locks.lock().unwrap();
        locks
            .get(session_id)
            .and_then(Weak::upgrade)
            .map(|state| state.waiting.load(Ordering::Acquire))
            .unwrap_or(0)
    }
}

pub(crate) use assets::platform_asset;

#[cfg(test)]
mod tests;
