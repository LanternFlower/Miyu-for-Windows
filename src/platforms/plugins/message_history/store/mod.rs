mod query;
mod schema;
mod types;
mod write;
pub(crate) use query::*;
pub(crate) use schema::*;
pub(crate) use types::*;
pub(crate) use write::*;

use crate::platforms::{ConversationKind, PlatformMention};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

const DEFAULT_QUEUE_CAPACITY: usize = 128;
const MAX_QUEUE_CAPACITY: usize = 4_096;

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

#[cfg(test)]
mod tests;
