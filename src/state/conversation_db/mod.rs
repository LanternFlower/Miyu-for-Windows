mod attachments;
mod goals;
pub use goals::*;
mod history;
mod platform;
mod queue;
mod rows;
pub(crate) use rows::*;
pub use rows::{interrupted_text, pending_placeholder};
mod sessions;
mod shared_files;
pub use shared_files::SharedFile;
mod turns;
mod types;
pub use types::*;

use crate::i18n::text as t;
use crate::llm::{ChatMessage, TurnTokens};
use crate::memory_types::EvictedTurn;
use crate::question::QuestionExchange;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

fn insert_platform_access_audit(
    tx: &Transaction<'_>,
    operation: &str,
    key: &PlatformAccessGrantKey,
    actor: &PlatformAccessActor,
    created_at: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO platform_access_audit (
             audit_id, operation, platform, account_scope, permission,
             subject_kind, subject_id, actor_platform, actor_account_id,
             actor_user_id, actor_conversation_kind, actor_conversation_id,
             actor_message_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            format!("access-audit-{:032x}", rand::random::<u128>()),
            operation,
            key.platform,
            key.account_scope,
            key.permission,
            key.subject_kind,
            key.subject_id,
            actor.platform,
            actor.account_id,
            actor.user_id,
            actor.conversation_kind,
            actor.conversation_id,
            actor.message_id,
            created_at,
        ],
    )?;
    Ok(())
}

pub struct ConversationDb {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for ConversationDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationDb").finish_non_exhaustive()
    }
}

/// 把删掉的行占的页真正还给磁盘。
///
/// SQLite 删行只是把页挂进 freelist，文件本身**不会变小**，那些页也不会还给
/// 操作系统。本机实测：conversation.db 76 MB，其中 **90%（67.9 MB）是空闲页**，
/// 存活数据只有 7.4 MB——清理过的旧会话、旧图片全躺在那儿占着地方。
///
/// 同仓库的 message_history 库一直是对的（`auto_vacuum = INCREMENTAL` +
/// 每次清理跑一段有界的 `incremental_vacuum`），只有这个库漏了。
///
/// 两条路：
///
/// - **老库**（`auto_vacuum` 读出来是 0）：`open` 里那句 PRAGMA 对已有数据的
///   库是空转——实测设完立刻读回来还是 0，必须**紧跟一次完整 VACUUM** 才落地。
///   VACUUM 的代价只随存活数据长（实测约 2.6 ms/MB：存活 5.6 MB→15 ms、
///   22.5 MB→58 ms、67.5 MB→175 ms），跟文件多大无关。而且只会发生一次：
///   转换完 `auto_vacuum` 就是 2 了。
/// - **已转换的库**：只回收有界的一小段，照抄 message_history 的 256 页
///   （4 KB 页面下是 1 MB）。不在启动路径上跑完整 VACUUM。
///
/// 全程 `let _ =`：回收空间失败不该让人打不开自己的会话库。
fn reclaim_free_pages(conn: &Connection) {
    let mode: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .unwrap_or(0);
    if mode == 0 {
        let _ = conn.execute_batch("VACUUM;");
        return;
    }
    let _ = conn.execute_batch("PRAGMA incremental_vacuum(256);");
}

impl ConversationDb {
    pub fn open(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        let db_path = state_dir.join("conversation.db");
        let mut conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open conversation db: {}", db_path.display()))?;
        conn.execute_batch(
            // auto_vacuum 必须在建表**之前**设，新库才认。老库这句是空转，
            // 由下面的 `reclaim_free_pages` 补一次 VACUUM 来落地。
            // cache_size：本库存活数据只有个位数 MB、典型查询 1-2 ms，
            // 默认 2 MB 页缓存对它是浪费，1 MB 足够且无感。
            "PRAGMA auto_vacuum = INCREMENTAL;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;
             PRAGMA cache_size = -1024;",
        )?;
        // Back up the database file before applying schema migrations to a
        // database that already holds data.
        if super::migrations::current_version(&conn)? < super::migrations::LATEST_VERSION {
            let has_turns: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='turns')",
                [],
                |row| row.get(0),
            )?;
            if has_turns {
                let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
                let _ = std::fs::copy(&db_path, state_dir.join("conversation.db.bak"));
            }
        }
        super::migrations::run_migrations(&mut conn)?;
        reclaim_free_pages(&conn);
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn next_seq_locked(&self, conn: &Connection, session_id: &str) -> Result<i64> {
        let next_seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(next_seq)
    }
}

