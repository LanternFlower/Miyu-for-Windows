//! 群里「谁在说话、说了什么」的短期记账。
//!
//! 用来判断消息热度与去重。所有集合都有硬上限（`MESSAGE_ACTIVITY_*`）：这些数
//! 据完全由对端喂进来，不设限就是把内存交给群友管。
//!
//! `RecentImageLedger` 同理，按会话保留最近几张图，供「刚才那张图」这类指代
//! 使用；TTL 到了就丢。

use crate::platforms::*;

/// How long a delivered image stays deduplicated for its conversation.
/// Auto-attached reply images (generate_image / search_web_images) must not
/// be sent twice when a turn is retried or recovered after an interrupted
/// send; an explicit "send it again" goes through send_message_to_user,
/// which is not filtered by this.
/// Kept short: it only needs to span a recovery turn, and a genuine
/// "send that one again" outside the window must still work.
pub(crate) const RECENT_IMAGE_TTL: Duration = Duration::from_secs(5 * 60);

pub(crate) const RECENT_IMAGE_CONVERSATIONS: usize = 64;

pub(crate) const RECENT_IMAGES_PER_CONVERSATION: usize = 32;

pub(crate) type RecentImageLedger = HashMap<String, Vec<(blake3::Hash, Instant)>>;

pub(crate) fn recent_images() -> &'static Mutex<RecentImageLedger> {
    static LEDGER: OnceLock<Mutex<RecentImageLedger>> = OnceLock::new();
    LEDGER.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn record_recent_conversation_images(scope_key: &str, digests: &[blake3::Hash]) {
    let now = Instant::now();
    let mut ledger = recent_images().lock().unwrap();
    ledger.retain(|_, entries| {
        entries.retain(|(_, at)| now.duration_since(*at) < RECENT_IMAGE_TTL);
        !entries.is_empty()
    });
    let entries = ledger.entry(scope_key.to_string()).or_default();
    for digest in digests {
        entries.retain(|(known, _)| known != digest);
        entries.push((*digest, now));
    }
    if entries.len() > RECENT_IMAGES_PER_CONVERSATION {
        let excess = entries.len() - RECENT_IMAGES_PER_CONVERSATION;
        entries.drain(..excess);
    }
    if ledger.len() > RECENT_IMAGE_CONVERSATIONS {
        // Bound the ledger even when every conversation stays inside the TTL.
        let oldest = ledger
            .iter()
            .filter_map(|(key, entries)| {
                entries.last().map(|(_, at)| (*at, key.clone()))
            })
            .min()
            .map(|(_, key)| key);
        if let Some(key) = oldest {
            ledger.remove(&key);
        }
    }
}

pub(crate) fn recent_conversation_images(scope_key: &str) -> Vec<blake3::Hash> {
    let now = Instant::now();
    recent_images()
        .lock()
        .unwrap()
        .get(scope_key)
        .map(|entries| {
            entries
                .iter()
                .filter(|(_, at)| now.duration_since(*at) < RECENT_IMAGE_TTL)
                .map(|(digest, _)| *digest)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) const MESSAGE_ACTIVITY_SCOPE_SOFT_LIMIT: usize = 512;

pub(crate) const MESSAGE_ACTIVITY_SEEN_LIMIT: usize = 4_096;

pub(crate) const MESSAGE_ACTIVITY_SENDER_LIMIT: usize = 4_096;

pub(crate) const MESSAGE_ACTIVITY_MAX_ID_BYTES: usize = 256;

#[derive(Clone, Default)]
pub(crate) struct MessageActivityRegistry {
    pub(crate) entries: Arc<Mutex<HashMap<String, Weak<MessageActivity>>>>,
}

#[derive(Clone)]
pub(crate) struct MessageActivityHandle(Arc<MessageActivity>);

pub(crate) struct MessageActivity {
    pub(crate) state: Mutex<MessageActivityState>,
}

#[derive(Default)]
pub(crate) struct MessageActivityState {
    pub(crate) total_messages: u64,
    pub(crate) sender_messages: HashMap<String, u64>,
    pub(crate) seen_messages: HashMap<String, SeenMessage>,
}

#[derive(Clone, Copy)]
pub(crate) struct SeenMessage {
    pub(crate) position: PlatformMessagePosition,
    pub(crate) received_at: Instant,
}

impl MessageActivityRegistry {
    pub(crate) fn observe(
        &self,
        scope: &str,
        message_id: &str,
        sender_id: &str,
        received_at: Instant,
    ) -> (MessageActivityHandle, PlatformMessagePosition, Instant) {
        let activity = {
            let mut entries = self.entries.lock().unwrap();
            if entries.len() >= MESSAGE_ACTIVITY_SCOPE_SOFT_LIMIT && !entries.contains_key(scope) {
                entries.retain(|_, activity| activity.strong_count() > 0);
            }
            match entries.get(scope).and_then(Weak::upgrade) {
                Some(activity) => activity,
                None => {
                    let activity = Arc::new(MessageActivity {
                        state: Mutex::new(MessageActivityState::default()),
                    });
                    entries.insert(scope.to_string(), Arc::downgrade(&activity));
                    activity
                }
            }
        };
        let handle = MessageActivityHandle(activity);
        let (position, received_at) = handle.observe(message_id, sender_id, received_at);
        (handle, position, received_at)
    }
}

impl MessageActivityHandle {
    pub(crate) fn observe(
        &self,
        message_id: &str,
        sender_id: &str,
        received_at: Instant,
    ) -> (PlatformMessagePosition, Instant) {
        let mut state = self.0.state.lock().unwrap();
        let track_id = !message_id.is_empty() && message_id.len() <= MESSAGE_ACTIVITY_MAX_ID_BYTES;
        if track_id {
            if let Some(seen) = state.seen_messages.get(message_id) {
                return (seen.position, seen.received_at);
            }
        }
        state.total_messages = state.total_messages.saturating_add(1);
        let total_messages = state.total_messages;
        let sender_messages = {
            // 与 seen_messages 同款兜底:常驻 daemon 里陌生发送者只增不减。
            // 清表的代价只是各发送者的"第 N 条"计数重新起算。
            if state.sender_messages.len() >= MESSAGE_ACTIVITY_SENDER_LIMIT
                && !state.sender_messages.contains_key(sender_id)
            {
                state.sender_messages.clear();
            }
            let count = state
                .sender_messages
                .entry(sender_id.to_string())
                .or_default();
            *count = count.saturating_add(1);
            *count
        };
        let position = PlatformMessagePosition {
            total_messages,
            sender_messages,
        };
        if track_id {
            if state.seen_messages.len() >= MESSAGE_ACTIVITY_SEEN_LIMIT {
                state.seen_messages.clear();
            }
            state.seen_messages.insert(
                message_id.to_string(),
                SeenMessage {
                    position,
                    received_at,
                },
            );
        }
        (position, received_at)
    }

    pub(crate) fn position_for(&self, sender_id: &str) -> PlatformMessagePosition {
        let state = self.0.state.lock().unwrap();
        PlatformMessagePosition {
            total_messages: state.total_messages,
            sender_messages: state.sender_messages.get(sender_id).copied().unwrap_or(0),
        }
    }
}
