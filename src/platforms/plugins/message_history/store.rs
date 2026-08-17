use crate::platforms::{ConversationKind, PlatformMention};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

const SCHEMA_VERSION: i64 = 4;
const DEFAULT_QUEUE_CAPACITY: usize = 128;
const MAX_QUEUE_CAPACITY: usize = 4_096;
const MAX_BATCH_MESSAGES: usize = 256;
const DEFAULT_DELETE_BATCH_SIZE: usize = 1_000;
const MAX_DELETE_BATCH_SIZE: usize = 5_000;
const MAX_PAGE_SIZE: usize = 1_000;
const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_NAME_BYTES: usize = 512;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_MEDIA_ITEMS: usize = 16;
const MAX_MENTIONED_USERS: usize = 32;
const MAX_MEDIA_LABEL_BYTES: usize = 512;
const MAX_MIME_BYTES: usize = 128;
const MAX_SEARCH_BYTES: usize = 1_024;
const MAX_SEARCH_TERMS: usize = 32;
const MAX_ACTIVITY_RANKING_LIMIT: usize = 200;
const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct ConversationKey {
    platform: String,
    account_id: String,
    conversation_kind: String,
    conversation_id: String,
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
    platform: String,
    account_id: String,
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

    fn sanitized(mut self) -> Self {
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

    fn sanitized(mut self) -> Result<Self> {
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
    fn sanitized(mut self) -> Result<Self> {
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

#[derive(Debug, Clone)]
pub(crate) struct RecentQuery {
    pub(crate) group: GroupKey,
    pub(crate) persona_scope: String,
    pub(crate) before: Option<HistoryCursor>,
    pub(crate) limit: usize,
    pub(crate) respect_context_boundary: bool,
    pub(crate) include_recalled: bool,
    pub(crate) before_ingress_order: Option<i64>,
    /// Lower bound used by the reply turn: everything the previous turn already
    /// rendered stays in the conversation history, so a turn only has to carry
    /// what arrived since.
    pub(crate) after_ingress_order: Option<i64>,
}

impl RecentQuery {
    pub(crate) fn for_context(
        group: GroupKey,
        persona_scope: impl Into<String>,
        limit: usize,
    ) -> Self {
        Self {
            group,
            persona_scope: persona_scope.into(),
            before: None,
            limit,
            respect_context_boundary: true,
            include_recalled: false,
            before_ingress_order: None,
            after_ingress_order: None,
        }
    }

    pub(crate) fn for_history(group: GroupKey, limit: usize) -> Self {
        Self {
            group,
            persona_scope: String::new(),
            before: None,
            limit,
            respect_context_boundary: false,
            include_recalled: false,
            before_ingress_order: None,
            after_ingress_order: None,
        }
    }

    pub(crate) fn after_ingress_order(mut self, order: Option<i64>) -> Self {
        self.after_ingress_order = order;
        self
    }

    pub(crate) fn before_ingress_order(mut self, order: Option<i64>) -> Self {
        self.before_ingress_order = order;
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SearchQuery {
    pub(crate) scope: HistoryScope,
    pub(crate) text: String,
    pub(crate) sender_id: Option<String>,
    pub(crate) before: Option<HistoryCursor>,
    pub(crate) since: Option<i64>,
    pub(crate) until: Option<i64>,
    pub(crate) limit: usize,
    pub(crate) include_recalled: bool,
    pub(crate) include_bot: bool,
}

impl SearchQuery {
    pub(crate) fn new(scope: HistoryScope, text: impl Into<String>, limit: usize) -> Self {
        Self {
            scope,
            text: text.into(),
            sender_id: None,
            before: None,
            since: None,
            until: None,
            limit,
            include_recalled: false,
            include_bot: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HistoryPage {
    /// Search results are newest-first. Recent-history results are chronological
    /// within the selected newest page so they can be injected directly.
    pub(crate) messages: Vec<HistoryMessage>,
    pub(crate) next_cursor: Option<HistoryCursor>,
}

#[derive(Debug, Clone)]
pub(crate) struct ActivityRankingQuery {
    pub(crate) group: GroupKey,
    pub(crate) since: i64,
    pub(crate) until: i64,
    pub(crate) limit: usize,
    pub(crate) include_bot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ActivityRankingItem {
    pub(crate) rank: u64,
    pub(crate) sender_id: String,
    pub(crate) sender_name: String,
    pub(crate) message_count: u64,
    pub(crate) active_days: u64,
    pub(crate) first_sent_at: i64,
    pub(crate) last_sent_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ActivityRanking {
    pub(crate) total_messages: u64,
    pub(crate) participant_count: u64,
    pub(crate) items: Vec<ActivityRankingItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DeleteMode {
    All,
    KeepDays(u32),
}

#[derive(Debug, Clone)]
pub(crate) struct DeleteRequest {
    pub(crate) scope: HistoryScope,
    pub(crate) mode: DeleteMode,
    pub(crate) sender_id: Option<String>,
    pub(crate) since: Option<i64>,
    pub(crate) until: Option<i64>,
    /// Unix timestamp used as a stable reference for `KeepDays`.
    pub(crate) now: i64,
    pub(crate) batch_size: usize,
}

impl DeleteRequest {
    pub(crate) fn all(scope: HistoryScope, now: i64) -> Self {
        Self {
            scope,
            mode: DeleteMode::All,
            sender_id: None,
            since: None,
            until: None,
            now,
            batch_size: DEFAULT_DELETE_BATCH_SIZE,
        }
    }

    pub(crate) fn keep_days(scope: HistoryScope, days: u32, now: i64) -> Result<Self> {
        if days == 0 {
            bail!("keep_days must be a positive integer");
        }
        Ok(Self {
            scope,
            mode: DeleteMode::KeepDays(days),
            sender_id: None,
            since: None,
            until: None,
            now,
            batch_size: DEFAULT_DELETE_BATCH_SIZE,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct DeleteReport {
    pub(crate) messages_deleted: u64,
    pub(crate) recalls_deleted: u64,
    pub(crate) boundaries_deleted: u64,
    pub(crate) batches: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct ContextBoundary {
    pub(crate) after_row_id: i64,
    pub(crate) reset_at: i64,
}

/// Cheap-to-clone, backpressured handle to a single SQLite owner thread.
/// Construction does not create a directory, DB, thread, or SQLite connection.
#[derive(Clone)]
pub(crate) struct HistoryStore {
    inner: Arc<HistoryStoreInner>,
}

impl std::fmt::Debug for HistoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryStore")
            .field("db_path", &self.inner.db_path)
            .field("queue_capacity", &self.inner.queue_capacity)
            .finish_non_exhaustive()
    }
}

struct HistoryStoreInner {
    db_path: PathBuf,
    queue_capacity: usize,
    actor: Mutex<Option<mpsc::Sender<Command>>>,
}

impl HistoryStore {
    pub(crate) fn new(db_path: impl Into<PathBuf>) -> Self {
        Self::with_queue_capacity(db_path, DEFAULT_QUEUE_CAPACITY)
    }

    pub(crate) fn with_queue_capacity(db_path: impl Into<PathBuf>, queue_capacity: usize) -> Self {
        Self {
            inner: Arc::new(HistoryStoreInner {
                db_path: db_path.into(),
                queue_capacity: queue_capacity.clamp(1, MAX_QUEUE_CAPACITY),
                actor: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn db_path(&self) -> &Path {
        &self.inner.db_path
    }

    pub(crate) async fn record_message(&self, message: NewHistoryMessage) -> Result<RecordOutcome> {
        let mut outcomes = self.record_messages(vec![message]).await?;
        outcomes
            .pop()
            .ok_or_else(|| anyhow!("history actor returned no record outcome"))
    }

    pub(crate) async fn record_messages(
        &self,
        messages: Vec<NewHistoryMessage>,
    ) -> Result<Vec<RecordOutcome>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        if messages.len() > MAX_BATCH_MESSAGES {
            bail!("history record batch exceeds the limit of {MAX_BATCH_MESSAGES} messages");
        }
        let messages = messages
            .into_iter()
            .map(NewHistoryMessage::sanitized)
            .collect::<Result<Vec<_>>>()?;
        self.request(|reply| Command::Record { messages, reply })
            .await
    }

    pub(crate) async fn record_recall(&self, mut recall: NewRecall) -> Result<RecallOutcome> {
        recall.message_id = validate_identifier("message id", recall.message_id)?;
        recall.operator_id = recall
            .operator_id
            .map(|value| validate_identifier("recall operator id", value))
            .transpose()?;
        self.request(|reply| Command::Recall { recall, reply })
            .await
    }

    pub(crate) async fn reset_context(
        &self,
        group: GroupKey,
        persona_scope: String,
        reset_at: i64,
    ) -> Result<ContextBoundary> {
        self.request(|reply| Command::ResetContext {
            group,
            persona_scope,
            reset_at,
            reply,
        })
        .await
    }

    pub(crate) async fn context_boundary(
        &self,
        group: GroupKey,
        persona_scope: String,
    ) -> Result<Option<ContextBoundary>> {
        self.request(|reply| Command::GetBoundary {
            group,
            persona_scope,
            reply,
        })
        .await
    }

    pub(crate) async fn recent(&self, query: RecentQuery) -> Result<HistoryPage> {
        self.request(|reply| Command::Recent { query, reply }).await
    }

    pub(crate) async fn search(&self, mut query: SearchQuery) -> Result<HistoryPage> {
        query.sender_id = query
            .sender_id
            .map(|value| validate_identifier("sender id", value))
            .transpose()?;
        if query
            .since
            .zip(query.until)
            .is_some_and(|(from, to)| from > to)
        {
            bail!("history search time range must have since <= until");
        }
        self.request(|reply| Command::Search { query, reply }).await
    }

    pub(crate) async fn activity_ranking(
        &self,
        mut query: ActivityRankingQuery,
    ) -> Result<ActivityRanking> {
        if query.since > query.until {
            bail!("activity ranking time range must have since <= until");
        }
        query.limit = query.limit.clamp(1, MAX_ACTIVITY_RANKING_LIMIT);
        self.request(|reply| Command::ActivityRanking { query, reply })
            .await
    }

    /// The caller must complete Miyu-admin authorization before invoking this.
    /// The store intentionally has no concept of QQ group-owner/admin roles.
    pub(crate) async fn delete_history(&self, mut request: DeleteRequest) -> Result<DeleteReport> {
        if matches!(request.mode, DeleteMode::KeepDays(0)) {
            bail!("keep_days must be a positive integer");
        }
        request.sender_id = request
            .sender_id
            .map(|value| validate_identifier("sender id", value))
            .transpose()?;
        if request
            .since
            .zip(request.until)
            .is_some_and(|(from, to)| from > to)
        {
            bail!("history deletion time range must have since <= until");
        }
        request.batch_size = request.batch_size.clamp(1, MAX_DELETE_BATCH_SIZE);
        self.request(|reply| Command::Delete { request, reply })
            .await
    }

    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T>>) -> Command,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        let actor = self.actor_sender()?;
        let (reply, receiver) = oneshot::channel();
        actor
            .send(build(reply))
            .await
            .map_err(|_| anyhow!("message history actor is unavailable"))?;
        receiver
            .await
            .map_err(|_| anyhow!("message history actor stopped before replying"))?
    }

    fn actor_sender(&self) -> Result<mpsc::Sender<Command>> {
        let mut guard = self
            .inner
            .actor
            .lock()
            .map_err(|_| anyhow!("message history actor lock was poisoned"))?;
        if let Some(sender) = guard.as_ref().filter(|sender| !sender.is_closed()) {
            return Ok(sender.clone());
        }

        let (sender, receiver) = mpsc::channel(self.inner.queue_capacity);
        let path = self.inner.db_path.clone();
        std::thread::Builder::new()
            .name("miyu-message-history".to_string())
            .spawn(move || actor_loop(path, receiver))
            .context("starting message history actor")?;
        *guard = Some(sender.clone());
        Ok(sender)
    }
}

enum Command {
    Record {
        messages: Vec<NewHistoryMessage>,
        reply: oneshot::Sender<Result<Vec<RecordOutcome>>>,
    },
    Recall {
        recall: NewRecall,
        reply: oneshot::Sender<Result<RecallOutcome>>,
    },
    ResetContext {
        group: GroupKey,
        persona_scope: String,
        reset_at: i64,
        reply: oneshot::Sender<Result<ContextBoundary>>,
    },
    GetBoundary {
        group: GroupKey,
        persona_scope: String,
        reply: oneshot::Sender<Result<Option<ContextBoundary>>>,
    },
    Recent {
        query: RecentQuery,
        reply: oneshot::Sender<Result<HistoryPage>>,
    },
    Search {
        query: SearchQuery,
        reply: oneshot::Sender<Result<HistoryPage>>,
    },
    ActivityRanking {
        query: ActivityRankingQuery,
        reply: oneshot::Sender<Result<ActivityRanking>>,
    },
    Delete {
        request: DeleteRequest,
        reply: oneshot::Sender<Result<DeleteReport>>,
    },
}

fn actor_loop(db_path: PathBuf, mut receiver: mpsc::Receiver<Command>) {
    let mut connection = None;
    while let Some(command) = receiver.blocking_recv() {
        match command {
            Command::Record { messages, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| insert_messages(conn, messages));
                let _ = reply.send(result);
            }
            Command::Recall { recall, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| insert_recall(conn, recall));
                let _ = reply.send(result);
            }
            Command::ResetContext {
                group,
                persona_scope,
                reset_at,
                reply,
            } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| upsert_boundary(conn, &group, &persona_scope, reset_at));
                let _ = reply.send(result);
            }
            Command::GetBoundary {
                group,
                persona_scope,
                reply,
            } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| read_boundary(conn, &group, &persona_scope));
                let _ = reply.send(result);
            }
            Command::Recent { query, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| query_recent(conn, query));
                let _ = reply.send(result);
            }
            Command::Search { query, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| query_search(conn, query));
                let _ = reply.send(result);
            }
            Command::ActivityRanking { query, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| query_activity_ranking(conn, query));
                let _ = reply.send(result);
            }
            Command::Delete { request, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| delete_history(conn, request));
                let _ = reply.send(result);
            }
        }
    }
}

fn actor_connection<'a>(
    connection: &'a mut Option<Connection>,
    db_path: &Path,
) -> Result<&'a mut Connection> {
    if connection.is_none() {
        *connection = Some(open_database(db_path)?);
    }
    connection
        .as_mut()
        .ok_or_else(|| anyhow!("message history connection was not initialized"))
}

fn open_database(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating message history directory: {}", parent.display()))?;
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening message history database: {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA auto_vacuum = INCREMENTAL;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA wal_autocheckpoint = 1000;
         PRAGMA cache_size = -4096;
         PRAGMA mmap_size = 0;",
    )?;
    migrate(&conn)?;
    // Version-1 databases may already contain a boundary left above the
    // largest surviving rowid by an older keep-days cleanup. Repair it every
    // time the lazy connection opens so existing installations recover
    // without requiring another destructive operation.
    clamp_boundaries_to_current_rowid(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        bail!("message history database schema {version} is newer than supported {SCHEMA_VERSION}");
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }

    conn.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS messages (
             id INTEGER PRIMARY KEY,
             platform TEXT NOT NULL,
             account_id TEXT NOT NULL,
             group_id TEXT NOT NULL,
             message_id TEXT NOT NULL,
             sender_id TEXT NOT NULL,
             sender_name TEXT NOT NULL,
             text TEXT NOT NULL,
             media_json TEXT NOT NULL,
             mentions_json TEXT NOT NULL,
             reply_to_message_id TEXT,
             is_bot INTEGER NOT NULL CHECK (is_bot IN (0, 1)),
             sent_at INTEGER NOT NULL,
             recalled_at INTEGER,
             recorded_at INTEGER NOT NULL DEFAULT (unixepoch()),
             UNIQUE (platform, account_id, group_id, message_id)
         );
         CREATE INDEX IF NOT EXISTS idx_messages_scope_time
             ON messages(platform, account_id, group_id, sent_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_messages_account_time
             ON messages(platform, account_id, sent_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_messages_scope_sender_time
             ON messages(platform, account_id, group_id, sender_id, sent_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_messages_account_sender_time
             ON messages(platform, account_id, sender_id, sent_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_messages_scope_reply
             ON messages(platform, account_id, group_id, reply_to_message_id)
             WHERE reply_to_message_id IS NOT NULL;

         CREATE TABLE IF NOT EXISTS recalls (
             id INTEGER PRIMARY KEY,
             platform TEXT NOT NULL,
             account_id TEXT NOT NULL,
             group_id TEXT NOT NULL,
             message_id TEXT NOT NULL,
             operator_id TEXT,
             recalled_at INTEGER NOT NULL,
             UNIQUE (platform, account_id, group_id, message_id)
         );
         CREATE INDEX IF NOT EXISTS idx_recalls_scope_time
             ON recalls(platform, account_id, group_id, recalled_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_recalls_account_time
             ON recalls(platform, account_id, recalled_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_recalls_scope_operator_time
             ON recalls(platform, account_id, group_id, operator_id, recalled_at DESC)
             WHERE operator_id IS NOT NULL;

         CREATE TABLE IF NOT EXISTS context_boundaries (
             platform TEXT NOT NULL,
             account_id TEXT NOT NULL,
             group_id TEXT NOT NULL,
             persona_scope TEXT NOT NULL DEFAULT 'default',
             after_row_id INTEGER NOT NULL,
             reset_at INTEGER NOT NULL,
             PRIMARY KEY (platform, account_id, group_id, persona_scope)
         ) WITHOUT ROWID;

         CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
             text,
             sender_name,
             content='messages',
             content_rowid='id',
             tokenize='trigram'
         );
         CREATE TRIGGER IF NOT EXISTS messages_fts_insert AFTER INSERT ON messages BEGIN
             INSERT INTO messages_fts(rowid, text, sender_name)
             VALUES (new.id, new.text, new.sender_name);
         END;
         CREATE TRIGGER IF NOT EXISTS messages_fts_delete AFTER DELETE ON messages BEGIN
             INSERT INTO messages_fts(messages_fts, rowid, text, sender_name)
             VALUES ('delete', old.id, old.text, old.sender_name);
         END;
         CREATE TRIGGER IF NOT EXISTS messages_fts_update
         AFTER UPDATE OF text, sender_name ON messages BEGIN
             INSERT INTO messages_fts(messages_fts, rowid, text, sender_name)
             VALUES ('delete', old.id, old.text, old.sender_name);
             INSERT INTO messages_fts(rowid, text, sender_name)
             VALUES (new.id, new.text, new.sender_name);
         END;
         PRAGMA user_version = 1;
         COMMIT;",
    )
    .context("creating message history schema")?;
    if version < 2 {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE messages ADD COLUMN ingress_order INTEGER;
             CREATE INDEX IF NOT EXISTS idx_messages_scope_ingress
                 ON messages(platform, account_id, group_id, ingress_order)
                 WHERE ingress_order IS NOT NULL;
             PRAGMA user_version = 2;
             COMMIT;",
        )
        .context("migrating message history schema to version 2")?;
    }
    if version < 3 {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE context_boundaries RENAME TO context_boundaries_v2;
             CREATE TABLE context_boundaries (
                 platform TEXT NOT NULL,
                 account_id TEXT NOT NULL,
                 group_id TEXT NOT NULL,
                 persona_scope TEXT NOT NULL,
                 after_row_id INTEGER NOT NULL,
                 reset_at INTEGER NOT NULL,
                 PRIMARY KEY (platform, account_id, group_id, persona_scope)
             ) WITHOUT ROWID;
             INSERT INTO context_boundaries (
                 platform, account_id, group_id, persona_scope, after_row_id, reset_at
             )
             SELECT platform, account_id, group_id, 'default', after_row_id, reset_at
             FROM context_boundaries_v2;
             DROP TABLE context_boundaries_v2;
             PRAGMA user_version = 3;
             COMMIT;",
        )
        .context("migrating message history schema to version 3")?;
    }
    if version < 4 {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             DROP TRIGGER IF EXISTS messages_fts_insert;
             DROP TRIGGER IF EXISTS messages_fts_delete;
             DROP TRIGGER IF EXISTS messages_fts_update;
             DROP TABLE IF EXISTS messages_fts;

             ALTER TABLE messages RENAME TO messages_v3;
             ALTER TABLE recalls RENAME TO recalls_v3;
             ALTER TABLE context_boundaries RENAME TO context_boundaries_v3;

             CREATE TABLE messages (
                 id INTEGER PRIMARY KEY,
                 platform TEXT NOT NULL,
                 account_id TEXT NOT NULL,
                 conversation_kind TEXT NOT NULL
                     CHECK (conversation_kind IN ('group', 'private')),
                 conversation_id TEXT NOT NULL,
                 message_id TEXT NOT NULL,
                 sender_id TEXT NOT NULL,
                 sender_name TEXT NOT NULL,
                 text TEXT NOT NULL,
                 media_json TEXT NOT NULL,
                 mentions_json TEXT NOT NULL,
                 reply_to_message_id TEXT,
                 is_bot INTEGER NOT NULL CHECK (is_bot IN (0, 1)),
                 sent_at INTEGER NOT NULL,
                 ingress_order INTEGER,
                 recalled_at INTEGER,
                 recorded_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 UNIQUE (
                     platform, account_id, conversation_kind, conversation_id, message_id
                 )
             );
             INSERT INTO messages (
                 id, platform, account_id, conversation_kind, conversation_id,
                 message_id, sender_id, sender_name, text, media_json, mentions_json,
                 reply_to_message_id, is_bot, sent_at, ingress_order, recalled_at,
                 recorded_at
             )
             SELECT id, platform, account_id, 'group', group_id, message_id, sender_id,
                    sender_name, text, media_json, mentions_json, reply_to_message_id,
                    is_bot, sent_at, ingress_order, recalled_at, recorded_at
             FROM messages_v3;

             CREATE TABLE recalls (
                 id INTEGER PRIMARY KEY,
                 platform TEXT NOT NULL,
                 account_id TEXT NOT NULL,
                 conversation_kind TEXT NOT NULL
                     CHECK (conversation_kind IN ('group', 'private')),
                 conversation_id TEXT NOT NULL,
                 message_id TEXT NOT NULL,
                 operator_id TEXT,
                 recalled_at INTEGER NOT NULL,
                 UNIQUE (
                     platform, account_id, conversation_kind, conversation_id, message_id
                 )
             );
             INSERT INTO recalls (
                 id, platform, account_id, conversation_kind, conversation_id,
                 message_id, operator_id, recalled_at
             )
             SELECT id, platform, account_id, 'group', group_id, message_id,
                    operator_id, recalled_at
             FROM recalls_v3;

             CREATE TABLE context_boundaries (
                 platform TEXT NOT NULL,
                 account_id TEXT NOT NULL,
                 conversation_kind TEXT NOT NULL
                     CHECK (conversation_kind IN ('group', 'private')),
                 conversation_id TEXT NOT NULL,
                 persona_scope TEXT NOT NULL,
                 after_row_id INTEGER NOT NULL,
                 reset_at INTEGER NOT NULL,
                 PRIMARY KEY (
                     platform, account_id, conversation_kind, conversation_id, persona_scope
                 )
             ) WITHOUT ROWID;
             INSERT INTO context_boundaries (
                 platform, account_id, conversation_kind, conversation_id,
                 persona_scope, after_row_id, reset_at
             )
             SELECT platform, account_id, 'group', group_id, persona_scope,
                    after_row_id, reset_at
             FROM context_boundaries_v3;

             DROP TABLE messages_v3;
             DROP TABLE recalls_v3;
             DROP TABLE context_boundaries_v3;

             CREATE INDEX idx_messages_scope_time
                 ON messages(
                     platform, account_id, conversation_kind, conversation_id,
                     sent_at DESC, id DESC
                 );
             CREATE INDEX idx_messages_account_time
                 ON messages(platform, account_id, sent_at DESC, id DESC);
             CREATE INDEX idx_messages_scope_sender_time
                 ON messages(
                     platform, account_id, conversation_kind, conversation_id,
                     sender_id, sent_at DESC, id DESC
                 );
             CREATE INDEX idx_messages_account_sender_time
                 ON messages(platform, account_id, sender_id, sent_at DESC, id DESC);
             CREATE INDEX idx_messages_scope_reply
                 ON messages(
                     platform, account_id, conversation_kind, conversation_id,
                     reply_to_message_id
                 )
                 WHERE reply_to_message_id IS NOT NULL;
             CREATE INDEX idx_messages_scope_ingress
                 ON messages(
                     platform, account_id, conversation_kind, conversation_id, ingress_order
                 )
                 WHERE ingress_order IS NOT NULL;

             CREATE INDEX idx_recalls_scope_time
                 ON recalls(
                     platform, account_id, conversation_kind, conversation_id,
                     recalled_at DESC, id DESC
                 );
             CREATE INDEX idx_recalls_account_time
                 ON recalls(platform, account_id, recalled_at DESC, id DESC);
             CREATE INDEX idx_recalls_scope_operator_time
                 ON recalls(
                     platform, account_id, conversation_kind, conversation_id,
                     operator_id, recalled_at DESC
                 )
                 WHERE operator_id IS NOT NULL;

             CREATE VIRTUAL TABLE messages_fts USING fts5(
                 text,
                 sender_name,
                 content='messages',
                 content_rowid='id',
                 tokenize='trigram'
             );
             INSERT INTO messages_fts(rowid, text, sender_name)
                 SELECT id, text, sender_name FROM messages;
             CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
                 INSERT INTO messages_fts(rowid, text, sender_name)
                 VALUES (new.id, new.text, new.sender_name);
             END;
             CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
                 INSERT INTO messages_fts(messages_fts, rowid, text, sender_name)
                 VALUES ('delete', old.id, old.text, old.sender_name);
             END;
             CREATE TRIGGER messages_fts_update
             AFTER UPDATE OF text, sender_name ON messages BEGIN
                 INSERT INTO messages_fts(messages_fts, rowid, text, sender_name)
                 VALUES ('delete', old.id, old.text, old.sender_name);
                 INSERT INTO messages_fts(rowid, text, sender_name)
                 VALUES (new.id, new.text, new.sender_name);
             END;
             PRAGMA user_version = 4;
             COMMIT;",
        )
        .context("migrating message history schema to version 4")?;
    }
    Ok(())
}

fn insert_messages(
    conn: &mut Connection,
    messages: Vec<NewHistoryMessage>,
) -> Result<Vec<RecordOutcome>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut outcomes = Vec::with_capacity(messages.len());
    for message in messages {
        let media_json = serde_json::to_string(&message.content.media)?;
        let mentions_json = if message.content.mentioned_users.is_empty() {
            serde_json::to_string(&message.content.mentioned_user_ids)?
        } else {
            serde_json::to_string(&message.content.mentioned_users)?
        };
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO messages (
                 platform, account_id, conversation_kind, conversation_id, message_id,
                 sender_id, sender_name, text, media_json, mentions_json,
                 reply_to_message_id, is_bot, sent_at, ingress_order, recalled_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 (SELECT recalled_at FROM recalls
                  WHERE platform = ?1 AND account_id = ?2
                    AND conversation_kind = ?3 AND conversation_id = ?4
                    AND message_id = ?5)
             )",
            params![
                message.group.platform,
                message.group.account_id,
                message.group.conversation_kind,
                message.group.conversation_id,
                message.message_id,
                message.sender_id,
                message.sender_name,
                message.content.text,
                media_json,
                mentions_json,
                message.reply_to_message_id,
                message.is_bot,
                message.sent_at,
                message.ingress_order,
            ],
        )? != 0;
        let row_id = tx.query_row(
            "SELECT id FROM messages
             WHERE platform = ?1 AND account_id = ?2
               AND conversation_kind = ?3 AND conversation_id = ?4 AND message_id = ?5",
            params![
                message.group.platform,
                message.group.account_id,
                message.group.conversation_kind,
                message.group.conversation_id,
                message.message_id,
            ],
            |row| row.get(0),
        )?;
        outcomes.push(RecordOutcome { row_id, inserted });
    }
    tx.commit()?;
    Ok(outcomes)
}

fn insert_recall(conn: &mut Connection, recall: NewRecall) -> Result<RecallOutcome> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existed: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM recalls
             WHERE platform = ?1 AND account_id = ?2
               AND conversation_kind = ?3 AND conversation_id = ?4 AND message_id = ?5
         )",
        params![
            recall.group.platform,
            recall.group.account_id,
            recall.group.conversation_kind,
            recall.group.conversation_id,
            recall.message_id,
        ],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO recalls (
             platform, account_id, conversation_kind, conversation_id,
             message_id, operator_id, recalled_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(
             platform, account_id, conversation_kind, conversation_id, message_id
         ) DO UPDATE SET
             operator_id = COALESCE(recalls.operator_id, excluded.operator_id),
             recalled_at = MIN(recalls.recalled_at, excluded.recalled_at)",
        params![
            recall.group.platform,
            recall.group.account_id,
            recall.group.conversation_kind,
            recall.group.conversation_id,
            recall.message_id,
            recall.operator_id,
            recall.recalled_at,
        ],
    )?;
    let matched_message = tx.execute(
        "UPDATE messages
         SET recalled_at = CASE
             WHEN recalled_at IS NULL THEN ?6
             ELSE MIN(recalled_at, ?6)
         END
         WHERE platform = ?1 AND account_id = ?2
           AND conversation_kind = ?3 AND conversation_id = ?4 AND message_id = ?5",
        params![
            recall.group.platform,
            recall.group.account_id,
            recall.group.conversation_kind,
            recall.group.conversation_id,
            recall.message_id,
            recall.recalled_at,
        ],
    )? != 0;
    tx.commit()?;
    Ok(RecallOutcome {
        newly_recorded: !existed,
        matched_message,
    })
}

fn upsert_boundary(
    conn: &Connection,
    group: &GroupKey,
    persona_scope: &str,
    reset_at: i64,
) -> Result<ContextBoundary> {
    let after_row_id = conn.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM messages
         WHERE platform = ?1 AND account_id = ?2
           AND conversation_kind = ?3 AND conversation_id = ?4",
        params![
            group.platform,
            group.account_id,
            group.conversation_kind,
            group.conversation_id
        ],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO context_boundaries (
             platform, account_id, conversation_kind, conversation_id,
             persona_scope, after_row_id, reset_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(
             platform, account_id, conversation_kind, conversation_id, persona_scope
         ) DO UPDATE SET
             after_row_id = excluded.after_row_id,
             reset_at = excluded.reset_at",
        params![
            group.platform,
            group.account_id,
            group.conversation_kind,
            group.conversation_id,
            persona_scope,
            after_row_id,
            reset_at,
        ],
    )?;
    Ok(ContextBoundary {
        after_row_id,
        reset_at,
    })
}

fn read_boundary(
    conn: &Connection,
    group: &GroupKey,
    persona_scope: &str,
) -> Result<Option<ContextBoundary>> {
    conn.query_row(
        "SELECT after_row_id, reset_at FROM context_boundaries
         WHERE platform = ?1 AND account_id = ?2
           AND conversation_kind = ?3 AND conversation_id = ?4 AND persona_scope = ?5",
        params![
            group.platform,
            group.account_id,
            group.conversation_kind,
            group.conversation_id,
            persona_scope
        ],
        |row| {
            Ok(ContextBoundary {
                after_row_id: row.get(0)?,
                reset_at: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

const MESSAGE_COLUMNS: &str = "m.id, m.platform, m.account_id, m.conversation_kind, \
    m.conversation_id, m.message_id, m.sender_id, m.sender_name, m.text, m.media_json, \
    m.mentions_json, m.reply_to_message_id, m.is_bot, m.sent_at, m.ingress_order, m.recalled_at";

fn query_recent(conn: &Connection, query: RecentQuery) -> Result<HistoryPage> {
    let page_size = page_size(query.limit);
    let fetch_size = page_size + 1;
    let before = query.before.unwrap_or(HistoryCursor {
        sent_at: i64::MAX,
        row_id: i64::MAX,
    });
    let boundary = if query.respect_context_boundary {
        read_boundary(conn, &query.group, &query.persona_scope)?
            .map(|boundary| boundary.after_row_id)
            .unwrap_or(0)
    } else {
        0
    };
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages AS m
         WHERE m.platform = ?1 AND m.account_id = ?2
           AND m.conversation_kind = ?3 AND m.conversation_id = ?4
           AND m.id > ?5
           AND (?6 OR m.recalled_at IS NULL)
           AND (m.sent_at < ?7 OR (m.sent_at = ?7 AND m.id < ?8))
           AND (?9 IS NULL OR m.ingress_order IS NULL OR m.ingress_order < ?9)
           AND (?11 IS NULL OR (m.ingress_order IS NOT NULL AND m.ingress_order > ?11))
          ORDER BY
            CASE WHEN ?9 IS NOT NULL AND m.ingress_order IS NOT NULL THEN 0 ELSE 1 END ASC,
            CASE WHEN ?9 IS NOT NULL THEN m.ingress_order END DESC,
            m.sent_at DESC,
            m.id DESC
         LIMIT ?10"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![
            query.group.platform,
            query.group.account_id,
            query.group.conversation_kind,
            query.group.conversation_id,
            boundary,
            query.include_recalled,
            before.sent_at,
            before.row_id,
            query.before_ingress_order,
            fetch_size as i64,
            query.after_ingress_order,
        ],
        map_message,
    )?;
    let mut messages = Vec::with_capacity(page_size.min(64));
    let mut has_more = false;
    for row in rows {
        let message = row?;
        if messages.len() == page_size {
            has_more = true;
            break;
        }
        messages.push(message);
    }
    let next_cursor = has_more.then(|| cursor_for(messages.last().expect("non-empty page")));
    messages.reverse();
    Ok(HistoryPage {
        messages,
        next_cursor,
    })
}

fn query_search(conn: &Connection, query: SearchQuery) -> Result<HistoryPage> {
    let terms = search_terms(&query.text)?;
    let page_size = page_size(query.limit);
    let fetch_size = page_size + 1;
    let before = query.before.unwrap_or(HistoryCursor {
        sent_at: i64::MAX,
        row_id: i64::MAX,
    });
    let use_fts = !terms.is_empty() && terms.iter().all(|term| term.chars().count() >= 3);
    let mut arguments = Vec::<SqlValue>::new();
    let mut conditions = Vec::<String>::new();
    let from = if use_fts {
        arguments.push(SqlValue::Text(build_fts_query(&terms)));
        conditions.push("messages_fts MATCH ?1".to_string());
        "messages_fts JOIN messages AS m ON m.id = messages_fts.rowid"
    } else {
        for term in &terms {
            arguments.push(SqlValue::Text(term.clone()));
            let parameter = arguments.len();
            conditions.push(format!(
                "(instr(lower(m.text), lower(?{parameter})) > 0 OR \
                 instr(lower(m.sender_name), lower(?{parameter})) > 0)"
            ));
        }
        "messages AS m"
    };

    match &query.scope {
        HistoryScope::Group(conversation) | HistoryScope::Private(conversation) => {
            arguments.push(SqlValue::Text(conversation.platform.clone()));
            let platform = arguments.len();
            arguments.push(SqlValue::Text(conversation.account_id.clone()));
            let account = arguments.len();
            arguments.push(SqlValue::Text(conversation.conversation_kind.clone()));
            let kind = arguments.len();
            arguments.push(SqlValue::Text(conversation.conversation_id.clone()));
            let conversation_id = arguments.len();
            conditions.push(format!(
                "m.platform = ?{platform} AND m.account_id = ?{account} \
                 AND m.conversation_kind = ?{kind} AND m.conversation_id = ?{conversation_id}"
            ));
        }
        HistoryScope::AllGroups(account) => {
            arguments.push(SqlValue::Text(account.platform.clone()));
            let platform = arguments.len();
            arguments.push(SqlValue::Text(account.account_id.clone()));
            let account = arguments.len();
            conditions.push(format!(
                "m.platform = ?{platform} AND m.account_id = ?{account} \
                 AND m.conversation_kind = 'group'"
            ));
        }
        HistoryScope::Account(account) => {
            arguments.push(SqlValue::Text(account.platform.clone()));
            let platform = arguments.len();
            arguments.push(SqlValue::Text(account.account_id.clone()));
            let account = arguments.len();
            conditions.push(format!(
                "m.platform = ?{platform} AND m.account_id = ?{account}"
            ));
        }
    }

    if let Some(sender_id) = query.sender_id {
        arguments.push(SqlValue::Text(sender_id));
        let sender = arguments.len();
        conditions.push(format!("m.sender_id = ?{sender}"));
    }
    arguments.push(SqlValue::Integer(i64::from(query.include_recalled)));
    let recalled = arguments.len();
    conditions.push(format!("(?{recalled} OR m.recalled_at IS NULL)"));
    arguments.push(SqlValue::Integer(i64::from(query.include_bot)));
    let bot = arguments.len();
    conditions.push(format!("(?{bot} OR NOT m.is_bot)"));
    arguments.push(query.since.map(SqlValue::Integer).unwrap_or(SqlValue::Null));
    let since = arguments.len();
    conditions.push(format!("(?{since} IS NULL OR m.sent_at >= ?{since})"));
    arguments.push(query.until.map(SqlValue::Integer).unwrap_or(SqlValue::Null));
    let until = arguments.len();
    conditions.push(format!("(?{until} IS NULL OR m.sent_at <= ?{until})"));
    arguments.push(SqlValue::Integer(before.sent_at));
    let before_at = arguments.len();
    arguments.push(SqlValue::Integer(before.row_id));
    let before_id = arguments.len();
    conditions.push(format!(
        "(m.sent_at < ?{before_at} OR (m.sent_at = ?{before_at} AND m.id < ?{before_id}))"
    ));
    arguments.push(SqlValue::Integer(fetch_size as i64));
    let limit = arguments.len();

    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM {from}
         WHERE {}
         ORDER BY m.sent_at DESC, m.id DESC
         LIMIT ?{limit}",
        conditions.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(arguments.iter()), map_message)?;
    let mut messages = Vec::with_capacity(page_size.min(64));
    let mut has_more = false;
    for row in rows {
        let message = row?;
        if messages.len() == page_size {
            has_more = true;
            break;
        }
        messages.push(message);
    }
    let next_cursor = has_more.then(|| cursor_for(messages.last().expect("non-empty page")));
    Ok(HistoryPage {
        messages,
        next_cursor,
    })
}

fn query_activity_ranking(
    conn: &Connection,
    query: ActivityRankingQuery,
) -> Result<ActivityRanking> {
    let mut stmt = conn.prepare(
        "WITH scoped AS (
             SELECT id,
                     CASE WHEN is_bot = 1 THEN ?2 ELSE sender_id END AS effective_sender_id,
                    sender_name,
                    sent_at
              FROM messages
              WHERE platform = ?1 AND account_id = ?2
                AND conversation_kind = ?3 AND conversation_id = ?4
                AND sent_at >= ?5 AND sent_at <= ?6
                AND (?7 OR is_bot = 0)
         ),
         named AS (
             SELECT effective_sender_id,
                    sender_name,
                    ROW_NUMBER() OVER (
                        PARTITION BY effective_sender_id
                        ORDER BY sent_at DESC, id DESC
                    ) AS name_rank
             FROM scoped
         ),
         aggregated AS (
             SELECT effective_sender_id,
                    COUNT(*) AS message_count,
                    COUNT(DISTINCT date(sent_at, 'unixepoch', 'localtime')) AS active_days,
                    MIN(sent_at) AS first_sent_at,
                    MAX(sent_at) AS last_sent_at
             FROM scoped
             GROUP BY effective_sender_id
         ),
         ranked AS (
             SELECT ROW_NUMBER() OVER (
                        ORDER BY aggregated.message_count DESC,
                                 aggregated.last_sent_at DESC,
                                 aggregated.effective_sender_id ASC
                    ) AS rank,
                    aggregated.effective_sender_id,
                    COALESCE(named.sender_name, aggregated.effective_sender_id) AS sender_name,
                    aggregated.message_count,
                    aggregated.active_days,
                    aggregated.first_sent_at,
                    aggregated.last_sent_at,
                    SUM(aggregated.message_count) OVER () AS total_messages,
                    COUNT(*) OVER () AS participant_count
             FROM aggregated
             LEFT JOIN named
               ON named.effective_sender_id = aggregated.effective_sender_id
              AND named.name_rank = 1
         )
         SELECT rank, effective_sender_id, sender_name, message_count, active_days,
                first_sent_at, last_sent_at, total_messages, participant_count
         FROM ranked
         ORDER BY rank
         LIMIT ?8",
    )?;
    let rows = stmt.query_map(
        params![
            query.group.platform,
            query.group.account_id,
            query.group.conversation_kind,
            query.group.conversation_id,
            query.since,
            query.until,
            query.include_bot,
            query.limit as i64,
        ],
        |row| {
            Ok((
                ActivityRankingItem {
                    rank: row.get(0)?,
                    sender_id: row.get(1)?,
                    sender_name: row.get(2)?,
                    message_count: row.get(3)?,
                    active_days: row.get(4)?,
                    first_sent_at: row.get(5)?,
                    last_sent_at: row.get(6)?,
                },
                row.get::<_, u64>(7)?,
                row.get::<_, u64>(8)?,
            ))
        },
    )?;
    let mut items = Vec::with_capacity(query.limit.min(32));
    let mut total_messages = 0;
    let mut participant_count = 0;
    for row in rows {
        let (item, total, participants) = row?;
        total_messages = total;
        participant_count = participants;
        items.push(item);
    }
    Ok(ActivityRanking {
        total_messages,
        participant_count,
        items,
    })
}

fn delete_history(conn: &mut Connection, request: DeleteRequest) -> Result<DeleteReport> {
    let cutoff = match request.mode {
        DeleteMode::All => None,
        DeleteMode::KeepDays(days) => Some(
            request
                .now
                .saturating_sub(i64::from(days).saturating_mul(SECONDS_PER_DAY)),
        ),
    };
    let batch_size = request.batch_size.clamp(1, MAX_DELETE_BATCH_SIZE);
    let mut report = DeleteReport::default();

    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = delete_message_batch(
            &tx,
            &request.scope,
            cutoff,
            request.sender_id.as_deref(),
            request.since,
            request.until,
            batch_size,
        )?;
        tx.commit()?;
        if deleted == 0 {
            break;
        }
        report.messages_deleted = report.messages_deleted.saturating_add(deleted as u64);
        report.batches = report.batches.saturating_add(1);
    }

    let delete_auxiliary =
        request.sender_id.is_none() && request.since.is_none() && request.until.is_none();
    if delete_auxiliary {
        loop {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let deleted = delete_recall_batch(&tx, &request.scope, cutoff, batch_size)?;
            tx.commit()?;
            if deleted == 0 {
                break;
            }
            report.recalls_deleted = report.recalls_deleted.saturating_add(deleted as u64);
            report.batches = report.batches.saturating_add(1);
        }

        let boundary_tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        report.boundaries_deleted = delete_boundaries(&boundary_tx, &request.scope, cutoff)? as u64;
        clamp_boundaries_to_current_rowid(&boundary_tx)?;
        boundary_tx.commit()?;
    }

    // Never run a full VACUUM in the daemon. Reclaim a bounded number of pages
    // after an explicit admin purge and let later purges continue the work.
    conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE); PRAGMA incremental_vacuum(256);")?;
    Ok(report)
}

