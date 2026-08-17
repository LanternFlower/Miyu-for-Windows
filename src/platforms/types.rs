use anyhow::Result;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ConversationKind {
    Private,
    Group,
}

impl ConversationKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Group => "group",
        }
    }
}

/// Stable identity of a transport conversation. The bot account is part of
/// the key so two OneBot accounts can never share history or routing rules.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PlatformConversation {
    pub(crate) platform: String,
    pub(crate) account_id: String,
    pub(crate) kind: ConversationKind,
    pub(crate) conversation_id: String,
}

impl PlatformConversation {
    pub(crate) fn scope_key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.platform,
            self.account_id,
            self.kind.as_str(),
            self.conversation_id
        )
    }
}

/// Authenticated transport principal for one inbound actor. Display names are
/// deliberately excluded: they are presentation metadata, not identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PlatformPrincipal {
    pub(crate) platform: String,
    pub(crate) account_id: String,
    pub(crate) user_id: String,
}

impl PlatformPrincipal {
    pub(crate) fn stable_key(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        for component in [&self.platform, &self.account_id, &self.user_id] {
            hasher.update(&(component.len() as u64).to_le_bytes());
            hasher.update(component.as_bytes());
        }
        format!("principal:{}", &hasher.finalize().to_hex()[..24])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutboundOrigin {
    FinalReply,
    /// A completed model round sent mid-turn while later rounds are still
    /// running. Unlike `FinalReply` it must not consume the reserved response
    /// target, and unlike `Tool` it must not suppress the final reply.
    IntermediateReply,
    Tool,
    Command,
    Plugin,
}

/// The reply target selected for one platform turn. Target selection belongs
/// to the trigger pipeline; explicit mentions may replace its automatic
/// mention while preserving the quoted message and adaptive policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResponseTarget {
    pub(crate) message_id: String,
    pub(crate) user_id: String,
    pub(crate) quote: bool,
    pub(crate) mention: bool,
    pub(crate) explicit_mention_user_ids: Vec<String>,
}

impl ResponseTarget {
    pub(crate) fn quoted(message_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            user_id: user_id.into(),
            quote: true,
            mention: false,
            explicit_mention_user_ids: Vec::new(),
        }
    }