fn delete_visible_turns_in_transaction(
    tx: &Transaction<'_>,
    session_id: &str,
    turn_ids: &[String],
) -> Result<usize> {
    let mut affected = 0usize;
    for turn_id in turn_ids {
        let deleted = tx.execute(
            "DELETE FROM turns
             WHERE turn_id = ?1 AND session_id = ?2 AND hidden = 0 AND is_summary = 0
               AND status != 'running'",
            params![turn_id, session_id],
        )?;
        if deleted != 1 {
            bail!(
                "{}",
                t(
                    "conversation changed before popped turns could be deleted",
                    "删除弹出轮次前会话已发生变化"
                )
            );
        }
        tx.execute(
            "DELETE FROM session_loaded_items
             WHERE session_id = ?1 AND source_turn_id = ?2",
            params![session_id, turn_id],
        )?;
        affected += deleted;
    }
    Ok(affected)
}

fn verify_loaded_tool_sources(
    tx: &Transaction<'_>,
    session_id: &str,
    expected: Option<&[(String, Option<String>)]>,
) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let current = {
        let mut stmt = tx.prepare(
            "SELECT name, source_turn_id FROM session_loaded_items
             WHERE session_id = ?1 AND kind = 'tool' ORDER BY name ASC",
        )?;
        let rows = stmt
            .query_map(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<(String, Option<String>)>, _>>()?;
        rows
    };
    if current != expected {
        bail!(
            "{}",
            t(
                "dynamic tool state changed while popping context",
                "弹出上下文时动态工具状态已发生变化"
            )
        );
    }
    Ok(())
}

#[allow(dead_code)]
fn turn_chars(turn: &Turn) -> usize {
    turn.user_content.chars().count()
        + turn.assistant_content.chars().count()
        + turn
            .assistant_reasoning
            .as_deref()
            .map(str::chars)
            .map(Iterator::count)
            .unwrap_or(0)
        + turn
            .tool_reports
            .iter()
            .map(|r| r.chars().count())
            .sum::<usize>()
        + turn
            .question_exchanges
            .iter()
            .filter_map(|exchange| serde_json::to_string(exchange).ok())
            .map(|exchange| exchange.chars().count())
            .sum::<usize>()
        + turn
            .followups
            .iter()
            .map(|followup| {
                followup.content.chars().count()
                    + followup
                        .preceding_assistant_content
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
                    + followup
                        .preceding_assistant_reasoning
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
            })
            .sum::<usize>()
}

