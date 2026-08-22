//! 插件的运行时状态。
//!
//! 每个群一份状态，带软上限与空闲 TTL（`SESSION_STATE_*`）——群会一直增加，
//! 不淘汰就是慢性泄漏。
//!
//! `DynamicGate` 控制并发判定：同一个群同时只跑一个「要不要插话」的判断，
//! permit 用 `Drop` 释放。`wait_for_supersede` 处理的是判断跑到一半又来了新消
//! 息——旧判断的结论已经过时，直接让位。

use crate::platforms::plugins::real_context::*;

pub(in crate::platforms::plugins::real_context) const SESSION_STATE_SOFT_LIMIT: usize = 512;

pub(in crate::platforms::plugins::real_context) const SESSION_STATE_IDLE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

pub(in crate::platforms::plugins::real_context) const PENDING_REPLY_TTL: Duration = Duration::from_secs(31 * 60);

#[derive(Default)]
pub(in crate::platforms::plugins::real_context) struct RuntimeState {
    pub(in crate::platforms::plugins::real_context) sessions: HashMap<String, SessionRuntime>,
    pub(in crate::platforms::plugins::real_context) next_generation: u64,
}

impl RuntimeState {
    pub(in crate::platforms::plugins::real_context) fn session_mut(&mut self, key: &str, now: Instant) -> &mut SessionRuntime {
        let session = self
            .sessions
            .entry(key.to_string())
            .or_insert_with(|| SessionRuntime::new(now));
        session.last_touched = now;
        session
    }

    pub(in crate::platforms::plugins::real_context) fn prune(&mut self, now: Instant) {
        for session in self.sessions.values_mut() {
            session
                .pending
                .retain(|_, pending| now.duration_since(pending.started) <= PENDING_REPLY_TTL);
        }
        if self.sessions.len() > SESSION_STATE_SOFT_LIMIT {
            self.sessions.retain(|_, session| {
                !session.pending.is_empty()
                    || now.duration_since(session.last_touched) <= SESSION_STATE_IDLE_TTL
            });
        }
        let removable = self.sessions.len().saturating_sub(SESSION_STATE_SOFT_LIMIT);
        if removable > 0 {
            let mut inactive = self
                .sessions
                .iter()
                .filter(|(_, session)| session.pending.is_empty())
                .map(|(key, session)| (key.clone(), session.last_touched))
                .collect::<Vec<_>>();
            inactive.sort_unstable_by_key(|(_, touched)| *touched);
            for (key, _) in inactive.into_iter().take(removable) {
                self.sessions.remove(&key);
            }
        }
    }
}

pub(in crate::platforms::plugins::real_context) struct SessionRuntime {
    pub(in crate::platforms::plugins::real_context) last_touched: Instant,
    pub(in crate::platforms::plugins::real_context) last_reply: Option<Instant>,
    pub(in crate::platforms::plugins::real_context) heat: f64,
    pub(in crate::platforms::plugins::real_context) heat_updated: Instant,
    pub(in crate::platforms::plugins::real_context) continuation: Option<Continuation>,
    pub(in crate::platforms::plugins::real_context) pending: HashMap<String, PendingReply>,
}

impl SessionRuntime {
    pub(in crate::platforms::plugins::real_context) fn new(now: Instant) -> Self {
        Self {
            last_touched: now,
            last_reply: None,
            heat: 0.0,
            heat_updated: now,
            continuation: None,
            pending: HashMap::new(),
        }
    }

    pub(in crate::platforms::plugins::real_context) fn decay_heat(&mut self, now: Instant, recover_minutes: u64) {
        let recover = Duration::from_secs(recover_minutes.max(1) * 60).as_secs_f64();
        let elapsed = now.duration_since(self.heat_updated).as_secs_f64();
        self.heat = (self.heat - elapsed / recover).max(0.0);
        self.heat_updated = now;
    }

    pub(in crate::platforms::plugins::real_context) fn increase_heat(&mut self, now: Instant, settings: &RealContextPluginSettings) {
        if !settings.reply_restraint_enable {
            return;
        }
        self.decay_heat(now, settings.reply_restraint_recover_minutes);
        self.heat += settings.reply_restraint_multiplier;
        self.heat_updated = now;
    }

    pub(in crate::platforms::plugins::real_context) fn continuation_match(&mut self, sender_id: &str, now: Instant, enabled: bool) -> bool {
        if !enabled {
            self.continuation = None;
            return false;
        }
        let Some(continuation) = self.continuation.as_ref() else {
            return false;
        };
        // Only the clock and the speaker bound a continuation. There used to be
        // a turn cap as well, which cut a conversation off mid-flow purely
        // because it had gone on for a few exchanges — the window itself is
        // what expresses "we are still talking".
        if now > continuation.expires_at || continuation.user_id != sender_id {
            self.continuation = None;
            return false;
        }
        true
    }