fn delete_message_batch(
    tx: &Transaction<'_>,
    scope: &HistoryScope,
    cutoff: Option<i64>,
    sender_id: Option<&str>,
    since: Option<i64>,
    until: Option<i64>,
    batch_size: usize,
) -> Result<usize> {
    match scope {
        HistoryScope::Group(conversation) | HistoryScope::Private(conversation) => Ok(tx.execute(
            "DELETE FROM messages WHERE id IN (
                 SELECT id FROM messages
                 WHERE platform = ?1 AND account_id = ?2
                   AND conversation_kind = ?3 AND conversation_id = ?4
                   AND (?5 IS NULL OR sent_at < ?5)
                   AND (?6 IS NULL OR sender_id = ?6)
                   AND (?7 IS NULL OR sent_at >= ?7)
                   AND (?8 IS NULL OR sent_at <= ?8)
                 ORDER BY id LIMIT ?9
             )",
            params![
                conversation.platform,
                conversation.account_id,
                conversation.conversation_kind,
                conversation.conversation_id,
                cutoff,
                sender_id,
                since,
                until,
                batch_size as i64,
            ],
        )?),
        HistoryScope::AllGroups(account) => Ok(tx.execute(
            "DELETE FROM messages WHERE id IN (
                 SELECT id FROM messages
                 WHERE platform = ?1 AND account_id = ?2
                   AND conversation_kind = 'group'
                   AND (?3 IS NULL OR sent_at < ?3)
                   AND (?4 IS NULL OR sender_id = ?4)
                   AND (?5 IS NULL OR sent_at >= ?5)
                   AND (?6 IS NULL OR sent_at <= ?6)
                 ORDER BY id LIMIT ?7
             )",
            params![
                account.platform,
                account.account_id,
                cutoff,
                sender_id,
                since,
                until,
                batch_size as i64,
            ],
        )?),
        HistoryScope::Account(account) => Ok(tx.execute(
            "DELETE FROM messages WHERE id IN (
                 SELECT id FROM messages
                 WHERE platform = ?1 AND account_id = ?2
                   AND (?3 IS NULL OR sent_at < ?3)
                   AND (?4 IS NULL OR sender_id = ?4)
                   AND (?5 IS NULL OR sent_at >= ?5)
                   AND (?6 IS NULL OR sent_at <= ?6)
                 ORDER BY id LIMIT ?7
             )",
            params![
                account.platform,
                account.account_id,
                cutoff,
                sender_id,
                since,
                until,
                batch_size as i64,
            ],
        )?),
    }
}

