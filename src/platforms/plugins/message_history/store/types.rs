//! 消息历史的数据类型与入库前的清洗。
//!
//! 每个 `MAX_*` 都对应一列或一个集合的上限：这些内容全部来自平台，不设限就是
//! 把库的大小和内存交给对端。清洗（`sanitize_*`、`truncate_utf8`）在入库前做，
//! 因为**出库时再修就晚了**——脏数据已经在磁盘上了。

use crate::platforms::plugins::message_history::store::*;

pub(crate) const MAX_IDENTIFIER_BYTES: usize = 256;

pub(crate) const MAX_NAME_BYTES: usize = 512;

pub(crate) const MAX_TEXT_BYTES: usize = 64 * 1024;

pub(crate) const MAX_MEDIA_ITEMS: usize = 16;

pub(crate) const MAX_MENTIONED_USERS: usize = 32;

pub(crate) const MAX_MEDIA_LABEL_BYTES: usize = 512;

pub(crate) const MAX_MIME_BYTES: usize = 128;

pub(crate) const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct ConversationKey {
    pub(crate) platform: String,
    pub(crate) account_id: String,
    pub(crate) conversation_kind: String,
    pub(crate) conversation_id: String,
}

/// Compatibility name for real-context code that only reads group history.
pub(crate) type GroupKey = ConversationKey;

impl ConversationKey {
    /// Constructs a group conversation key for existing real-context callers.
    pub(crate) fn new(
        platform: impl Into<String>,
        account_id: impl Into<String>,
        group_id: impl Into<String>,
    ) -> Result<Self> {
        Self::for_kind(platform, account_id, ConversationKind::Group, group_id)
    }

    pub(crate) fn for_kind(
        platform: impl Into<String>,
        account_id: impl Into<String>,
        kind: ConversationKind,
        conversation_id: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            platform: validate_identifier("platform", platform.into())?,
            account_id: validate_identifier("account id", account_id.into())?,
            conversation_kind: kind.as_str().to_string(),
            conversation_id: validate_identifier("conversation id", conversation_id.into())?,
        })
    }

    pub(crate) fn platform(&self) -> &str {
        &self.platform
    }

    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }

    pub(crate) fn group_id(&self) -> &str {
        &self.conversation_id
    }

    pub(crate) fn conversation_kind(&self) -> &str {
        &self.conversation_kind
    }

    pub(crate) fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    pub(crate) fn is_group(&self) -> bool {
        self.conversation_kind == ConversationKind::Group.as_str()
    }

    pub(crate) fn account_scope(&self) -> AccountKey {
        AccountKey {
            platform: self.platform.clone(),
            account_id: self.account_id.clone(),
        }
    }
}

/// Account-wide history access is reserved for already-authorized tools. It
/// never crosses the platform or bot-account boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct AccountKey {
    pub(crate) platform: String,
    pub(crate) account_id: String,
}

impl AccountKey {
    pub(crate) fn new(platform: impl Into<String>, account_id: impl Into<String>) -> Result<Self> {
        Ok(Self {
            platform: validate_identifier("platform", platform.into())?,
            account_id: validate_identifier("account id", account_id.into())?,
        })
    }

