//! 回合的并发闸门与限流。
//!
//! 全局有并发上限（`MAX_CONCURRENT_PLATFORM_TURNS`），每个会话同时只能有一个回
//! 合（`SessionTurnTicket`）。票据用 `Drop` 释放——回合可能在任何 await 点被取
//! 消，只有 Drop 保证跑到。
//!
//! 限流是双层的：按会话和按发送者。只按会话的话，一个人在多个群刷屏就绕过去
//! 了。

use crate::platforms::*;

/// Hard ceiling for one platform-driven turn; beyond this the run is
/// cancelled so a wedged turn cannot pin the bridge task forever.
pub(crate) const PLATFORM_TURN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub(crate) const RATE_PRUNE_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) const MAX_CONCURRENT_PLATFORM_TURNS: usize = 16;

pub(crate) struct SessionTurnState {
    pub(crate) slots: Arc<tokio::sync::Semaphore>,
    pub(crate) gate: Arc<tokio::sync::RwLock<()>>,
    pub(crate) waiting: AtomicUsize,
    pub(crate) max_queued: usize,
    pub(crate) preempting: AtomicBool,
    pub(crate) preemption_changed: tokio::sync::Notify,
    pub(crate) generation: AtomicU64,
}

impl SessionTurnState {
    pub(crate) fn new(limits: PlatformSessionLimits) -> Self {
        Self {
            slots: Arc::new(tokio::sync::Semaphore::new(limits.running)),
            gate: Arc::new(tokio::sync::RwLock::new(())),
            waiting: AtomicUsize::new(0),
            max_queued: limits.queued,
            preempting: AtomicBool::new(false),
            preemption_changed: tokio::sync::Notify::new(),
            generation: AtomicU64::new(0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTurnAcquireError {
    Full,
    Closed,
}

pub(crate) struct SessionTurnTicket {
    pub(crate) states: Arc<Mutex<HashMap<String, Weak<SessionTurnState>>>>,
    pub(crate) session_id: String,
    pub(crate) state: Arc<SessionTurnState>,
    pub(crate) generation: u64,
    pub(crate) exclusive: bool,
    /// 到达顺序位。票据在判断之前建好并持有它,判断期间就已经排上队;票据
    /// 没走到 `acquire` 就掉落时,顺序位随之让出。抢占票据不参与排序——覆盖
    /// 的语义就是插队。
    pub(crate) order_slot: Option<TurnOrderSlot>,
}

impl SessionTurnTicket {
    pub(crate) async fn acquire(
        mut self,
    ) -> std::result::Result<SessionTurnLease, SessionTurnAcquireError> {
        if !self.exclusive {
            if let Some(slot) = self.order_slot.as_ref() {
                slot.wait_turn().await;
            }
        }
        let (guard, permit) = if self.exclusive {
            (
                SessionTurnGuard::Write(self.state.gate.clone().write_owned().await),
                None,
            )
        } else {
            let permit = match self.state.slots.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(tokio::sync::TryAcquireError::Closed) => {
                    return Err(SessionTurnAcquireError::Closed)
                }
                Err(tokio::sync::TryAcquireError::NoPermits) => {
                    if self
                        .state
                        .waiting
                        .try_update(Ordering::AcqRel, Ordering::Acquire, |waiting| {
                            (waiting < self.state.max_queued).then_some(waiting + 1)
                        })
                        .is_err()
                    {
                        return Err(SessionTurnAcquireError::Full);
                    }
                    let acquired = self.state.slots.clone().acquire_owned().await;
                    self.state.waiting.fetch_sub(1, Ordering::AcqRel);
                    acquired.map_err(|_| SessionTurnAcquireError::Closed)?
                }
            };
            while self.state.preempting.load(Ordering::Acquire) {
                let changed = self.state.preemption_changed.notified();
                if !self.state.preempting.load(Ordering::Acquire) {
                    break;
                }
                changed.await;
            }
            (
                SessionTurnGuard::Read(self.state.gate.clone().read_owned().await),
                Some(permit),
            )
        };
        // 名额到手,顺序位使命完成。继续占着会让 running > 1 的并行体制退化
        // 成串行——后面的人本该现在就去抢下一个名额。
        if let Some(mut slot) = self.order_slot.take() {
            slot.release();
        }
        // 克隆而不是移动:票据现在实现了 Drop(见下),Rust 不允许从带 Drop 的
        // 类型里搬字段出来。三个字段都是 Arc / String,克隆的代价可以忽略。
        Ok(SessionTurnLease {
            guard: Some(guard),
            permit,
            states: self.states.clone(),
            session_id: self.session_id.clone(),
            state: self.state.clone(),
            generation: self.generation,
            exclusive: self.exclusive,
        })
    }
}

impl Drop for SessionTurnTicket {
    fn drop(&mut self) {
        // 判断挪到 acquire 之前以后,"票据建好却从不消耗"成了常态(判断说不回
        // 复就直接 return)。原先这条路不存在,登记表只由租约的 Drop 回收,于是
        // 每条不回复的消息都会在表里留下一个死 Weak——admission 测试抓到了。
        //
        // 判据与租约一致:只有自己是最后一个持有者时才回收。`acquire` 成功后
        // 租约也持有同一个 Arc,strong_count ≥ 2,这里就不会误删。
        self.order_slot.take();
        let mut states = self.states.lock().unwrap();
        if Arc::strong_count(&self.state) != 1 {
            return;
        }
        // 抢占票据在创建时就置了 preempting,由租约的 Drop 清。没走到 acquire
        // 的抢占票据必须自己清,否则这条会话上所有后续回合永远等下去。
        if self.exclusive {
            self.state.preempting.store(false, Ordering::Release);
            self.state.preemption_changed.notify_waiters();
        }
        if states
            .get(&self.session_id)
            .is_some_and(|registered| Weak::ptr_eq(registered, &Arc::downgrade(&self.state)))
        {
            states.remove(&self.session_id);
        }
    }
}

pub(crate) enum SessionTurnGuard {
    Read(tokio::sync::OwnedRwLockReadGuard<()>),
    Write(tokio::sync::OwnedRwLockWriteGuard<()>),
}

pub(crate) struct SessionTurnLease {
    pub(crate) guard: Option<SessionTurnGuard>,
    pub(crate) permit: Option<tokio::sync::OwnedSemaphorePermit>,
    pub(crate) states: Arc<Mutex<HashMap<String, Weak<SessionTurnState>>>>,
    pub(crate) session_id: String,
    pub(crate) state: Arc<SessionTurnState>,
    pub(crate) generation: u64,
    pub(crate) exclusive: bool,
}

impl SessionTurnLease {
    pub(crate) fn is_valid(&self) -> bool {
        self.state.generation.load(Ordering::Acquire) == self.generation
    }
}

impl Drop for SessionTurnLease {
    fn drop(&mut self) {
        // Release the session before removing its registry entry. Otherwise a
        // new arrival could create a second lock during this guard's drop.
        self.guard.take();
        self.permit.take();
        if self.exclusive {
            self.state.preempting.store(false, Ordering::Release);
            self.state.preemption_changed.notify_waiters();
        }
        let mut states = self.states.lock().unwrap();
        if Arc::strong_count(&self.state) == 1
            && states
                .get(&self.session_id)
                .is_some_and(|registered| Weak::ptr_eq(registered, &Arc::downgrade(&self.state)))
        {
            states.remove(&self.session_id);
        }
    }
}

#[derive(Clone)]
pub(crate) struct TurnProfile {
    pub(crate) active_persona: Option<String>,
    pub(crate) text_models: Option<Vec<ActiveProviderModelConfig>>,
    pub(crate) multimodal_models: Option<Vec<ActiveProviderModelConfig>>,
    pub(crate) system_context: Vec<String>,
    /// Per-message transport context (sender identity JSON, message ids, …).
    /// Rendered as a tail system message after the user turn instead of being
    /// folded into the system prompt, so the stable prefix stays byte-identical
    /// across turns (v7 Phase 2.1).
    pub(crate) turn_system_context: Vec<String>,
    /// Raw input snapshot for the memory diary (pre-plugin content); `None`
    /// keeps the agent's default of recording the turn content as-is.
    pub(crate) memory_content: Option<String>,
    pub(crate) context_images: Vec<PlatformContextImageRef>,
    pub(crate) context_files: Box<[PlatformContextFileRef]>,
    pub(crate) platform: Option<Arc<PlatformTurnContext>>,
    pub(crate) image_cache_namespace: Option<String>,
    pub(crate) image_source_label: Option<String>,
    pub(crate) memory_write_enabled: bool,
    /// Structured platform history replaces ambiguous core user/assistant
    /// replay for shared conversations such as QQ groups.
    pub(crate) suppress_session_history: bool,
    /// Group overflow handling; `None` inherits the global `context` settings.
    pub(crate) group_context: Option<crate::config::PlatformGroupContextConfig>,
    pub(crate) followup: Option<Arc<PlatformFollowupRun>>,
}

impl Default for TurnProfile {
    fn default() -> Self {
        Self {
            active_persona: None,
            text_models: None,
            multimodal_models: None,
            system_context: Vec::new(),
            turn_system_context: Vec::new(),
            memory_content: None,
            context_images: Vec::new(),
            context_files: Vec::new().into_boxed_slice(),
            platform: None,
            image_cache_namespace: None,
            image_source_label: None,
            memory_write_enabled: true,
            suppress_session_history: false,
            group_context: None,
            followup: None,
        }
    }
}

/// Per-conversation fixed-window rate limiter shared by all platforms.
pub(crate) struct RateWindow {
    pub(crate) last_prune: Instant,
    pub(crate) conversations: HashMap<String, SenderWindow>,
}

pub(crate) struct SenderWindow {
    pub(crate) window_start: Instant,
    pub(crate) window: Duration,
    pub(crate) count: u32,
    pub(crate) notified: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RateDecision {
    Allow,
    /// Over quota and already warned this window.
    DropSilently,
    /// Over quota for the first time this window: send one notice.
    DropWithNotice,
}

impl RateWindow {
    pub(crate) fn new() -> Self {
        Self {
            last_prune: Instant::now(),
            conversations: HashMap::new(),
        }
    }

    pub(crate) fn check(&mut self, conversation: &str, limit: PlatformRateLimit) -> RateDecision {
        self.check_at(Instant::now(), conversation, limit)
    }

    pub(crate) fn available(&mut self, conversation: &str, limit: PlatformRateLimit) -> bool {
        self.available_at(Instant::now(), conversation, limit)
    }

    pub(crate) fn available_at(
        &mut self,
        now: Instant,
        conversation: &str,
        limit: PlatformRateLimit,
    ) -> bool {
        self.prune_at(now);
        if limit.max_messages == 0 {
            return true;
        }
        let configured_window = Duration::from_secs(u64::from(limit.window_seconds));
        self.conversations.get(conversation).is_none_or(|entry| {
            entry.window != configured_window
                || now.duration_since(entry.window_start) >= configured_window
                || entry.count < limit.max_messages
        })
    }

    pub(crate) fn check_at(
        &mut self,
        now: Instant,
        conversation: &str,
        limit: PlatformRateLimit,
    ) -> RateDecision {
        self.prune_at(now);
        if limit.max_messages == 0 {
            return RateDecision::Allow;
        }
        let configured_window = Duration::from_secs(u64::from(limit.window_seconds));
        let entry = self
            .conversations
            .entry(conversation.to_string())
            .or_insert(SenderWindow {
                window_start: now,
                window: configured_window,
                count: 0,
                notified: false,
            });
        if entry.window != configured_window
            || now.duration_since(entry.window_start) >= configured_window
        {
            *entry = SenderWindow {
                window_start: now,
                window: configured_window,
                count: 0,
                notified: false,
            };
        }
        if entry.count < limit.max_messages {
            entry.count += 1;
            return RateDecision::Allow;
        }
        if entry.notified {
            return RateDecision::DropSilently;
        }
        entry.notified = true;
        RateDecision::DropWithNotice
    }

    pub(crate) fn prune_at(&mut self, now: Instant) {
        if now.duration_since(self.last_prune) >= RATE_PRUNE_INTERVAL {
            self.last_prune = now;
            self.conversations.retain(|_, entry| {
                now.checked_duration_since(entry.window_start)
                    .is_some_and(|elapsed| elapsed < entry.window)
            });
        }
    }
}

/// Finds or creates the dedicated user session for a stable external
/// conversation identity. The visible session name can be edited freely;
/// routing never depends on it after the binding has been created.
pub(crate) fn resolve_platform_session(
    state: &DaemonState,
    conversation: &PlatformConversation,
    persona: &str,
    participant_id: Option<String>,
    name: &str,
    legacy_name: Option<&str>,
) -> Result<Arc<str>> {
    let key = PlatformSessionBindingKey {
        platform: conversation.platform.clone(),
        account_id: conversation.account_id.clone(),
        conversation_kind: conversation.kind.as_str().to_string(),
        conversation_id: conversation.conversation_id.clone(),
        participant_id,
        persona: persona.to_string(),
    };
    if let Some(session_id) = state.state_store.find_platform_session_binding(&key)? {
        let record = state
            .state_store
            .session_record(&session_id)?
            .with_context(|| format!("bound platform session is missing: {session_id}"))?;
        return Ok(record.session_id.into());
    }

    // Adopt the pre-binding name only when it identifies exactly one session.
    // If multiple bot accounts race for the same legacy name, the first bind
    // wins and every later account gets a fresh, correctly isolated session.
    let mut candidates = state
        .state_store
        .list_sessions(&persona)?
        .into_iter()
        .filter(|overview| {
            overview.record.kind == "user"
                && (overview.record.name == name
                    || legacy_name.is_some_and(|legacy| overview.record.name == legacy))
        })
        .map(|overview| overview.record)
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        let record = candidates.pop().expect("length checked");
        match state
            .state_store
            .claim_platform_session(&key, &record.session_id)
        {
            Ok(session_id) if session_id == record.session_id => {
                return Ok(record.session_id.into());
            }
            Ok(session_id) => return Ok(session_id.into()),
            Err(error) => {
                tracing::warn!(error = %error, session_id = %record.session_id, "{}", crate::i18n::text("legacy platform session could not be bound", "无法绑定旧版平台会话"));
                if let Some(session_id) = state.state_store.find_platform_session_binding(&key)? {
                    return Ok(session_id.into());
                }
            }
        }
    } else if candidates.len() > 1 {
        tracing::warn!(
            name,
            "{}",
            crate::i18n::text(
                "legacy platform session name is ambiguous; creating a new session",
                "旧版平台会话名称存在歧义；正在创建新会话",
            )
        );
    }

    let (record, created) = state
        .state_store
        .create_or_get_platform_session(&key, name)?;
    if created {
        state.events.publish(
            "session.created",
            serde_json::json!({
                "session_id": record.session_id,
                "name": record.name,
                "platform": conversation.platform,
                "account_id": conversation.account_id,
                "conversation_kind": conversation.kind.as_str(),
                "conversation_id": conversation.conversation_id,
            }),
        );
    }
    Ok(record.session_id.into())
}