    pub(crate) fn is_effective(&self) -> bool {
        (self.quote && !self.message_id.is_empty())
            || (self.mention && !self.user_id.is_empty())
            || !self.explicit_mention_user_ids.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlatformInboundEventKind {
    Message,
    MessageRecall,
    GroupBan,
    GroupDecrease,
    GroupFileUpload,
}

/// Receive-time counters for one message in its transport conversation.
/// The sender counter lets reply delivery distinguish intervening messages
/// from the target user's own follow-ups without retaining message bodies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PlatformMessagePosition {
    pub(crate) total_messages: u64,
    pub(crate) sender_messages: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlatformMediaKind {
    Image,
    File,
    Audio,
    Video,
    Emoji,
    Other,
}

/// Lightweight media metadata retained for plugins. Binary/base64 payloads
/// are deliberately excluded so observing a busy group cannot pin large
/// allocations in memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlatformInboundMedia {
    pub(crate) kind: PlatformMediaKind,
    pub(crate) id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PlatformMention {
    pub(crate) user_id: String,
    pub(crate) display_name: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct PlatformImageData {
    pub(crate) mime: String,
    pub(crate) data: Arc<[u8]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlatformContextImageRef {
    pub(crate) id: String,
    pub(crate) message_id: String,
    /// One-based position among image segments in the source message.
    pub(crate) image_index: usize,
}

/// Stable reference to one file placeholder rendered into platform history.
/// `file_id` is the provider-side identifier used to resolve the download URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlatformContextFileRef {
    pub(crate) id: String,
    pub(crate) message_id: String,
    /// One-based position among file segments in the source message.
    pub(crate) file_index: usize,
    pub(crate) file_id: String,
    pub(crate) file_name: String,
    /// Direct download URL when the bridge supplied one. Lazy downloads use
    /// this first; history-derived refs leave it empty and ask the bridge.
    pub(crate) url: Option<String>,
}

/// A platform file downloaded into Miyu's local platform file cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlatformFileDownload {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) size: u64,
}

/// Protocol-neutral view of an inbound event. It contains stable identities
/// rather than display names alone, allowing history and trigger plugins to
/// avoid confusing users with the same nickname.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlatformInboundEvent {
    pub(crate) kind: PlatformInboundEventKind,
    pub(crate) conversation: PlatformConversation,
    pub(crate) conversation_display_name: Option<String>,
    pub(crate) message_id: String,
    pub(crate) sender_id: String,
    pub(crate) sender_display_name: String,
    pub(crate) operator_id: Option<String>,
    pub(crate) timestamp: i64,
    /// Local monotonic receive time, unaffected by platform clock skew.
    pub(crate) received_at: Instant,
    /// Per-conversation position captured before asynchronous event handling.
    pub(crate) message_position: Option<PlatformMessagePosition>,
    /// Monotonic transport-receive order. History consumers use this instead
    /// of database insertion order so async metadata lookups cannot expose a
    /// later message to an earlier turn.
    pub(crate) ingress_order: Option<i64>,
    pub(crate) text: String,
    pub(crate) reply_to_message_id: Option<String>,
    pub(crate) replied_message: Option<PlatformMessageInfo>,
    pub(crate) mentioned_user_ids: Vec<String>,
    pub(crate) mentioned_users: Vec<PlatformMention>,
    /// Whether the platform identified the bot itself in an @ mention.
    pub(crate) mentioned_bot: bool,
    pub(crate) media: Vec<PlatformInboundMedia>,
    pub(crate) notice_sub_type: Option<String>,
    pub(crate) duration_seconds: Option<u64>,
}

/// Mutable trigger state passed through platform plugins. The core seeds it
/// with the platform's normal trigger result; a plugin may take over that
/// result without bypassing command handling or output ownership.
#[derive(Clone, Debug)]
pub(crate) struct TriggerDecision {
    pub(crate) should_reply: bool,
    pub(crate) content: String,
    pub(crate) response_target: Option<ResponseTarget>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlatformMessageInfo {
    pub(crate) message_id: String,
    pub(crate) sender_id: String,
    pub(crate) sender_display_name: String,
    pub(crate) timestamp: i64,
    pub(crate) text: String,
    pub(crate) reply_to_message_id: Option<String>,
    pub(crate) mentioned_user_ids: Vec<String>,
    pub(crate) mentioned_users: Vec<PlatformMention>,
    pub(crate) media: Vec<PlatformInboundMedia>,
    pub(crate) conversation_kind: Option<ConversationKind>,
    pub(crate) conversation_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlatformGroupMember {
    pub(crate) group_id: String,
    pub(crate) user_id: String,
    pub(crate) nickname: String,
    pub(crate) card: String,
    pub(crate) role: String,
    pub(crate) title: String,
    pub(crate) joined_at: i64,
    pub(crate) last_active_at: i64,
}

impl PlatformGroupMember {
    pub(crate) fn display_name(&self) -> &str {
        if self.card.trim().is_empty() {
            &self.nickname
        } else {
            &self.card
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum OutboundSegment {
    /// Agent output that still contains Markdown. Adapters flatten this only
    /// after plugins have had a chance to render it.
    Markdown(String),
    Text(String),
    Mention(String),
    ImageBytes {
        mime: String,
        data: Arc<[u8]>,
        alt: String,
    },
    ImagePath {
        path: PathBuf,
        alt: String,
    },
    FilePath {
        path: PathBuf,
        name: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ForwardNode {
    pub(crate) user_id: String,
    pub(crate) display_name: String,
    pub(crate) segments: Vec<OutboundSegment>,
}

#[derive(Clone, Debug)]
pub(crate) enum OutboundBody {
    Segments(Vec<OutboundSegment>),
    Forward(Vec<ForwardNode>),
}

#[derive(Clone, Debug)]
pub(crate) struct OutboundMessage {
    pub(crate) body: OutboundBody,
    pub(crate) response_target: Option<ResponseTarget>,
    pub(crate) origin: OutboundOrigin,
    /// Plugin-private, in-memory metadata. Adapters never serialize it.
    pub(crate) metadata: BTreeMap<String, Value>,
}

impl OutboundMessage {
    pub(crate) fn segments(origin: OutboundOrigin, segments: Vec<OutboundSegment>) -> Self {
        Self {
            body: OutboundBody::Segments(segments),
            response_target: None,
            origin,
            metadata: BTreeMap::new(),
        }
    }

    pub(crate) fn text(origin: OutboundOrigin, text: impl Into<String>) -> Self {
        Self::segments(origin, vec![OutboundSegment::Text(text.into())])
    }

    pub(crate) fn markdown(origin: OutboundOrigin, text: impl Into<String>) -> Self {
        Self::segments(origin, vec![OutboundSegment::Markdown(text.into())])
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SendReceipt {
    pub(crate) message_ids: Vec<String>,
    pub(crate) image_message_ids: Vec<String>,
    /// Number of platform operations that completed successfully, including
    /// operations whose API response did not contain a stable message id.
    pub(crate) delivered_parts: usize,
    /// Content digests for images confirmed by the platform adapter. Keeping
    /// these in the receipt lets a turn avoid re-sending tool-delivered media.
    pub(crate) image_digests: Vec<blake3::Hash>,
    /// Whether the platform confirmed the operation carrying the response
    /// quote and mentions. Partial failures must not re-arm a delivered target.
    pub(crate) response_target_delivered: bool,
}

impl SendReceipt {
    pub(crate) fn has_delivery(&self) -> bool {
        self.delivered_parts > 0
            || !self.message_ids.is_empty()
            || !self.image_message_ids.is_empty()
            || !self.image_digests.is_empty()
            || self.response_target_delivered
    }
}

#[derive(Debug)]
pub(crate) struct PartialSendError {
    error: anyhow::Error,
    receipt: SendReceipt,
}

impl PartialSendError {
    pub(crate) fn new(error: anyhow::Error, receipt: SendReceipt) -> Self {
        Self { error, receipt }
    }

    pub(crate) fn receipt(&self) -> &SendReceipt {
        &self.receipt
    }
}

impl std::fmt::Display for PartialSendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for PartialSendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BotSendAvailability {
    Available,
    Muted,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BotGroupRole {
    Owner,
    Admin,
    Member,
    Unknown,
}

impl BotGroupRole {
    pub(crate) fn can_manage(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }
}

/// Protocol adapter capability used by the platform-neutral output pipeline.
pub(crate) trait PlatformAdapter: Send + Sync {
    fn send<'a>(&'a self, message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>>;

    fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>>;

    fn bot_send_availability<'a>(&'a self) -> BoxFuture<'a, Result<BotSendAvailability>> {
        Box::pin(async { Ok(BotSendAvailability::Unknown) })
    }

    fn set_message_reaction<'a>(
        &'a self,
        _message_id: &'a str,
        _reaction_id: &'a str,
        _active: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("message reactions are not supported by this platform") })
    }

    fn message_info<'a>(
        &'a self,
        _message_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<PlatformMessageInfo>>> {
        Box::pin(async { anyhow::bail!("message lookup is not supported by this platform") })
    }

    fn message_images<'a>(
        &'a self,
        _message_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PlatformImageData>>> {
        Box::pin(async { anyhow::bail!("message image lookup is not supported by this platform") })
    }

    /// Resolve one context-file placeholder into a locally cached file. The
    /// platform is responsible for provider URL lookup, size-capped download,
    /// and storage under `paths.cache_dir/platform_files/<platform>/`.
    fn fetch_platform_file<'a>(
        &'a self,
        _file_ref: &'a PlatformContextFileRef,
        _paths: &'a crate::paths::MiyuPaths,
    ) -> BoxFuture<'a, Result<PlatformFileDownload>> {
        Box::pin(async {
            anyhow::bail!("platform file downloads are not supported by this platform")
        })
    }

    fn group_members<'a>(&'a self) -> BoxFuture<'a, Result<Vec<PlatformGroupMember>>> {
        Box::pin(async { anyhow::bail!("group member lookup is not supported by this platform") })
    }

    fn group_member<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<PlatformGroupMember>>> {
        Box::pin(async { anyhow::bail!("group member lookup is not supported by this platform") })
    }

    /// Same lookup, but asks the platform to skip its own roster cache.
    /// Destructive actions validate through this: a stale roster is how
    /// already-departed members used to pass validation and then fail at the
    /// API. Platforms without a cache distinction just reuse `group_member`.
    fn group_member_fresh<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<PlatformGroupMember>>> {
        self.group_member(user_id)
    }

    fn bot_group_role<'a>(&'a self) -> BoxFuture<'a, Result<BotGroupRole>> {
        Box::pin(async { Ok(BotGroupRole::Unknown) })
    }

    fn delete_message<'a>(&'a self, _message_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("message recall is not supported by this platform") })
    }

    fn set_group_ban<'a>(
        &'a self,
        _user_id: &'a str,
        _duration_seconds: u64,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("group bans are not supported by this platform") })
    }

    fn set_group_kick<'a>(
        &'a self,
        _user_id: &'a str,
        _reject_add_request: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("group kicks are not supported by this platform") })
    }

    fn set_group_special_title<'a>(
        &'a self,
        _user_id: &'a str,
        _special_title: &'a str,
        _duration_seconds: i64,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("group titles are not supported by this platform") })
    }
}
