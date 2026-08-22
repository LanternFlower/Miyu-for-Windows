//! 写入与删除。
//!
//! 写走单线程 actor（见 [`super`] 的 `actor_loop`）：SQLite 的并发写要么锁要么
//! 出错，串行化比到处 retry 简单也快。
//!
//! 删除分批（`MAX_DELETE_BATCH_SIZE`）而不是一条 DELETE 干完：一次删几十万行会
//! 把库锁住足够久，久到平台那边的消息全部堆积。

use crate::platforms::plugins::message_history::store::*;

pub(crate) const MAX_BATCH_MESSAGES: usize = 256;

pub(crate) const DEFAULT_DELETE_BATCH_SIZE: usize = 1_000;

pub(crate) const MAX_DELETE_BATCH_SIZE: usize = 5_000;

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

pub(crate) fn insert_messages(
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

pub(crate) fn insert_recall(conn: &mut Connection, recall: NewRecall) -> Result<RecallOutcome> {
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

pub(crate) fn upsert_boundary(
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

pub(crate) fn read_boundary(
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

pub(crate) fn delete_history(conn: &mut Connection, request: DeleteRequest) -> Result<DeleteReport> {
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

pub(crate) fn delete_message_batch(
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

pub(crate) fn delete_recall_batch(
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

pub(crate) fn delete_boundaries(
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

pub(crate) fn clamp_boundaries_to_current_rowid(conn: &Connection) -> Result<()> {
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