fn load_redo_checkpoint_locked(
    conn: &Connection,
    turn_id: &str,
) -> Result<Option<TurnRedoCheckpoint>> {
    conn.query_row(
        "SELECT version, batch_prompt_ids, payload, unavailable_reason
         FROM turn_redo_checkpoints WHERE turn_id = ?1",
        params![turn_id],
        |row| {
            let version = row.get::<_, i64>(0)?;
            let batch_prompt_ids =
                serde_json::from_str::<Vec<String>>(&row.get::<_, String>(1)?).unwrap_or_default();
            let payload = row
                .get::<_, Option<Vec<u8>>>(2)?
                .and_then(|payload| serde_json::from_slice(&payload).ok());
            let unavailable_reason = if version == REDO_CHECKPOINT_VERSION {
                row.get(3)?
            } else {
                Some(format!("unsupported redo checkpoint version: {version}"))
            };
            Ok(TurnRedoCheckpoint {
                batch_prompt_ids,
                payload: (version == REDO_CHECKPOINT_VERSION)
                    .then_some(payload)
                    .flatten(),
                unavailable_reason,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn consume_stale_queued_prompts_locked(
    tx: &Transaction<'_>,
    turn_id: &str,
    revision: i64,
    queue_session_id: Option<&str>,
    now: &str,
) -> Result<usize> {
    let Some(queue_session_id) = queue_session_id else {
        return Ok(0);
    };
    let prompts = {
        let mut stmt = tx.prepare(
            "SELECT prompt_id, content FROM queued_prompts
             WHERE status = 'queued' AND queue_session_id = ?1
             ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(params![queue_session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    if prompts.is_empty() {
        return Ok(0);
    }

    tx.execute(
        "INSERT OR IGNORE INTO turn_journal_segments
            (turn_id, revision, segment_index, status, started_at)
         VALUES (?1, ?2, 0, 'running', ?3)",
        params![turn_id, revision, now],
    )?;
    let (segment_index, segment_status): (i64, String) = tx.query_row(
        "SELECT segment_index, status FROM turn_journal_segments
         WHERE turn_id = ?1 AND revision = ?2
         ORDER BY segment_index DESC LIMIT 1",
        params![turn_id, revision],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (preceding_content, preceding_reasoning) = if segment_status == "running" {
        journal_segment_projection_locked(tx, turn_id, revision, segment_index)?
    } else {
        (String::new(), None)
    };

    for (index, (prompt_id, content)) in prompts.iter().enumerate() {
        let affected = tx.execute(
            "UPDATE queued_prompts
             SET status = 'consumed', consumed_at = ?1, turn_id = ?2,
                 context_content = ?3, preceding_assistant_content = ?4,
                 preceding_assistant_reasoning = ?5
             WHERE prompt_id = ?6 AND status = 'queued' AND queue_session_id = ?7",
            params![
                now,
                turn_id,
                content,
                (index == 0 && !preceding_content.trim().is_empty())
                    .then_some(preceding_content.as_str()),
                (index == 0)
                    .then_some(preceding_reasoning.as_deref())
                    .flatten(),
                prompt_id,
                queue_session_id,
            ],
        )?;
        if affected != 1 {
            bail!("queued prompt changed during stale-turn recovery: {prompt_id}");
        }
    }

    let prompt_ids = prompts
        .iter()
        .map(|(prompt_id, _)| prompt_id)
        .collect::<Vec<_>>();
    let prompt_payload = serde_json::to_string(&prompt_ids)?;
    let next_segment = segment_index.saturating_add(1);
    if segment_status == "superseded" {
        tx.execute(
            "INSERT INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, ?2, ?3, 'running', ?4)",
            params![turn_id, revision, next_segment, now],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, text_payload, created_at)
             VALUES (?1, ?2, ?3, 'queued_prompts_consumed', ?4, ?5)",
            params![turn_id, revision, next_segment, prompt_payload, now],
        )?;
    } else {
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, text_payload, created_at)
             VALUES (?1, ?2, ?3, 'queued_prompts_consumed', ?4, ?5)",
            params![turn_id, revision, segment_index, prompt_payload, now],
        )?;
        tx.execute(
            "UPDATE turn_journal_segments
             SET status = 'completed', finished_at = ?1
             WHERE turn_id = ?2 AND revision = ?3 AND segment_index = ?4",
            params![now, turn_id, revision, segment_index],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, ?2, ?3, 'running', ?4)",
            params![turn_id, revision, next_segment, now],
        )?;
    }
    Ok(prompts.len())
}

/// MAX() keeps the stamp monotonic even if a stale writer commits late; a
/// wall-clock step backwards must never make an idle session look fresh.
fn touch_session_last_request(tx: &Transaction<'_>, turn_id: &str) -> Result<()> {
    tx.execute(
        "UPDATE sessions SET last_request_at = MAX(COALESCE(last_request_at, 0), ?1)
         WHERE session_id = (SELECT session_id FROM turns WHERE turn_id = ?2)",
        params![Utc::now().timestamp(), turn_id],
    )?;
    Ok(())
}

/// 并发回合完成序追加(消除插入型缓存断点):回合从 running 首次转为
/// 可回放(completed/interrupted)时,若同会话已有 seq 更靠后的可回放
/// 回合,按原 seq 插回会落在后续请求已缓存前缀的中间,之后每个请求都
/// 从那里断链(群聊约 1/5 回合重叠)。把 seq 提升到会话全局 max+1,让
/// "已完成历史"跨请求保持 append-only——这也更忠实:并发回合的实况
/// 请求本来就没见过彼此,群聊时间线由各回合的群聊转储自己承载。
/// 只动首次完成的回合(revision=0):redo 修订的位置已被历史请求看过,
/// 原位改写才是正确语义。
fn bump_completion_seq_locked(tx: &Transaction<'_>, turn_id: &str) -> Result<()> {
    tx.execute(
        "UPDATE turns AS t
            SET seq = (SELECT MAX(o.seq) + 1 FROM turns AS o
                        WHERE o.session_id = t.session_id)
          WHERE t.turn_id = ?1
            AND t.revision = 0
            AND EXISTS (SELECT 1 FROM turns AS later
                         WHERE later.session_id = t.session_id
                           AND later.seq > t.seq
                           AND later.status != 'running')",
        params![turn_id],
    )?;
    Ok(())
}

fn interrupted_projection_locked(
    tx: &Transaction<'_>,
    turn_id: &str,
    revision: i64,
) -> Result<(String, Option<String>)> {
    let segment_index: Option<i64> = tx
        .query_row(
            "SELECT segment_index
             FROM turn_journal_segments
             WHERE turn_id = ?1 AND revision = ?2 AND status != 'superseded'
             ORDER BY segment_index DESC LIMIT 1",
            params![turn_id, revision],
            |row| row.get(0),
        )
        .optional()?;
    let Some(segment_index) = segment_index else {
        return Ok((INTERRUPTED_TEXT.to_string(), None));
    };
    let (content, reasoning) =
        journal_segment_projection_locked(tx, turn_id, revision, segment_index)?;
    let content = if content.trim().is_empty() {
        INTERRUPTED_TEXT.to_string()
    } else {
        format!("{content}\n\n{INTERRUPTED_TEXT}")
    };
    Ok((content, reasoning))
}

fn journal_segment_projection_locked(
    tx: &Transaction<'_>,
    turn_id: &str,
    revision: i64,
    segment_index: i64,
) -> Result<(String, Option<String>)> {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut stmt = tx.prepare(
        "SELECT kind, text_payload
         FROM turn_journal_events
         WHERE turn_id = ?1 AND revision = ?2 AND segment_index = ?3
         ORDER BY event_id",
    )?;
    let rows = stmt.query_map(params![turn_id, revision, segment_index], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for row in rows {
        let (kind, text) = row?;
        match kind.as_str() {
            "assistant_content" => {
                if let Some(text) = text {
                    content.push_str(&text);
                }
            }
            "assistant_reasoning" => {
                if let Some(text) = text {
                    reasoning.push_str(&text);
                }
            }
            "reasoning_reset" => reasoning.clear(),
            _ => {}
        }
    }
    let reasoning = (!reasoning.trim().is_empty()).then_some(reasoning);
    Ok((content, reasoning))
}

fn restore_redo_backup_locked(tx: &Transaction<'_>, turn_id: &str, revision: i64) -> Result<bool> {
    let payload = tx
        .query_row(
            "SELECT payload FROM turn_redo_backups
             WHERE turn_id = ?1 AND revision = ?2",
            params![turn_id, revision],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let backup: TurnRedoBackup = serde_json::from_slice(&payload)?;
    let session_id: String = tx.query_row(
        "SELECT session_id FROM turns
         WHERE turn_id = ?1 AND revision = ?2 AND status = 'running'",
        params![turn_id, revision],
        |row| row.get(0),
    )?;

    // The failed redo generation is disposable. Its journal must disappear
    // before the previous revision becomes active again, otherwise a later
    // interruption could replay output from the cancelled branch.
    tx.execute(
        "DELETE FROM turn_journal_segments WHERE turn_id = ?1 AND revision = ?2",
        params![turn_id, revision],
    )?;

    tx.execute(
        "DELETE FROM question_exchanges WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "INSERT INTO question_exchanges (turn_id, exchange_index, payload)
         SELECT turn_id, exchange_index, payload
         FROM turn_redo_question_backups WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "DELETE FROM image_assets WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "INSERT INTO image_assets
            (asset_id, turn_id, tool_id, mime, width, height, alt, data, created_at)
         SELECT asset_id, turn_id, tool_id, mime, width, height, alt, data, created_at
         FROM turn_redo_image_backups WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "DELETE FROM artifact_assets WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "INSERT INTO artifact_assets
            (asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
             size_bytes, data, created_at, updated_at)
         SELECT asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
                size_bytes, data, created_at, updated_at
         FROM turn_redo_artifact_backups WHERE turn_id = ?1",
        params![turn_id],
    )?;
    // 备份里的 tool_reports 已经是「列 + 子表」合并后的全部（见 redo.rs 的
    // 备份构造），还原时整份写回列，所以子表必须清空，不能两边都留。
    tx.execute(
        "DELETE FROM turn_tool_reports WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "DELETE FROM session_loaded_items WHERE session_id = ?1",
        params![session_id],
    )?;
    for (kind, name, source_turn_id, created_at, updated_at) in &backup.loaded_items {
        tx.execute(
            "INSERT INTO session_loaded_items
                (session_id, kind, name, source_turn_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session_id,
                kind,
                name,
                source_turn_id,
                created_at,
                updated_at
            ],
        )?;
    }
    let original_prompts = backup
        .consumed_prompt_ids
        .iter()
        .collect::<std::collections::HashSet<_>>();
    let current_prompts = {
        let mut stmt = tx.prepare(
            "SELECT prompt_id FROM queued_prompts
             WHERE turn_id = ?1 AND status = 'consumed'",
        )?;
        let rows = stmt
            .query_map(params![turn_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for prompt_id in current_prompts {
        if !original_prompts.contains(&prompt_id) {
            tx.execute(
                "DELETE FROM queued_prompts WHERE prompt_id = ?1",
                params![prompt_id],
            )?;
        }
    }
    tx.execute(
        "DELETE FROM turn_redo_checkpoints WHERE turn_id = ?1",
        params![turn_id],
    )?;
    if let Some(checkpoint) = &backup.checkpoint {
        tx.execute(
            "INSERT INTO turn_redo_checkpoints
                (turn_id, version, batch_prompt_ids, payload, unavailable_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                turn_id,
                checkpoint.version,
                checkpoint.batch_prompt_ids,
                checkpoint.payload,
                checkpoint.unavailable_reason,
                checkpoint.created_at
            ],
        )?;
    }
    tx.execute(
        "UPDATE turns SET
            user_content = ?1,
            display_content = ?2,
            assistant_content = ?3,
            assistant_reasoning = ?4,
            assistant_provider_id = ?5,
            assistant_model = ?6,
            assistant_timestamp = ?7,
            status = ?8,
            tool_reports = ?9,
            owner_pid = ?10,
            queue_session_id = ?11,
            token_total = ?12,
            token_usage_estimated = ?13,
            revision = ?14,
            token_prompt = ?17,
            token_cache_read = ?18
         WHERE turn_id = ?15 AND revision = ?16 AND status = 'running'",
        params![
            backup.user_content,
            backup.display_content,
            backup.assistant_content,
            backup.assistant_reasoning,
            backup.assistant_provider_id,
            backup.assistant_model,
            backup.assistant_timestamp,
            backup.status,
            backup.tool_reports,
            backup.owner_pid,
            backup.queue_session_id,
            backup.token_total,
            backup.token_usage_estimated,
            revision.saturating_sub(1),
            turn_id,
            revision,
            backup.token_prompt,
            backup.token_cache_read
        ],
    )?;
    if let (Some(content), Some(display_content)) = (
        backup.followup_content.as_deref(),
        backup.followup_display_content.as_deref(),
    ) {
        tx.execute(
            "UPDATE queued_prompts
             SET content = ?1, display_content = ?2, context_content = ?3
             WHERE prompt_id = (
                SELECT prompt_id FROM queued_prompts
                WHERE turn_id = ?4 AND status = 'consumed'
                ORDER BY seq DESC LIMIT 1
             )",
            params![
                content,
                display_content,
                backup.followup_context_content,
                turn_id
            ],
        )?;
    }
    tx.execute(
        "DELETE FROM turn_redo_backups WHERE turn_id = ?1 AND revision = ?2",
        params![turn_id, revision],
    )?;
    Ok(true)
}

impl ConversationDb {
    /// Display transcripts of the last `limit` visible turns of a session,
    /// oldest first. Turns finished before this column existed simply come
    /// back with an empty transcript, and the caller falls back to the plain
    /// prompt/reply pair.
    pub fn session_replay(&self, session_id: &str, limit: usize) -> Result<Vec<TurnReplay>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            // 这个 LIKE 标出 daemon 自己合成的轮：后台任务唤醒和目标续轮。
            // 它们不是用户输入，回放时不能画成用户气泡——判据取 `user_content`
            // 的开头标签，因为那是模型真正收到的东西，而 `display_content`
            // 是给人看的、随时可能改文案。
            "SELECT display_content, assistant_content, replay_journal,
                    (user_content LIKE '<background-job-report>%'
                     OR user_content LIKE '<goal_round>%')
               FROM turns
              WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                AND status = 'completed'
              ORDER BY seq DESC
              LIMIT ?2",
        )?;
        let mut rows = stmt
            .query_map(params![session_id, limit as i64], |row| {
                Ok(TurnReplay {
                    display_content: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    assistant_content: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    entries: row
                        .get::<_, Option<String>>(2)?
                        .and_then(|json| serde_json::from_str(&json).ok())
                        .unwrap_or_default(),
                    is_synthetic: row.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }
}

#[cfg(test)]
mod reclaim_tests_support {
    use super::*;

    pub(super) fn page_stats(path: &Path) -> (i64, i64, i64) {
        let conn = Connection::open(path).unwrap();
        let get = |pragma: &str| -> i64 {
            conn.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))
                .unwrap()
        };
        (get("auto_vacuum"), get("page_count"), get("freelist_count"))
    }
}

#[cfg(test)]
mod reclaim_tests {
    use super::reclaim_tests_support::*;
    use super::*;

    /// 造一个「老库」：`auto_vacuum = 0`（SQLite 的默认），塞一堆数据再删掉，
    /// 留下一屁股空闲页。这正是本机那个 76 MB / 90% 空闲的库的来历。
    fn write_legacy_database(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA auto_vacuum = NONE;
             PRAGMA journal_mode = DELETE;
             CREATE TABLE junk(id INTEGER PRIMARY KEY, body TEXT);",
        )
        .unwrap();
        let mut insert = conn.prepare("INSERT INTO junk(body) VALUES(?1)").unwrap();
        let filler = "x".repeat(2048);
        for _ in 0..2_000 {
            insert.execute(params![filler]).unwrap();
        }
        drop(insert);
        conn.execute_batch("DELETE FROM junk;").unwrap();
    }

    /// 老库打开时要被转换并回收：文件真的变小，`auto_vacuum` 从 0 变 2。
    #[test]
    fn opening_a_legacy_database_reclaims_free_pages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.db");
        write_legacy_database(&path);

        let (mode, pages, free) = page_stats(&path);
        assert_eq!(mode, 0, "构造出来的应该是老库");
        assert!(free * 2 > pages, "构造出来的库应该大半是空闲页");
        let before = std::fs::metadata(&path).unwrap().len();

        let db = ConversationDb::open(dir.path()).unwrap();
        drop(db);

        let (mode, pages, free) = page_stats(&path);
        let after = std::fs::metadata(&path).unwrap().len();
        assert_eq!(mode, 2, "应该已经转成 INCREMENTAL");
        assert!(after < before / 2, "文件没缩水：{before} → {after} 字节");
        assert!(
            free * 10 < pages,
            "回收后不该还剩大量空闲页：{free}/{pages}"
        );
    }

    /// 新库开箱就是 INCREMENTAL——`auto_vacuum` 只有在建表**之前**设才管用，
    /// 这条防的是有人把那句 PRAGMA 挪到 `run_migrations` 后面。
    #[test]
    fn a_fresh_database_is_created_with_incremental_vacuum() {
        let dir = tempfile::tempdir().unwrap();
        let db = ConversationDb::open(dir.path()).unwrap();
        drop(db);
        let (mode, _, _) = page_stats(&dir.path().join("conversation.db"));
        assert_eq!(mode, 2);
    }

    /// 转换只发生一次：第二次打开不该再跑完整 VACUUM。
    #[test]
    fn the_conversion_does_not_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.db");
        write_legacy_database(&path);
        drop(ConversationDb::open(dir.path()).unwrap());
        let (mode, _, _) = page_stats(&path);
        assert_eq!(mode, 2);
        drop(ConversationDb::open(dir.path()).unwrap());
        let (mode, _, _) = page_stats(&path);
        assert_eq!(mode, 2);
    }
}

#[cfg(test)]
mod reclaim_probe {
    use super::reclaim_tests_support::*;

    /// 量尺：把一份**真实**的 conversation.db 复制到临时目录，用真实的 `open`
    /// 路径跑一遍，看能还回去多少磁盘。
    ///
    /// ```
    /// cp ~/.miyu/state/conversation.db /tmp/probe/
    /// MIYU_RECLAIM_PROBE_DIR=/tmp/probe \
    ///   cargo test --lib reclaim_probe -- --ignored --nocapture
    /// ```
    ///
    /// 不读默认路径、只认显式给的目录：量尺不该顺手打开用户正在用的库。
    #[test]
    #[ignore]
    fn reclaim_on_a_real_database() {
        let Some(dir) = std::env::var_os("MIYU_RECLAIM_PROBE_DIR") else {
            println!("\n  跳过：没给 MIYU_RECLAIM_PROBE_DIR");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let path = dir.join("conversation.db");
        let (mode, pages, free) = page_stats(&path);
        let before = std::fs::metadata(&path).unwrap().len();
        println!(
            "\n  改前  {:.1} MB  auto_vacuum={mode}  页 {pages}（空闲 {free}，{:.0}%）",
            before as f64 / 1048576.0,
            100.0 * free as f64 / pages as f64
        );

        let started = std::time::Instant::now();
        drop(super::ConversationDb::open(&dir).unwrap());
        let elapsed = started.elapsed();

        let (mode, pages, free) = page_stats(&path);
        let after = std::fs::metadata(&path).unwrap().len();
        println!(
            "  改后  {:.1} MB  auto_vacuum={mode}  页 {pages}（空闲 {free}）\n  \
             还回 {:.1} MB（{:.0}%），open 用时 {:.0} ms",
            after as f64 / 1048576.0,
            (before - after) as f64 / 1048576.0,
            100.0 * (before - after) as f64 / before as f64,
            elapsed.as_secs_f64() * 1000.0,
        );
    }
}