fn delete_recall_batch(
    tx: &Transaction<'_>,
    scope: &HistoryScope,
    cutoff: Option<i64>,
    batch_size: usize,
) -> Result<usize> {
    match scope {
        HistoryScope::Group(conversation) | HistoryScope::Private(conversation) => Ok(tx.execute(
            "DELETE FROM recalls WHERE id IN (
                 SELECT r.id FROM recalls AS r
                 WHERE r.platform = ?1 AND r.account_id = ?2
                   AND r.conversation_kind = ?3 AND r.conversation_id = ?4
                   AND (?5 IS NULL OR (
                       r.recalled_at < ?5 AND NOT EXISTS (
                           SELECT 1 FROM messages AS m
                           WHERE m.platform = r.platform AND m.account_id = r.account_id
                             AND m.conversation_kind = r.conversation_kind
                             AND m.conversation_id = r.conversation_id
                             AND m.message_id = r.message_id
                       )
                   ))
                 ORDER BY r.id LIMIT ?6
             )",
            params![
                conversation.platform,
                conversation.account_id,
                conversation.conversation_kind,
                conversation.conversation_id,
                cutoff,
                batch_size as i64,
            ],
        )?),
        HistoryScope::AllGroups(account) => Ok(tx.execute(
            "DELETE FROM recalls WHERE id IN (
                 SELECT r.id FROM recalls AS r
                 WHERE r.platform = ?1 AND r.account_id = ?2
                   AND r.conversation_kind = 'group'
                   AND (?3 IS NULL OR (
                       r.recalled_at < ?3 AND NOT EXISTS (
                           SELECT 1 FROM messages AS m
                           WHERE m.platform = r.platform AND m.account_id = r.account_id
                             AND m.conversation_kind = r.conversation_kind
                             AND m.conversation_id = r.conversation_id
                             AND m.message_id = r.message_id
                       )
                   ))
                 ORDER BY r.id LIMIT ?4
             )",
            params![
                account.platform,
                account.account_id,
                cutoff,
                batch_size as i64,
            ],
        )?),
        HistoryScope::Account(account) => Ok(tx.execute(
            "DELETE FROM recalls WHERE id IN (
                 SELECT r.id FROM recalls AS r
                 WHERE r.platform = ?1 AND r.account_id = ?2
                   AND (?3 IS NULL OR (
                       r.recalled_at < ?3 AND NOT EXISTS (
                           SELECT 1 FROM messages AS m
                           WHERE m.platform = r.platform AND m.account_id = r.account_id
                             AND m.conversation_kind = r.conversation_kind
                             AND m.conversation_id = r.conversation_id
                             AND m.message_id = r.message_id
                       )
                   ))
                 ORDER BY r.id LIMIT ?4
             )",
            params![
                account.platform,
                account.account_id,
                cutoff,
                batch_size as i64,
            ],
        )?),
    }
}