    pub(crate) fn platform(&self) -> &str {
        &self.platform
    }

    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum HistoryScope {
    Group(GroupKey),
    Private(ConversationKey),
    /// 账号下全部群聊(不含私聊):`all_groups` 参数的字面语义。
    AllGroups(AccountKey),
    Account(AccountKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MediaKind {
    Image,
    Sticker,
    File,
    Audio,
    Video,
    Other,
}

/// Deliberately contains no URL, filesystem path, byte buffer, or Base64
/// field. History only needs enough structure to tell the model what appeared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MediaPlaceholder {
    pub(crate) kind: MediaKind,
    pub(crate) label: Option<String>,
    pub(crate) mime: Option<String>,
    /// Provider-side media id retained only for files: `read_platform_file`
    /// needs it to ask the bridge for a download URL. Never a local path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) media_id: Option<String>,
}

impl MediaPlaceholder {
    pub(crate) fn new(
        kind: MediaKind,
        label: Option<impl Into<String>>,
        mime: Option<impl Into<String>>,
    ) -> Self {
        Self {
            kind,
            label: label.map(Into::into),
            mime: mime.map(Into::into),
            media_id: None,
        }
    }

    pub(crate) fn with_media_id(mut self, media_id: Option<impl Into<String>>) -> Self {
        self.media_id = media_id.map(Into::into);
        self
    }

    pub(crate) fn sanitized(mut self) -> Self {
        self.label = self
            .label
            .map(|value| sanitize_single_line(&value, MAX_MEDIA_LABEL_BYTES))
            .filter(|value| !value.is_empty());
        self.mime = self
            .mime
            .map(|value| sanitize_single_line(&value, MAX_MIME_BYTES))
            .filter(|value| !value.is_empty());
        self.media_id = self
            .media_id
            .map(|value| sanitize_single_line(&value, MAX_IDENTIFIER_BYTES))
            .filter(|value| !value.is_empty());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct SanitizedContent {
    pub(crate) text: String,
    pub(crate) media: Vec<MediaPlaceholder>,
    pub(crate) mentioned_user_ids: Vec<String>,
    #[serde(default)]
    pub(crate) mentioned_users: Vec<PlatformMention>,
}

impl SanitizedContent {
    pub(crate) fn new(text: impl Into<String>, media: Vec<MediaPlaceholder>) -> Self {
        Self {
            text: text.into(),
            media,
            mentioned_user_ids: Vec::new(),
            mentioned_users: Vec::new(),
        }
    }

    pub(crate) fn sanitized(mut self) -> Result<Self> {
        self.text = sanitize_multiline(&self.text, MAX_TEXT_BYTES);
        self.media = self
            .media
            .into_iter()
            .take(MAX_MEDIA_ITEMS)
            .map(MediaPlaceholder::sanitized)
            .collect();
        let mut seen = HashSet::with_capacity(self.mentioned_user_ids.len());
        self.mentioned_user_ids = self
            .mentioned_user_ids
            .into_iter()
            .map(|value| validate_identifier("mentioned user id", value))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|value| seen.insert(value.clone()))
            .take(MAX_MENTIONED_USERS)
            .collect();
        let mut seen = HashSet::with_capacity(self.mentioned_users.len());
        self.mentioned_users = self
            .mentioned_users
            .into_iter()
            .map(|mention| {
                Ok(PlatformMention {
                    user_id: validate_identifier("mentioned user id", mention.user_id)?,
                    display_name: mention
                        .display_name
                        .map(|name| sanitize_single_line(&name, MAX_NAME_BYTES))
                        .filter(|name| !name.is_empty()),
                })
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|mention| seen.insert(mention.user_id.clone()))
            .take(MAX_MENTIONED_USERS)
            .collect();
        Ok(self)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NewHistoryMessage {
    pub(crate) group: GroupKey,
    pub(crate) message_id: String,
    pub(crate) sender_id: String,
    pub(crate) sender_name: String,
    pub(crate) content: SanitizedContent,
    pub(crate) reply_to_message_id: Option<String>,
    pub(crate) is_bot: bool,
    /// Unix timestamp supplied by the platform event.
    pub(crate) sent_at: i64,
    /// Monotonic receive order shared by all messages produced for one
    /// inbound turn. Legacy and externally recorded rows may omit it.
    pub(crate) ingress_order: Option<i64>,
}

impl NewHistoryMessage {
    pub(crate) fn sanitized(mut self) -> Result<Self> {
        self.message_id = validate_identifier("message id", self.message_id)?;
        self.sender_id = validate_identifier("sender id", self.sender_id)?;
        self.sender_name = sanitize_single_line(&self.sender_name, MAX_NAME_BYTES);
        if self.sender_name.is_empty() {
            self.sender_name.clone_from(&self.sender_id);
        }
        self.reply_to_message_id = self
            .reply_to_message_id
            .map(|value| validate_identifier("reply message id", value))
            .transpose()?;
        self.content = self.content.sanitized()?;
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HistoryMessage {
    pub(crate) row_id: i64,
    #[serde(rename = "conversation")]
    pub(crate) group: GroupKey,
    pub(crate) message_id: String,
    pub(crate) sender_id: String,
    pub(crate) sender_name: String,
    pub(crate) content: SanitizedContent,
    pub(crate) reply_to_message_id: Option<String>,
    pub(crate) is_bot: bool,
    pub(crate) sent_at: i64,
    pub(crate) ingress_order: Option<i64>,
    pub(crate) recalled_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct RecordOutcome {
    pub(crate) row_id: i64,
    pub(crate) inserted: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct NewRecall {
    pub(crate) group: GroupKey,
    pub(crate) message_id: String,
    pub(crate) operator_id: Option<String>,
    pub(crate) recalled_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct RecallOutcome {
    pub(crate) newly_recorded: bool,
    pub(crate) matched_message: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryCursor {
    pub(crate) sent_at: i64,
    pub(crate) row_id: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct ContextBoundary {
    pub(crate) after_row_id: i64,
    pub(crate) reset_at: i64,
}

pub(crate) fn validate_identifier(label: &str, value: String) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        bail!("{label} exceeds {MAX_IDENTIFIER_BYTES} bytes");
    }
    if value.chars().any(char::is_control) {
        bail!("{label} contains control characters");
    }
    Ok(value.to_string())
}

pub(crate) fn sanitize_multiline(value: &str, max_bytes: usize) -> String {
    let filtered = value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .collect::<String>();
    truncate_utf8(filtered.trim(), max_bytes)
}

pub(crate) fn sanitize_single_line(value: &str, max_bytes: usize) -> String {
    let filtered = value
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '\n' | '\r' | '\t') {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    truncate_utf8(filtered.trim(), max_bytes)
}

pub(crate) fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