    pub(in crate::platforms::plugins::real_context) fn mark_continuation(
        &mut self,
        sender_id: &str,
        now: Instant,
        settings: &RealContextPluginSettings,
    ) {
        if !settings.continuation_enable {
            self.continuation = None;
            return;
        }
        // Every reply we actually send restarts the clock, including one the
        // continuation window itself prompted: answering inside the window is
        // exactly the evidence that the exchange is still live, so it should
        // extend the window rather than count down against it.
        self.continuation = Some(Continuation {
            user_id: sender_id.to_string(),
            expires_at: now + Duration::from_secs(settings.continuation_window_seconds),
        });
    }
}

pub(in crate::platforms::plugins::real_context) struct Continuation {
    pub(in crate::platforms::plugins::real_context) user_id: String,
    pub(in crate::platforms::plugins::real_context) expires_at: Instant,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(in crate::platforms::plugins::real_context) struct ActiveReplyTarget {
    pub(in crate::platforms::plugins::real_context) message_id: String,
    pub(in crate::platforms::plugins::real_context) sender_id: String,
    pub(in crate::platforms::plugins::real_context) sender_name: String,
    pub(in crate::platforms::plugins::real_context) timestamp: i64,
    pub(in crate::platforms::plugins::real_context) content: String,
    pub(in crate::platforms::plugins::real_context) reply_message_id: Option<String>,
    pub(in crate::platforms::plugins::real_context) reply_sender_id: Option<String>,
    pub(in crate::platforms::plugins::real_context) reply_sender_name: Option<String>,
    pub(in crate::platforms::plugins::real_context) reply_content: Option<String>,
    #[serde(default)]
    pub(in crate::platforms::plugins::real_context) mentioned_user_ids: Vec<String>,
    #[serde(default)]
    pub(in crate::platforms::plugins::real_context) mentioned_users: Vec<PlatformMention>,
    pub(in crate::platforms::plugins::real_context) supplemental: bool,
}

pub(in crate::platforms::plugins::real_context) struct PendingReply {
    pub(in crate::platforms::plugins::real_context) generation: u64,
    pub(in crate::platforms::plugins::real_context) started: Instant,
    pub(in crate::platforms::plugins::real_context) trigger: TriggerKind,
    /// 回复承诺已成立(直触发,或主动判断已通过)。补救窗口内的新消息
    /// 直接顶替目标而不再重新判断;未承诺(仍在判断中)则取消旧判断、
    /// 对新消息重新判断。
    pub(in crate::platforms::plugins::real_context) committed: bool,
    pub(in crate::platforms::plugins::real_context) reactions: Vec<(String, String)>,
    pub(in crate::platforms::plugins::real_context) targets: Vec<ActiveReplyTarget>,
    pub(in crate::platforms::plugins::real_context) cancel: tokio::sync::watch::Sender<bool>,
}

pub(in crate::platforms::plugins::real_context) async fn wait_for_supersede(receiver: &mut tokio::sync::watch::Receiver<bool>) {
    if *receiver.borrow() {
        return;
    }
    while receiver.changed().await.is_ok() {
        if *receiver.borrow() {
            return;
        }
    }
}

#[derive(Default)]
pub(in crate::platforms::plugins::real_context) struct DynamicGate {
    pub(in crate::platforms::plugins::real_context) active: AtomicUsize,
    pub(in crate::platforms::plugins::real_context) notify: Notify,
}

impl DynamicGate {
    pub(in crate::platforms::plugins::real_context) async fn acquire(&self, limit: usize, timeout: Duration) -> Option<DynamicGatePermit<'_>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let current = self.active.load(Ordering::Acquire);
            if current < limit.max(1)
                && self
                    .active
                    .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                return Some(DynamicGatePermit { gate: self });
            }
            if tokio::time::timeout_at(deadline, self.notify.notified())
                .await
                .is_err()
            {
                return None;
            }
        }
    }
}

pub(in crate::platforms::plugins::real_context) struct DynamicGatePermit<'a> {
    pub(in crate::platforms::plugins::real_context) gate: &'a DynamicGate,
}

impl Drop for DynamicGatePermit<'_> {
    fn drop(&mut self) {
        self.gate.active.fetch_sub(1, Ordering::AcqRel);
        self.gate.notify.notify_one();
    }
}

pub(super) fn group_key(context: &PlatformTurnContext) -> Result<GroupKey> {
    group_key_for(context, &context.conversation.conversation_id)
}

pub(super) fn group_key_for(context: &PlatformTurnContext, group_id: &str) -> Result<GroupKey> {
    GroupKey::new(
        context.conversation.platform.clone(),
        context.conversation.account_id.clone(),
        group_id.to_string(),
    )
}

pub(super) fn account_key(context: &PlatformTurnContext) -> Result<AccountKey> {
    AccountKey::new(
        context.conversation.platform.clone(),
        context.conversation.account_id.clone(),
    )
}

pub(in crate::platforms::plugins::real_context) fn runtime_session_key(context: &PlatformTurnContext) -> String {
    format!(
        "{}|persona:{}",
        context.conversation.scope_key(),
        context.config.active_persona_scope()
    )
}

pub(in crate::platforms::plugins::real_context) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