fn delete_boundaries(
    tx: &Transaction<'_>,
    scope: &HistoryScope,
    cutoff: Option<i64>,
) -> Result<usize> {
    match scope {
        HistoryScope::Group(conversation) | HistoryScope::Private(conversation) => Ok(tx.execute(
            "DELETE FROM context_boundaries
             WHERE platform = ?1 AND account_id = ?2
               AND conversation_kind = ?3 AND conversation_id = ?4
               AND (?5 IS NULL OR reset_at < ?5)",
            params![
                conversation.platform,
                conversation.account_id,
                conversation.conversation_kind,
                conversation.conversation_id,
                cutoff
            ],
        )?),
        HistoryScope::AllGroups(account) => Ok(tx.execute(
            "DELETE FROM context_boundaries
             WHERE platform = ?1 AND account_id = ?2
               AND conversation_kind = 'group'
               AND (?3 IS NULL OR reset_at < ?3)",
            params![account.platform, account.account_id, cutoff],
        )?),
        HistoryScope::Account(account) => Ok(tx.execute(
            "DELETE FROM context_boundaries
              WHERE platform = ?1 AND account_id = ?2
               AND (?3 IS NULL OR reset_at < ?3)",
            params![account.platform, account.account_id, cutoff],
        )?),
    }
}

