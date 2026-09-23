use anyhow::Result;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConversationKind {
    Private,
    Group,
}

impl ConversationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Group => "group",
        }
    }
}

/// Stable identity of a transport conversation. The bot account is part of
/// the key so two OneBot accounts can never share history or routing rules.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PlatformConversation {
    pub platform: String,
    pub account_id: String,
    pub kind: ConversationKind,
    pub conversation_id: String,
}

impl PlatformConversation {
    pub fn scope_key(&self) -> String {
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
pub struct PlatformPrincipal {
    pub platform: String,
    pub account_id: String,
    pub user_id: String,
}

impl PlatformPrincipal {
    pub fn stable_key(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        for component in [&self.platform, &self.account_id, &self.user_id] {
            hasher.update(&(component.len() as u64).to_le_bytes());
            hasher.update(component.as_bytes());
        }
        format!("principal:{}", &hasher.finalize().to_hex()[..24])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboundOrigin {
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
pub struct ResponseTarget {
    pub message_id: String,
    /// 平台自己的消息序号（QQ 的 msgSeq）。
    ///
    /// 和 `message_id` 并存是因为它们的可靠性不一样：NapCat 给出的
    /// `message_id` 是它进程内一张 5000 条的表映射出来的短号，表里没有了
    /// （它重启过、或被挤掉了）就**查不回原消息**——而它查不到时是**一声不吭
    /// 地把引用扔掉、消息照发**，我们这边看到的是发送成功。序号走的是协议本
    /// 身的字段，不依赖那张表，所以有就带上，NapCat 也优先用它。
    pub message_seq: Option<i64>,
    pub user_id: String,
    pub quote: bool,
    pub mention: bool,
    pub explicit_mention_user_ids: Vec<String>,
}

impl ResponseTarget {
    /// 「这条消息谁也不引用、谁也不艾特」。
    ///
    /// 不是 `None`：`None` 的意思是「还没定，去把本回合预留的那个取走」
    /// (`turn_context` 的 `reserved_target`)，而这里要表达的是**已经定了，就是
    /// 不带**。表情包那条消息就走这个——它跟在正文后面(或前面)，再挂一次引用
    /// 会让群里看着像回了两遍(09-21 用户拍板：表情包永远不带引用和艾特)。
    pub fn silent() -> Self {
        Self {
            message_id: String::new(),
            message_seq: None,
            user_id: String::new(),
            quote: false,
            mention: false,
            explicit_mention_user_ids: Vec::new(),
        }
    }

    pub fn is_effective(&self) -> bool {
        (self.quote && !self.message_id.is_empty())
            || (self.mention && !self.user_id.is_empty())
            || !self.explicit_mention_user_ids.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformInboundEventKind {
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
pub struct PlatformMessagePosition {
    pub total_messages: u64,
    pub sender_messages: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformMediaKind {
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
pub struct PlatformInboundMedia {
    pub kind: PlatformMediaKind,
    pub id: Option<String>,
    pub name: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlatformMention {
    pub user_id: String,
    pub display_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PlatformImageData {
    pub mime: String,
    pub data: Arc<[u8]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformContextImageRef {
    pub id: String,
    pub message_id: String,
    /// One-based position among image segments in the source message.
    pub image_index: usize,
}

/// Stable reference to one file placeholder rendered into platform history.
/// `file_id` is the provider-side identifier used to resolve the download URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformContextFileRef {
    pub id: String,
    pub message_id: String,
    /// One-based position among file segments in the source message.
    pub file_index: usize,
    pub file_id: String,
    pub file_name: String,
    /// Direct download URL when the bridge supplied one. Lazy downloads use
    /// this first; history-derived refs leave it empty and ask the bridge.
    pub url: Option<String>,
}

/// A platform file downloaded into Miyu's local platform file cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFileDownload {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
}

/// Protocol-neutral view of an inbound event. It contains stable identities
/// rather than display names alone, allowing history and trigger plugins to
/// avoid confusing users with the same nickname.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformInboundEvent {
    pub kind: PlatformInboundEventKind,
    pub conversation: PlatformConversation,
    pub conversation_display_name: Option<String>,
    pub message_id: String,
    /// 平台自己的消息序号（QQ 的 msgSeq）。引用这条消息时带上它，见
    /// [`ResponseTarget::message_seq`]。
    pub message_seq: Option<i64>,
    pub sender_id: String,
    pub sender_display_name: String,
    pub operator_id: Option<String>,
    pub timestamp: i64,
    /// Local monotonic receive time, unaffected by platform clock skew.
    pub received_at: Instant,
    /// Per-conversation position captured before asynchronous event handling.
    pub message_position: Option<PlatformMessagePosition>,
    /// Monotonic transport-receive order. History consumers use this instead
    /// of database insertion order so async metadata lookups cannot expose a
    /// later message to an earlier turn.
    pub ingress_order: Option<i64>,
    pub text: String,
    pub reply_to_message_id: Option<String>,
    pub replied_message: Option<PlatformMessageInfo>,
    pub mentioned_user_ids: Vec<String>,
    pub mentioned_users: Vec<PlatformMention>,
    /// Whether the platform identified the bot itself in an @ mention.
    pub mentioned_bot: bool,
    pub media: Vec<PlatformInboundMedia>,
    pub notice_sub_type: Option<String>,
    pub duration_seconds: Option<u64>,
}

/// Mutable trigger state passed through platform plugins. The core seeds it
/// with the platform's normal trigger result; a plugin may take over that
/// result without bypassing command handling or output ownership.
#[derive(Clone, Debug)]
pub struct TriggerDecision {
    pub should_reply: bool,
    pub content: String,
    pub response_target: Option<ResponseTarget>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformMessageInfo {
    pub message_id: String,
    pub sender_id: String,
    pub sender_display_name: String,
    pub timestamp: i64,
    pub text: String,
    pub reply_to_message_id: Option<String>,
    pub mentioned_user_ids: Vec<String>,
    pub mentioned_users: Vec<PlatformMention>,
    pub media: Vec<PlatformInboundMedia>,
    pub conversation_kind: Option<ConversationKind>,
    pub conversation_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformGroupMember {
    pub group_id: String,
    pub user_id: String,
    pub nickname: String,
    pub card: String,
    pub role: String,
    pub title: String,
    pub joined_at: i64,
    pub last_active_at: i64,
}

impl PlatformGroupMember {
    pub fn display_name(&self) -> &str {
        if self.card.trim().is_empty() {
            &self.nickname
        } else {
            &self.card
        }
    }
}

#[derive(Clone, Debug)]
pub enum OutboundSegment {
    /// Agent output that still contains Markdown. Adapters flatten this only
    /// after plugins have had a chance to render it.
    Markdown(String),
    Text(String),
    Mention(String),
    ImageBytes {
        #[allow(dead_code)] // 出站段的 DTO 字段,适配器按需读
        mime: String,
        data: Arc<[u8]>,
        #[allow(dead_code)] // 出站段的 DTO 字段,适配器按需读
        alt: String,
    },
    ImagePath {
        path: PathBuf,
        #[allow(dead_code)] // 出站段的 DTO 字段,适配器按需读
        alt: String,
    },
    FilePath {
        path: PathBuf,
        name: Option<String>,
    },
    /// 语音消息(wav/mp3 文件),适配器按平台规则单独成一条消息发。
    /// `transcript` 是合成前的原文,只进历史(`[语音] 原文`),不上平台。
    AudioPath {
        path: PathBuf,
        transcript: String,
    },
}

/// 语音消息在历史库里的样子:`[语音] 原文`,没有原文时只留 `[语音]`。
/// 入站(别人发的语音转写)与出站(她自己合成的语音)共用同一格式。
pub fn voice_history_text(transcript: &str) -> String {
    let transcript = transcript.trim();
    if transcript.is_empty() {
        "[语音]".to_string()
    } else {
        format!("[语音] {transcript}")
    }
}

#[derive(Clone, Debug)]
pub struct ForwardNode {
    pub user_id: String,
    pub display_name: String,
    pub segments: Vec<OutboundSegment>,
}

#[derive(Clone, Debug)]
pub enum OutboundBody {
    Segments(Vec<OutboundSegment>),
    Forward(Vec<ForwardNode>),
}

#[derive(Clone, Debug)]
pub struct OutboundMessage {
    pub body: OutboundBody,
    pub response_target: Option<ResponseTarget>,
    pub origin: OutboundOrigin,
    /// Plugin-private, in-memory metadata. Adapters never serialize it.
    pub metadata: BTreeMap<String, Value>,
}

impl OutboundMessage {
    pub fn segments(origin: OutboundOrigin, segments: Vec<OutboundSegment>) -> Self {
        Self {
            body: OutboundBody::Segments(segments),
            response_target: None,
            origin,
            metadata: BTreeMap::new(),
        }
    }

    pub fn text(origin: OutboundOrigin, text: impl Into<String>) -> Self {
        Self::segments(origin, vec![OutboundSegment::Text(text.into())])
    }

    pub fn markdown(origin: OutboundOrigin, text: impl Into<String>) -> Self {
        Self::segments(origin, vec![OutboundSegment::Markdown(text.into())])
    }
}

#[derive(Clone, Debug, Default)]
pub struct SendReceipt {
    pub message_ids: Vec<String>,
    pub image_message_ids: Vec<String>,
    /// Number of platform operations that completed successfully, including
    /// operations whose API response did not contain a stable message id.
    pub delivered_parts: usize,
    /// Content digests for images confirmed by the platform adapter. Keeping
    /// these in the receipt lets a turn avoid re-sending tool-delivered media.
    pub image_digests: Vec<blake3::Hash>,
    /// Whether the platform confirmed the operation carrying the response
    /// quote and mentions. Partial failures must not re-arm a delivered target.
    pub response_target_delivered: bool,
}

impl SendReceipt {
    pub fn has_delivery(&self) -> bool {
        self.delivered_parts > 0
            || !self.message_ids.is_empty()
            || !self.image_message_ids.is_empty()
            || !self.image_digests.is_empty()
            || self.response_target_delivered
    }
}

#[derive(Debug)]
pub struct PartialSendError {
    error: anyhow::Error,
    receipt: SendReceipt,
}

impl PartialSendError {
    pub fn new(error: anyhow::Error, receipt: SendReceipt) -> Self {
        Self { error, receipt }
    }

    pub fn receipt(&self) -> &SendReceipt {
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
pub enum BotSendAvailability {
    Available,
    Muted,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BotGroupRole {
    Owner,
    Admin,
    Member,
    Unknown,
}

impl BotGroupRole {
    pub fn can_manage(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }
}

/// Protocol adapter capability used by the platform-neutral output pipeline.
pub trait PlatformAdapter: Send + Sync {
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

/// 工具层需要从平台回合上下文拿到的全部能力。
///
/// 只有这四件事：主体身份（记忆按它分区）、是否管理员、宿主工具是否放行、
/// 按消息 ID 取图。`PlatformTurnContext` 本身是个几百行的运行时结构，工具层
/// 为这四个方法依赖它，就等于把整个平台运行时钉进工具层——改平台的任何东西
/// 都要重编工具。
///
/// 定义在这里而不是 `platforms`：这一层是纯数据 + 契约，谁都能依赖。
pub trait PlatformToolContext: Send + Sync {
    /// 记忆分区用的主体身份。
    fn principal(&self) -> PlatformPrincipal;

    /// 当前发起者是不是管理员。
    fn is_admin(&self) -> bool;

    /// 发起者的显示名，记忆里用它标注"谁说的"。
    fn sender_display_name(&self) -> String;

    /// 宿主工具（读写文件、执行命令等）在这一轮是否放行。
    fn host_tools_allowed(&self) -> bool;

    /// 按消息 ID 取该消息里的图片。返回 boxed future 以便 trait object 化。
    fn message_images_task(
        &self,
        message_id: String,
    ) -> futures_util::future::BoxFuture<'static, anyhow::Result<Vec<PlatformImageData>>>;

    /// 把一个上下文文件引用(`file_<msg>_<n>`)懒下载到本地缓存。看视频
    /// 走这条:视频和文件共用同一条懒下载链路(09-04)。
    fn fetch_platform_file_task(
        &self,
        file_ref: PlatformContextFileRef,
    ) -> futures_util::future::BoxFuture<'static, anyhow::Result<PlatformFileDownload>>;
}

// ── QQ 头像 URL 的可信判定 ──
//
// 纯字符串判定，零依赖。工具层要用它来放行「模型只能分析可信来源的图片」这条
// 约束，放在 platforms 里会让 tools 为一个字符串函数依赖整个平台运行时。

/// Whether `url` is exactly one of the avatar URL shapes produced above.
/// Used by the scoped vision gate to admit avatar lookups while still
/// rejecting every other remote URL.
pub fn is_trusted_avatar_url(url: &str) -> bool {
    is_user_avatar_url(url) || is_group_avatar_url(url)
}

pub(crate) fn is_user_avatar_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://q.qlogo.cn/headimg_dl?dst_uin=") else {
        return false;
    };
    let Some((uin, spec)) = rest.split_once("&spec=") else {
        return false;
    };
    is_numeric_id(uin) && is_numeric_id(spec)
}

pub(crate) fn is_group_avatar_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://p.qlogo.cn/gh/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let (Some(first), Some(second), Some(size), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    first == second && is_numeric_id(first) && is_numeric_id(size)
}

pub fn is_numeric_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
