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

/// 跨回合文字近似闸的记忆窗口。并发回合各答各的,回合内幂等闸看不见
/// 对方(08-25 拯救者 21 秒内被答两遍);短 TTL 的会话级记忆补上这个盲区。
/// TTL 故意短:两个人隔几分钟问同一件事,理应各得其答。
const RECENT_REPLY_TTL: Duration = Duration::from_secs(120);
const RECENT_REPLIES_PER_CONVERSATION: usize = 8;

#[allow(clippy::type_complexity)]
fn recent_replies(
) -> &'static Mutex<HashMap<String, Vec<(std::collections::HashSet<(char, char)>, Instant)>>> {
    static LEDGER: OnceLock<
        Mutex<HashMap<String, Vec<(std::collections::HashSet<(char, char)>, Instant)>>>,
    > = OnceLock::new();
    LEDGER.get_or_init(Mutex::default)
}

/// 最近的群成员移除台账(08-25 批量踢人假失败取证):snowluma 的成员表
/// 缓存滞后可达数秒,踢成功后连查三次仍"在群";而 QQ 的 group_decrease
/// 通知在踢成功后 1-2 秒内必达——事件才是权威判据,轮询只作兜底。
const RECENT_REMOVAL_TTL: Duration = Duration::from_secs(10 * 60);

fn recent_removals() -> &'static Mutex<HashMap<String, Vec<(String, Instant)>>> {
    static LEDGER: OnceLock<Mutex<HashMap<String, Vec<(String, Instant)>>>> = OnceLock::new();
    LEDGER.get_or_init(Mutex::default)
}

pub(crate) fn record_group_removal(scope_key: &str, user_id: &str) {
    if user_id.is_empty() {
        return;
    }
    let now = Instant::now();
    let mut ledger = recent_removals().lock().unwrap();
    ledger.retain(|_, entries| {
        entries.retain(|(_, at)| now.duration_since(*at) < RECENT_REMOVAL_TTL);
        !entries.is_empty()
    });
    ledger
        .entry(scope_key.to_string())
        .or_default()
        .push((user_id.to_string(), now));
}

pub(crate) fn recent_group_removal(scope_key: &str, user_id: &str) -> bool {
    let now = Instant::now();
    recent_removals()
        .lock()
        .unwrap()
        .get(scope_key)
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|(id, at)| id == user_id && now.duration_since(*at) < RECENT_REMOVAL_TTL)
        })
}

pub(crate) fn record_recent_conversation_reply(scope_key: &str, text: &str) {
    let grams = crate::platforms::turn_context::reply_text_bigrams(text);
    if grams.len() < 16 {
        return;
    }
    let now = Instant::now();
    let mut ledger = recent_replies().lock().unwrap();
    ledger.retain(|_, entries| {
        entries.retain(|(_, at)| now.duration_since(*at) < RECENT_REPLY_TTL);
        !entries.is_empty()
    });
    let entries = ledger.entry(scope_key.to_string()).or_default();
    entries.push((grams, now));
    if entries.len() > RECENT_REPLIES_PER_CONVERSATION {
        let excess = entries.len() - RECENT_REPLIES_PER_CONVERSATION;
        entries.drain(..excess);
    }
}

/// 该会话近两分钟内是否投递过与 `text` 高度近似的回复(0.75 比回合内闸
/// 更严:跨回合误杀的代价是有人被已读不回,阈值宁高勿低)。
pub(crate) fn recent_conversation_reply_similar(scope_key: &str, text: &str) -> bool {
    let grams = crate::platforms::turn_context::reply_text_bigrams(text);
    if grams.len() < 16 {
        return false;
    }
    let now = Instant::now();
    recent_replies()
        .lock()
        .unwrap()
        .get(scope_key)
        .is_some_and(|entries| {
            entries
                .iter()
                .filter(|(_, at)| now.duration_since(*at) < RECENT_REPLY_TTL)
                .any(|(known, _)| {
                    crate::platforms::turn_context::bigram_jaccard(&grams, known) >= 0.75
                })
        })
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
            .filter_map(|(key, entries)| entries.last().map(|(_, at)| (*at, key.clone())))
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
    /// 会话里最后一条消息是不是机器人自己发的。入站消息把它清掉,投递成功
    /// 把它立起来——只看"最后一条",不计数,所以不需要清理策略。
    pub(crate) last_message_is_own: bool,
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
        state.last_message_is_own = false;
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

    /// 投递成功后登记:此刻会话里最后一条是自己发的。
    pub(crate) fn record_own_message(&self) {
        self.0.state.lock().unwrap().last_message_is_own = true;
    }

    /// 会话里最后一条是不是自己发的。连发时用它兜住引用——见
    /// `AdaptiveResponseTargetPolicy::resolve`。
    pub(crate) fn last_message_is_own(&self) -> bool {
        self.0.state.lock().unwrap().last_message_is_own
    }

    pub(crate) fn position_for(&self, sender_id: &str) -> PlatformMessagePosition {
        let state = self.0.state.lock().unwrap();
        PlatformMessagePosition {
            total_messages: state.total_messages,
            sender_messages: state.sender_messages.get(sender_id).copied().unwrap_or(0),
        }
    }
}