fn clamp_boundaries_to_current_rowid(conn: &Connection) -> Result<()> {
    // `INTEGER PRIMARY KEY` may reuse lower rowids after the highest messages
    // are deleted. A retained reset boundary must therefore never remain above
    // the current global maximum, or later messages could stay hidden until
    // their reused rowids eventually pass that stale boundary.
    let maximum_row_id: i64 =
        conn.query_row("SELECT COALESCE(MAX(id), 0) FROM messages", [], |row| {
            row.get(0)
        })?;
    conn.execute(
        "UPDATE context_boundaries
         SET after_row_id = ?1
         WHERE after_row_id > ?1",
        params![maximum_row_id],
    )?;
    Ok(())
}

fn map_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryMessage> {
    let media_json: String = row.get(9)?;
    let media = serde_json::from_str(&media_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let mentions_json: String = row.get(10)?;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StoredMentions {
        Users(Vec<PlatformMention>),
        Ids(Vec<String>),
    }
    let (mentioned_user_ids, mentioned_users) =
        match serde_json::from_str(&mentions_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })? {
            StoredMentions::Users(users) => (
                users
                    .iter()
                    .map(|mention| mention.user_id.clone())
                    .collect(),
                users,
            ),
            StoredMentions::Ids(ids) => (ids, Vec::new()),
        };
    Ok(HistoryMessage {
        row_id: row.get(0)?,
        group: GroupKey {
            platform: row.get(1)?,
            account_id: row.get(2)?,
            conversation_kind: row.get(3)?,
            conversation_id: row.get(4)?,
        },
        message_id: row.get(5)?,
        sender_id: row.get(6)?,
        sender_name: row.get(7)?,
        content: SanitizedContent {
            text: row.get(8)?,
            media,
            mentioned_user_ids,
            mentioned_users,
        },
        reply_to_message_id: row.get(11)?,
        is_bot: row.get(12)?,
        sent_at: row.get(13)?,
        ingress_order: row.get(14)?,
        recalled_at: row.get(15)?,
    })
}

fn cursor_for(message: &HistoryMessage) -> HistoryCursor {
    HistoryCursor {
        sent_at: message.sent_at,
        row_id: message.row_id,
    }
}

fn search_terms(text: &str) -> Result<Vec<String>> {
    let text = sanitize_multiline(text, MAX_SEARCH_BYTES);
    let terms = text
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .take(MAX_SEARCH_TERMS)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    Ok(terms)
}

fn build_fts_query(terms: &[String]) -> String {
    terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn page_size(requested: usize) -> usize {
    requested.clamp(1, MAX_PAGE_SIZE)
}

fn validate_identifier(label: &str, value: String) -> Result<String> {
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

fn sanitize_multiline(value: &str, max_bytes: usize) -> String {
    let filtered = value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .collect::<String>();
    truncate_utf8(filtered.trim(), max_bytes)
}

fn sanitize_single_line(value: &str, max_bytes: usize) -> String {
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

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn group(account: &str, group_id: &str) -> GroupKey {
        GroupKey::new("onebot", account, group_id).unwrap()
    }

    fn private(account: &str, user_id: &str) -> ConversationKey {
        ConversationKey::for_kind("onebot", account, ConversationKind::Private, user_id).unwrap()
    }

    fn message(
        group: GroupKey,
        message_id: impl Into<String>,
        sender_id: &str,
        sender_name: &str,
        text: impl Into<String>,
        sent_at: i64,
    ) -> NewHistoryMessage {
        NewHistoryMessage {
            group,
            message_id: message_id.into(),
            sender_id: sender_id.to_string(),
            sender_name: sender_name.to_string(),
            content: SanitizedContent::new(text, Vec::new()),
            reply_to_message_id: None,
            is_bot: false,
            sent_at,
            ingress_order: None,
        }
    }

    fn test_store() -> (TempDir, HistoryStore) {
        let temp = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(temp.path().join("nested/group_history.db"));
        (temp, store)
    }

    #[tokio::test]
    async fn database_is_lazy_and_uses_bounded_sqlite_settings() {
        let (_temp, store) = test_store();
        assert!(!store.db_path().exists());

        assert!(store
            .recent(RecentQuery::for_context(group("1", "10"), "default", 20))
            .await
            .unwrap()
            .messages
            .is_empty());
        assert!(store.db_path().exists());

        let conn = Connection::open(store.db_path()).unwrap();
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let auto_vacuum: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        assert_eq!(auto_vacuum, 2);
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn version_one_database_migrates_with_nullable_ingress_order() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE messages (
                 id INTEGER PRIMARY KEY,
                 platform TEXT NOT NULL,
                 account_id TEXT NOT NULL,
                 group_id TEXT NOT NULL,
                 message_id TEXT NOT NULL,
                 sender_id TEXT NOT NULL,
                 sender_name TEXT NOT NULL,
                 text TEXT NOT NULL,
                 media_json TEXT NOT NULL,
                 mentions_json TEXT NOT NULL,
                 reply_to_message_id TEXT,
                 is_bot INTEGER NOT NULL,
                 sent_at INTEGER NOT NULL,
                 recalled_at INTEGER,
                 recorded_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 UNIQUE (platform, account_id, group_id, message_id)
             );
             PRAGMA user_version = 1;",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let has_ingress_order = conn
            .prepare("PRAGMA table_info(messages)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .any(|column| column == "ingress_order");
        let has_conversation_kind = conn
            .prepare("PRAGMA table_info(messages)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .any(|column| column == "conversation_kind");
        assert_eq!(version, SCHEMA_VERSION);
        assert!(has_ingress_order);
        assert!(has_conversation_kind);
    }

    #[tokio::test]
    async fn private_and_group_conversations_are_isolated_and_filterable() {
        let (_temp, store) = test_store();
        let private_key = private("bot", "42");
        let group_key = group("bot", "42");
        store
            .record_message(message(
                private_key.clone(),
                "same-id",
                "42",
                "Alice",
                "private first",
                10,
            ))
            .await
            .unwrap();
        store
            .record_message(message(
                private_key.clone(),
                "private-2",
                "7",
                "Bob",
                "private second",
                20,
            ))
            .await
            .unwrap();
        store
            .record_message(message(
                group_key.clone(),
                "same-id",
                "42",
                "Alice",
                "group message",
                15,
            ))
            .await
            .unwrap();

        let private_page = store
            .search(SearchQuery::new(
                HistoryScope::Private(private_key.clone()),
                "private",
                20,
            ))
            .await
            .unwrap();
        assert_eq!(private_page.messages.len(), 2);
        assert!(private_page
            .messages
            .iter()
            .all(|message| message.group == private_key));

        let account_page = store
            .search(SearchQuery::new(
                HistoryScope::Account(private_key.account_scope()),
                "",
                20,
            ))
            .await
            .unwrap();
        assert_eq!(account_page.messages.len(), 3);
        assert!(account_page
            .messages
            .iter()
            .any(|message| message.group == group_key));
        assert!(account_page
            .messages
            .iter()
            .any(|message| message.group == private_key));

        let mut request = DeleteRequest::all(HistoryScope::Private(private_key.clone()), 30);
        request.sender_id = Some("42".to_string());
        request.since = Some(10);
        request.until = Some(10);
        let report = store.delete_history(request).await.unwrap();
        assert_eq!(report.messages_deleted, 1);
        let remaining_private = store
            .recent(RecentQuery::for_history(private_key, 20))
            .await
            .unwrap();
        assert_eq!(remaining_private.messages.len(), 1);
        assert_eq!(remaining_private.messages[0].message_id, "private-2");
        assert_eq!(
            store
                .recent(RecentQuery::for_history(group_key, 20))
                .await
                .unwrap()
                .messages
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn records_are_idempotent_isolated_and_sanitized() {
        let (_temp, store) = test_store();
        let first_group = group("bot-a", "group-1");
        let other_group = group("bot-a", "group-2");
        let other_account = group("bot-b", "group-1");
        let mut first = message(
            first_group.clone(),
            "m1",
            "u1",
            "Alice\nAdmin",
            " hello\0 world ",
            10,
        );
        first.content.media = vec![
            MediaPlaceholder::new(MediaKind::Image, Some(" cat\nphoto "), Some(" image/png ")),
            MediaPlaceholder::new(MediaKind::File, Some("notes.txt"), None::<String>),
        ];
        first.content.mentioned_user_ids = vec!["u2".to_string(), "u2".to_string()];
        first.content.mentioned_users = vec![PlatformMention {
            user_id: "u2".to_string(),
            display_name: Some("Yu\nyi".to_string()),
        }];

        let outcome = store.record_message(first.clone()).await.unwrap();
        assert!(outcome.inserted);
        let duplicate = store.record_message(first).await.unwrap();
        assert!(!duplicate.inserted);
        assert_eq!(outcome.row_id, duplicate.row_id);
        store
            .record_message(message(
                other_group.clone(),
                "m1",
                "u2",
                "Bob",
                "other group",
                11,
            ))
            .await
            .unwrap();
        store
            .record_message(message(
                other_account.clone(),
                "m1",
                "u3",
                "Carol",
                "other account",
                12,
            ))
            .await
            .unwrap();

        let page = store
            .recent(RecentQuery::for_history(first_group, 20))
            .await
            .unwrap();
        assert_eq!(page.messages.len(), 1);
        let stored = &page.messages[0];
        assert_eq!(stored.sender_name, "Alice Admin");
        assert_eq!(stored.content.text, "hello world");
        assert_eq!(stored.content.media[0].label.as_deref(), Some("cat photo"));
        assert_eq!(stored.content.media[0].mime.as_deref(), Some("image/png"));
        assert_eq!(stored.content.mentioned_user_ids, vec!["u2"]);
        assert_eq!(stored.content.mentioned_users[0].user_id, "u2");
        assert_eq!(
            stored.content.mentioned_users[0].display_name.as_deref(),
            Some("Yu yi")
        );
        assert_eq!(stored.group.group_id(), "group-1");
    }

    #[tokio::test]
    async fn the_reply_window_can_start_after_what_a_previous_turn_already_showed() {
        let (_temp, store) = test_store();
        let key = group("bot-a", "group-1");
        let mut first = message(key.clone(), "m1", "u1", "One", "已经发过", 10);
        first.ingress_order = Some(100);
        let mut second = message(key.clone(), "m2", "u2", "Two", "也发过", 10);
        second.ingress_order = Some(200);
        let mut third = message(key.clone(), "m3", "u3", "Three", "新到的", 10);
        third.ingress_order = Some(300);
        store
            .record_messages(vec![first, second, third])
            .await
            .unwrap();

        // Everything up to the watermark is already sitting in the replayed
        // conversation history, so the turn only carries what arrived since.
        let page = store
            .recent(
                RecentQuery::for_context(key.clone(), "default", 20).after_ingress_order(Some(200)),
            )
            .await
            .unwrap();
        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            ["m3"]
        );

        // No watermark yet — the first turn of a conversation still gets a full
        // opening snapshot.
        let page = store
            .recent(RecentQuery::for_context(key, "default", 20))
            .await
            .unwrap();
        assert_eq!(page.messages.len(), 3);
    }

    #[tokio::test]
    async fn context_ingress_boundary_excludes_current_and_future_messages() {
        let (_temp, store) = test_store();
        let key = group("bot-a", "group-1");
        let mut future = message(key.clone(), "future", "u3", "Future", "future", 10);
        future.ingress_order = Some(300);
        let mut previous = message(key.clone(), "previous", "u1", "Previous", "previous", 10);
        previous.ingress_order = Some(100);
        let mut current = message(key.clone(), "current", "u2", "Current", "current", 10);
        current.ingress_order = Some(200);

        // Deliberately persist in transport-opposite order to reproduce an
        // earlier message waiting on async metadata while a later one records.
        store
            .record_messages(vec![future, previous, current])
            .await
            .unwrap();

        let page = store
            .recent(RecentQuery::for_context(key, "default", 20).before_ingress_order(Some(200)))
            .await
            .unwrap();
        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["previous"]
        );
    }

    #[tokio::test]
    async fn context_history_is_ordered_by_transport_ingress() {
        let (_temp, store) = test_store();
        let key = group("bot-a", "group-1");
        let mut first = message(key.clone(), "first", "u1", "First", "first", 30);
        first.ingress_order = Some(100);
        let mut second = message(key.clone(), "second", "u2", "Second", "second", 10);
        second.ingress_order = Some(200);
        let mut third = message(key.clone(), "third", "u3", "Third", "third", 20);
        third.ingress_order = Some(300);

        store
            .record_messages(vec![third, first, second])
            .await
            .unwrap();

        let page = store
            .recent(RecentQuery::for_context(key, "default", 20).before_ingress_order(Some(400)))
            .await
            .unwrap();
        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );
    }

    #[tokio::test]
    async fn reset_boundary_only_changes_automatic_context() {
        let (_temp, store) = test_store();
        let key = group("bot", "group");
        store
            .record_messages(vec![
                message(key.clone(), "m1", "u", "A", "before one", 10),
                message(key.clone(), "m2", "u", "A", "before two", 20),
            ])
            .await
            .unwrap();
        let boundary = store
            .reset_context(key.clone(), "default".to_string(), 25)
            .await
            .unwrap();
        assert_eq!(boundary.after_row_id, 2);
        store
            .record_message(message(key.clone(), "m3", "u", "A", "after reset", 30))
            .await
            .unwrap();

        let context = store
            .recent(RecentQuery::for_context(key.clone(), "default", 20))
            .await
            .unwrap();
        assert_eq!(
            context
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["m3"]
        );
        let history = store
            .recent(RecentQuery::for_history(key, 20))
            .await
            .unwrap();
        assert_eq!(history.messages.len(), 3);
        let other_persona = store
            .recent(RecentQuery::for_context(group("bot", "group"), "other", 20))
            .await
            .unwrap();
        assert_eq!(other_persona.messages.len(), 3);
    }

    #[tokio::test]
    async fn recall_before_or_after_message_is_applied_and_hidden() {
        let (_temp, store) = test_store();
        let key = group("bot", "group");
        let early = store
            .record_recall(NewRecall {
                group: key.clone(),
                message_id: "early".to_string(),
                operator_id: Some("moderator".to_string()),
                recalled_at: 12,
            })
            .await
            .unwrap();
        assert!(early.newly_recorded);
        assert!(!early.matched_message);
        store
            .record_messages(vec![
                message(key.clone(), "early", "u1", "A", "hidden early", 10),
                message(key.clone(), "late", "u2", "B", "hidden late", 20),
                message(key.clone(), "visible", "u3", "C", "visible", 30),
            ])
            .await
            .unwrap();
        let late = store
            .record_recall(NewRecall {
                group: key.clone(),
                message_id: "late".to_string(),
                operator_id: None,
                recalled_at: 22,
            })
            .await
            .unwrap();
        assert!(late.matched_message);

        let visible = store
            .recent(RecentQuery::for_history(key.clone(), 20))
            .await
            .unwrap();
        assert_eq!(visible.messages.len(), 1);
        assert_eq!(visible.messages[0].message_id, "visible");

        let mut with_recalls = RecentQuery::for_history(key, 20);
        with_recalls.include_recalled = true;
        let page = store.recent(with_recalls).await.unwrap();
        assert_eq!(page.messages.len(), 3);
        assert_eq!(page.messages[0].recalled_at, Some(12));
        assert_eq!(page.messages[1].recalled_at, Some(22));
    }

    #[tokio::test]
    async fn activity_ranking_is_scoped_stable_and_counts_recalled_messages() {
        let (_temp, store) = test_store();
        let key = group("bot-a", "group-1");
        let other_group = group("bot-a", "group-2");
        let other_account = group("bot-b", "group-1");
        let first_day = SECONDS_PER_DAY * 10 + 43_200;
        let second_day = first_day + SECONDS_PER_DAY * 2;
        let mut bot_one = message(
            key.clone(),
            "bot-1",
            "bot-alias-1",
            "Miyu old",
            "bot",
            second_day + 20,
        );
        bot_one.is_bot = true;
        let mut bot_two = message(
            key.clone(),
            "bot-2",
            "bot-alias-2",
            "Miyu",
            "bot",
            second_day + 30,
        );
        bot_two.is_bot = true;
        store
            .record_messages(vec![
                message(key.clone(), "a-1", "1", "Alice old", "one", first_day),
                message(key.clone(), "a-2", "1", "Alice", "two", second_day + 10),
                message(
                    key.clone(),
                    "a-3",
                    "1",
                    "Alice newest",
                    "three",
                    second_day + 40,
                ),
                message(key.clone(), "b-1", "2", "Bob", "one", first_day + 10),
                message(key.clone(), "b-2", "2", "Bob", "two", second_day + 20),
                bot_one,
                bot_two,
                message(
                    other_group,
                    "other-group",
                    "3",
                    "Other",
                    "ignored",
                    second_day,
                ),
                message(
                    other_account,
                    "other-account",
                    "4",
                    "Other",
                    "ignored",
                    second_day,
                ),
            ])
            .await
            .unwrap();
        store
            .record_recall(NewRecall {
                group: key.clone(),
                message_id: "a-1".to_string(),
                operator_id: Some("1".to_string()),
                recalled_at: first_day + 100,
            })
            .await
            .unwrap();

        let ranking = store
            .activity_ranking(ActivityRankingQuery {
                group: key.clone(),
                since: first_day,
                until: second_day + 100,
                limit: 2,
                include_bot: true,
            })
            .await
            .unwrap();
        assert_eq!(ranking.total_messages, 7);
        assert_eq!(ranking.participant_count, 3);
        assert_eq!(ranking.items.len(), 2);
        assert_eq!(ranking.items[0].sender_id, "1");
        assert_eq!(ranking.items[0].sender_name, "Alice newest");
        assert_eq!(ranking.items[0].message_count, 3);
        assert_eq!(ranking.items[0].active_days, 2);
        assert_eq!(ranking.items[1].sender_id, "bot-a");
        assert_eq!(ranking.items[1].sender_name, "Miyu");
        assert_eq!(ranking.items[1].rank, 2);

        let without_bot = store
            .activity_ranking(ActivityRankingQuery {
                group: key,
                since: first_day,
                until: second_day + 100,
                limit: usize::MAX,
                include_bot: false,
            })
            .await
            .unwrap();
        assert_eq!(without_bot.total_messages, 5);
        assert_eq!(without_bot.participant_count, 2);
        assert_eq!(without_bot.items.len(), 2);
        assert_eq!(without_bot.items[1].sender_id, "2");
    }

    #[tokio::test]
    async fn activity_ranking_validates_time_range_and_includes_both_boundaries() {
        let (_temp, store) = test_store();
        let key = group("bot", "group");
        store
            .record_messages(vec![
                message(key.clone(), "before", "1", "A", "before", 9),
                message(key.clone(), "start", "1", "A", "start", 10),
                message(key.clone(), "end", "2", "B", "end", 20),
                message(key.clone(), "after", "2", "B", "after", 21),
            ])
            .await
            .unwrap();

        let result = store
            .activity_ranking(ActivityRankingQuery {
                group: key.clone(),
                since: 10,
                until: 20,
                limit: 20,
                include_bot: true,
            })
            .await
            .unwrap();
        assert_eq!(result.total_messages, 2);
        assert_eq!(result.participant_count, 2);
        assert!(store
            .activity_ranking(ActivityRankingQuery {
                group: key,
                since: 20,
                until: 10,
                limit: 20,
                include_bot: true,
            })
            .await
            .is_err());
    }

    #[tokio::test]
    async fn fts_search_is_safe_paginated_and_capped_at_one_thousand() {
        let (_temp, store) = test_store();
        let key = group("bot", "group");
        for batch_start in (0..1_005).step_by(MAX_BATCH_MESSAGES) {
            let end = (batch_start + MAX_BATCH_MESSAGES).min(1_005);
            let batch = (batch_start..end)
                .map(|index| {
                    message(
                        key.clone(),
                        format!("m{index}"),
                        "u",
                        "Search User",
                        format!("needle item {index}"),
                        index as i64,
                    )
                })
                .collect();
            store.record_messages(batch).await.unwrap();
        }
        store
            .record_message(message(
                key.clone(),
                "chinese",
                "u",
                "中文用户",
                "今天天气很好",
                1_000,
            ))
            .await
            .unwrap();

        let first = store
            .search(SearchQuery::new(
                HistoryScope::Group(key.clone()),
                "needle",
                usize::MAX,
            ))
            .await
            .unwrap();
        assert_eq!(first.messages.len(), MAX_PAGE_SIZE);
        assert!(first.next_cursor.is_some());
        let mut second_query =
            SearchQuery::new(HistoryScope::Group(key.clone()), "needle", MAX_PAGE_SIZE);
        second_query.before = first.next_cursor;
        let second = store.search(second_query).await.unwrap();
        assert_eq!(second.messages.len(), 5);
        assert!(second.next_cursor.is_none());

        let quoted = store
            .search(SearchQuery::new(
                HistoryScope::Group(key.clone()),
                "needle \"item\"",
                10,
            ))
            .await;
        assert!(quoted.is_ok());

        let chinese_trigram = store
            .search(SearchQuery::new(
                HistoryScope::Group(key.clone()),
                "天气很",
                10,
            ))
            .await
            .unwrap();
        assert_eq!(chinese_trigram.messages[0].message_id, "chinese");
        let chinese_short_fallback = store
            .search(SearchQuery::new(HistoryScope::Group(key), "天气", 10))
            .await
            .unwrap();
        assert_eq!(chinese_short_fallback.messages[0].message_id, "chinese");
    }

    #[tokio::test]
    async fn search_can_filter_recent_messages_by_sender_id() {
        let (_temp, store) = test_store();
        let key = group("bot", "group");
        store
            .record_messages(vec![
                message(key.clone(), "a1", "10001", "A", "first", 1),
                message(key.clone(), "b1", "10002", "B", "other", 2),
                message(key.clone(), "a2", "10001", "A", "second", 3),
                message(key.clone(), "a3", "10001", "A", "third", 4),
            ])
            .await
            .unwrap();

        let mut query = SearchQuery::new(HistoryScope::Group(key), "", 10);
        query.sender_id = Some("10001".to_string());
        let page = store.search(query).await.unwrap();

        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a3", "a2", "a1"]
        );
    }

    #[tokio::test]
    async fn history_pages_are_limited_by_message_count_only() {
        let (_temp, store) = test_store();
        let key = group("bot", "group");
        let large_text = format!("needle {}", "x".repeat(60 * 1024));
        let messages = (0..10)
            .map(|index| {
                message(
                    key.clone(),
                    format!("large-{index}"),
                    "u",
                    "Search User",
                    large_text.clone(),
                    index,
                )
            })
            .collect();
        store.record_messages(messages).await.unwrap();

        let page = store
            .search(SearchQuery::new(
                HistoryScope::Group(key),
                "needle",
                MAX_PAGE_SIZE,
            ))
            .await
            .unwrap();
        assert_eq!(page.messages.len(), 10);
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn explicit_deletion_is_batched_and_does_not_cross_scope() {
        let (_temp, store) = test_store();
        let first = group("bot", "first");
        let second = group("bot", "second");
        let other_account = group("other-bot", "first");
        let day = SECONDS_PER_DAY;
        store
            .record_messages(vec![
                message(first.clone(), "old1", "u", "A", "old one", day),
                message(first.clone(), "old2", "u", "A", "old two", day * 2),
                message(first.clone(), "new", "u", "A", "new", day * 9),
                message(second.clone(), "same-account", "u", "A", "keep", day),
                message(
                    other_account.clone(),
                    "other-account",
                    "u",
                    "A",
                    "keep",
                    day,
                ),
            ])
            .await
            .unwrap();
        store
            .reset_context(first.clone(), "default".to_string(), day * 2)
            .await
            .unwrap();
        store
            .record_recall(NewRecall {
                group: first.clone(),
                message_id: "old1".to_string(),
                operator_id: None,
                recalled_at: day * 2,
            })
            .await
            .unwrap();

        let mut request =
            DeleteRequest::keep_days(HistoryScope::Group(first.clone()), 3, day * 10).unwrap();
        request.batch_size = 1;
        let report = store.delete_history(request).await.unwrap();
        assert_eq!(report.messages_deleted, 2);
        assert_eq!(report.recalls_deleted, 1);
        assert_eq!(report.boundaries_deleted, 1);
        assert!(report.batches >= 3);

        let first_page = store
            .recent(RecentQuery::for_history(first.clone(), 20))
            .await
            .unwrap();
        assert_eq!(first_page.messages.len(), 1);
        assert_eq!(first_page.messages[0].message_id, "new");
        assert!(store
            .context_boundary(first.clone(), "default".to_string())
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .recent(RecentQuery::for_history(second.clone(), 20))
                .await
                .unwrap()
                .messages
                .len(),
            1
        );
        assert_eq!(
            store
                .recent(RecentQuery::for_history(other_account.clone(), 20))
                .await
                .unwrap()
                .messages
                .len(),
            1
        );

        let all = store
            .delete_history(DeleteRequest::all(
                HistoryScope::Group(first.clone()),
                day * 10,
            ))
            .await
            .unwrap();
        assert_eq!(all.messages_deleted, 1);
        assert!(store
            .recent(RecentQuery::for_history(first, 20))
            .await
            .unwrap()
            .messages
            .is_empty());

        let account_scope = HistoryScope::Account(second.account_scope());
        let account_search = store
            .search(SearchQuery::new(account_scope.clone(), "keep", 20))
            .await
            .unwrap();
        assert_eq!(account_search.messages.len(), 1);
        assert_eq!(account_search.messages[0].group, second);
        let account_report = store
            .delete_history(DeleteRequest::all(account_scope, day * 10))
            .await
            .unwrap();
        assert_eq!(account_report.messages_deleted, 1);
        assert_eq!(
            store
                .recent(RecentQuery::for_history(other_account, 20))
                .await
                .unwrap()
                .messages
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn retained_reset_boundary_does_not_hide_reused_rowids_after_cleanup() {
        let (_temp, store) = test_store();
        let key = group("bot", "group");
        let day = SECONDS_PER_DAY;

        store
            .record_message(message(key.clone(), "before-reset", "u", "A", "old", day))
            .await
            .unwrap();
        let boundary = store
            .reset_context(key.clone(), "default".to_string(), day * 10)
            .await
            .unwrap();
        assert_eq!(boundary.after_row_id, 1);

        // The message is outside the retention window, while the reset itself
        // is recent enough to remain. Deleting the sole message lets SQLite
        // reuse rowid 1 for the next insert.
        store
            .delete_history(
                DeleteRequest::keep_days(HistoryScope::Group(key.clone()), 3, day * 10).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .context_boundary(key.clone(), "default".to_string())
                .await
                .unwrap()
                .unwrap()
                .after_row_id,
            0
        );

        let inserted = store
            .record_message(message(
                key.clone(),
                "after-cleanup",
                "u",
                "A",
                "new",
                day * 10,
            ))
            .await
            .unwrap();
        assert_eq!(inserted.row_id, 1);
        let context = store
            .recent(RecentQuery::for_context(key, "default", 20))
            .await
            .unwrap();
        assert_eq!(
            context
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["after-cleanup"]
        );
    }

    #[test]
    fn opening_an_existing_database_repairs_a_stale_reset_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.db");
        let key = group("bot", "group");
        {
            let conn = open_database(&path).unwrap();
            conn.execute(
                "INSERT INTO context_boundaries (
                     platform, account_id, conversation_kind, conversation_id,
                     persona_scope, after_row_id, reset_at
                 ) VALUES (?1, ?2, ?3, ?4, 'default', 99, 123)",
                params![
                    key.platform(),
                    key.account_id(),
                    key.conversation_kind(),
                    key.conversation_id()
                ],
            )
            .unwrap();
        }

        let conn = open_database(&path).unwrap();
        assert_eq!(
            read_boundary(&conn, &key, "default")
                .unwrap()
                .unwrap()
                .after_row_id,
            0
        );
    }

    #[test]
    fn identifiers_and_keep_days_are_validated() {
        assert!(GroupKey::new("onebot", "", "group").is_err());
        assert!(GroupKey::new("onebot", "bot", "bad\ngroup").is_err());
        let scope = HistoryScope::Account(AccountKey::new("onebot", "bot").unwrap());
        assert!(DeleteRequest::keep_days(scope, 0, 0).is_err());
    }
}
