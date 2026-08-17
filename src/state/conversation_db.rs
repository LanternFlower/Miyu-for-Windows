use crate::i18n::text as t;
use crate::llm::{ChatMessage, TurnTokens};
use crate::memory::EvictedTurn;
use crate::question::QuestionExchange;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

const PENDING_PLACEHOLDER: &str = "<system-reminder>上一轮prompt正在由另一轮回复处理中，你只需要回应用户当前的prompt，不要处理上一轮的prompt</system-reminder>";
const INTERRUPTED_TEXT: &str =
    "<system-reminder>上一轮prompt已被中断，除非用户重新要求否则不要处理上一轮的prompt</system-reminder>";

/// Budget for a finished turn's display transcript. Generous enough for a
/// normal turn's prose plus a handful of tool blocks, small enough that a
/// session's worth of them stays cheap to load.
const REPLAY_JOURNAL_MAX_CHARS: usize = 8 * 1024;
/// Per-entry clamp so one runaway tool result cannot eat the whole budget.
const REPLAY_ENTRY_MAX_CHARS: usize = 2 * 1024;

/// One entry of a finished turn's display transcript, in stream order.
///
/// Reconstructed from the live journal just before it is dropped, so the
/// interleaving of prose and tool blocks survives — which is the whole point,
/// since `assistant_content` alone would flatten a turn into one paragraph.
/// Command output tails are deliberately absent: they are the bulky part and
/// the settled block reads fine without them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplayEntry {
    Text {
        text: String,
    },
    ToolCall {
        name: String,
        #[serde(default)]
        arguments: String,
    },
    ToolResult {
        name: String,
        ok: bool,
        #[serde(default)]
        output: String,
    },
}

/// `app_state` key prefixes for the two persona-scoped session pointers. The
/// terminal lane (shell-hook, `miyu new`/`session`) and the REPL lane move
/// independently; one-shot `ask` turns use neither.
const CURRENT_SESSION_POINTER: &str = "current_session_persona";
const REPL_SESSION_POINTER: &str = "repl_session_persona";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStatus {
    Running,
    Completed,
    Interrupted,
}

#[allow(dead_code)]
impl TurnStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "completed" => Self::Completed,
            "interrupted" => Self::Interrupted,
            _ => Self::Running,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PruneStats {
    pub turns: usize,
    pub saved_chars: usize,
}

/// Deterministic per-turn tool footprint. BTreeSet: sorted, deduplicated,
/// byte-deterministic serialization (cache-purity requirement for anything
/// that ends up in a rendered summary).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ToolFootprint {
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub read: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub modified: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub memories: std::collections::BTreeSet<String>,
}

impl ToolFootprint {
    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.modified.is_empty() && self.memories.is_empty()
    }

    pub fn merge(&mut self, other: ToolFootprint) {
        self.read.extend(other.read);
        self.modified.extend(other.modified);
        self.memories.extend(other.memories);
    }
}

/// 一轮工具调用:assistant(可带思考)发起若干 call,随后各自的结果。
/// `output` 与该轮模型实际看到的字节一致(超限时是 spill 预览),回放即重现。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolFlowRound {
    #[serde(default)]
    pub assistant_content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_reasoning: Option<String>,
    pub calls: Vec<ToolFlowCall>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolFlowCall {
    pub id: String,
    pub name: String,
    /// 模型原样产出的 JSON 字符串,不解析不重排(dsh:字节保真)。
    pub arguments: String,
    pub output: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Turn {
    pub turn_id: String,
    pub seq: i64,
    pub user_content: String,
    pub display_content: String,
    pub user_timestamp: String,
    pub assistant_content: String,
    pub assistant_reasoning: Option<String>,
    pub assistant_provider_id: Option<String>,
    pub assistant_model: Option<String>,
    pub assistant_timestamp: Option<String>,
    pub status: TurnStatus,
    pub tool_reports: Vec<String>,
    /// 结构化工具流(v20+):非空时历史回放走原生 tool_calls/role:"tool" 形态,
    /// tool_reports 只服务 UI 与旧回合兜底。
    pub tool_flow: Vec<ToolFlowRound>,
    pub question_exchanges: Vec<QuestionExchange>,
    pub followups: Vec<TurnFollowup>,
    pub attachments: Vec<UserAttachment>,
    pub hidden: bool,
    pub is_summary: bool,
    pub owner_pid: Option<i64>,
    pub token_total: u64,
    /// Prompt half of the turn's usage and how much of it the provider served
    /// from cache. A hit rate needs the prompt as its denominator, not the
    /// total: output tokens only enter the prompt on the *next* turn.
    pub token_prompt: u64,
    pub token_cache_read: u64,
    pub token_usage_estimated: bool,
    pub revision: i64,
    /// Semantic events for a non-completed generation. Completed turns keep
    /// this empty so normal history loading does not materialize large logs.
    pub journal_events: Vec<TurnJournalEvent>,
    /// Fossilized transient tail (v7 append-only): the system messages that
    /// followed the user message in the live request, replayed verbatim so the
    /// provider prefix cache sees a pure extension instead of a divergence.
    pub context_messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnJournalEvent {
    pub event_id: i64,
    pub revision: i64,
    pub segment_index: i64,
    pub kind: String,
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub text_payload: Option<String>,
    pub blob_payload: Option<Vec<u8>>,
    pub ok: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRedoCheckpointPayload {
    pub replay_messages: Vec<ChatMessage>,
    pub prefix_tool_reports: Vec<String>,
    pub tool_rounds: usize,
    pub question_rounds: usize,
    pub loaded_items: Vec<(String, String, Option<String>)>,
    pub prefix_question_count: usize,
    pub prefix_image_asset_ids: Vec<String>,
    #[serde(default)]
    pub prefix_artifact_asset_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TurnRedoCheckpoint {
    pub batch_prompt_ids: Vec<String>,
    pub payload: Option<TurnRedoCheckpointPayload>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedoInputKind {
    Initial,
    Followup,
}

#[derive(Debug, Clone)]
pub struct RedoCandidate {
    pub turn_id: String,
    pub revision: i64,
    pub input_id: String,
    pub input_kind: RedoInputKind,
    pub display_content: String,
    pub batch_prompt_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RedoStart {
    pub revision: i64,
    pub checkpoint: Option<TurnRedoCheckpointPayload>,
}

#[derive(Debug, Clone)]
pub struct StaleTurnRecovery {
    pub turn_id: String,
    pub session_id: String,
    pub restored_redo: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct TurnRedoBackup {
    status: String,
    user_content: String,
    display_content: String,
    followup_content: Option<String>,
    followup_display_content: Option<String>,
    followup_context_content: Option<String>,
    assistant_content: String,
    assistant_reasoning: Option<String>,
    assistant_provider_id: Option<String>,
    assistant_model: Option<String>,
    assistant_timestamp: Option<String>,
    tool_reports: String,
    owner_pid: Option<i64>,
    queue_session_id: Option<String>,
    token_total: i64,
    #[serde(default)]
    token_prompt: i64,
    #[serde(default)]
    token_cache_read: i64,
    token_usage_estimated: i64,
    loaded_items: Vec<(String, String, Option<String>, String, String)>,
    consumed_prompt_ids: Vec<String>,
    checkpoint: Option<RedoCheckpointBackup>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RedoCheckpointBackup {
    version: i64,
    batch_prompt_ids: String,
    payload: Option<Vec<u8>>,
    unavailable_reason: Option<String>,
    created_at: String,
}

const REDO_CHECKPOINT_VERSION: i64 = 1;
const MAX_REDO_CHECKPOINT_BYTES: usize = 2 * 1024 * 1024;
const MAX_JOURNAL_TEXT_EVENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_JOURNAL_BLOB_EVENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueuedPromptAttachment {
    Binary { mime: String, data_base64: String },
    Path { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedPrompt {
    pub prompt_id: String,
    pub seq: i64,
    pub content: String,
    pub display_content: String,
    pub attachments: Vec<QueuedPromptAttachment>,
    pub uploaded_attachments: Vec<UserAttachment>,
    pub submitted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFollowup {
    pub prompt_id: String,
    pub content: String,
    pub display_content: String,
    pub attachments: Vec<QueuedPromptAttachment>,
    pub uploaded_attachments: Vec<UserAttachment>,
    pub submitted_at: String,
    pub preceding_assistant_content: Option<String>,
    pub preceding_assistant_reasoning: Option<String>,
    pub preceding_assistant_provider_id: Option<String>,
    pub preceding_assistant_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAttachment {
    pub attachment_id: String,
    pub file_name: String,
    pub mime: String,
    pub kind: String,
    pub size_bytes: u64,
    pub width: u32,
    pub height: u32,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct UserAttachmentData {
    pub attachment: UserAttachment,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAsset {
    pub asset_id: String,
    pub turn_id: String,
    pub tool_id: Option<String>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub alt: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct ImageAssetData {
    pub asset: ImageAsset,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactAsset {
    pub asset_id: String,
    pub turn_id: String,
    pub tool_id: Option<String>,
    pub source_key: String,
    pub file_name: String,
    pub mime: String,
    pub kind: String,
    pub size_bytes: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct ArtifactAssetData {
    pub asset: ArtifactAsset,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: String,
    pub persona: String,
    pub name: String,
    pub kind: String,
    pub parent_session_id: Option<String>,
    pub workspace: Option<String>,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct SessionOverview {
    pub record: SessionRecord,
    pub turn_count: i64,
    pub last_user_content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlatformSessionBindingKey {
    pub platform: String,
    pub account_id: String,
    pub conversation_kind: String,
    pub conversation_id: String,
    pub participant_id: Option<String>,
    pub persona: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformSessionBinding {
    pub key: PlatformSessionBindingKey,
    pub session_id: String,
}

impl PlatformSessionBindingKey {
    fn normalized_participant_id(&self) -> &str {
        self.participant_id.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlatformPluginScopeKey {
    pub plugin_id: String,
    pub platform: String,
    pub account_id: String,
    pub conversation_kind: String,
    pub conversation_id: String,
}

/// Account scope shared by every account on one platform.
pub const GLOBAL_PLATFORM_ACCOUNT_SCOPE: &str = "*";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlatformAccessGrantKey {
    pub platform: String,
    pub account_scope: String,
    pub permission: String,
    pub subject_kind: String,
    pub subject_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformAccessActor {
    pub platform: String,
    pub account_id: String,
    pub user_id: String,
    pub conversation_kind: String,
    pub conversation_id: String,
    pub message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformAccessGrant {
    pub key: PlatformAccessGrantKey,
    pub granted_by: PlatformAccessActor,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformMemeRefRecord {
    pub platform: String,
    pub account_id: String,
    pub conversation_kind: String,
    pub conversation_id: String,
    pub message_id: String,
    pub library: String,
    pub meme_id: String,
    pub direction: String,
    pub created_at: String,
}

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

const SESSION_COLUMNS: &str = "session_id, persona, name, kind, parent_session_id, workspace, archived, created_at, updated_at";

fn session_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        session_id: row.get("session_id")?,
        persona: row.get("persona")?,
        name: row.get("name")?,
        kind: row.get("kind")?,
        parent_session_id: row.get("parent_session_id")?,
        workspace: row.get("workspace")?,
        archived: row.get("archived")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

pub struct ConversationDb {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for ConversationDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationDb").finish_non_exhaustive()
    }
}

impl ConversationDb {
    pub fn open(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        let db_path = state_dir.join("conversation.db");
        let mut conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open conversation db: {}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
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
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Resolves the current session pointer from `app_state`, self-healing a
    /// missing pointer or dangling session row back to the default session.
    pub fn resolve_current_session(&self) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let pointer: Option<String> = conn
            .query_row(
                "SELECT value FROM app_state WHERE key = 'current_session'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(session_id) = pointer {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
                params![session_id],
                |row| row.get(0),
            )?;
            if exists {
                return Ok(session_id);
            }
        }
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO sessions (session_id, persona, name, kind, created_at, updated_at)
             VALUES (?1, '', ?2, 'user', ?3, ?3)",
            params![
                super::migrations::DEFAULT_SESSION_ID,
                t("Terminal session", "终端集成会话"),
                now
            ],
        )?;
        conn.execute(
            "INSERT INTO app_state (key, value) VALUES ('current_session', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![super::migrations::DEFAULT_SESSION_ID],
        )?;
        Ok(super::migrations::DEFAULT_SESSION_ID.to_string())
    }

    /// Persists the current-session pointer. The target session must exist.
    pub fn set_current_session(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
            params![session_id],
            |row| row.get(0),
        )?;
        if !exists {
            bail!("session not found: {session_id}");
        }
        conn.execute(
            "INSERT INTO app_state (key, value) VALUES ('current_session', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![session_id],
        )?;
        Ok(())
    }

    /// Reads a persona-scoped session pointer, returning `None` when it points
    /// at something the caller must not land on (wrong persona, non-user kind,
    /// archived, or already deleted). Callers fall back and heal the pointer.
    fn persona_session_pointer(&self, prefix: &str, persona: &str) -> Result<Option<String>> {
        let key = format!("{prefix}:{persona}");
        let conn = self.conn.lock().unwrap();
        let session_id = conn
            .query_row(
                "SELECT value FROM app_state WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let valid = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1 AND persona = ?2 AND kind = ?3)",
                params![session_id, persona, super::USER_SESSION_KIND],
                |row| row.get::<_, bool>(0),
            )?;
        Ok(valid.then_some(session_id))
    }

    fn set_persona_session_pointer(
        &self,
        prefix: &str,
        persona: &str,
        session_id: &str,
    ) -> Result<()> {
        let key = format!("{prefix}:{persona}");
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO app_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, session_id],
        )?;
        Ok(())
    }

    pub fn persona_current_session(&self, persona: &str) -> Result<Option<String>> {
        self.persona_session_pointer(CURRENT_SESSION_POINTER, persona)
    }

    pub fn set_persona_current_session(&self, persona: &str, session_id: &str) -> Result<()> {
        self.set_persona_session_pointer(CURRENT_SESSION_POINTER, persona, session_id)
    }

    /// The REPL's own lane. Kept apart from the current-session pointer so a
    /// REPL reopens where it left off while shell-hook keeps using the
    /// terminal session it was on.
    pub fn repl_session(&self, persona: &str) -> Result<Option<String>> {
        self.persona_session_pointer(REPL_SESSION_POINTER, persona)
    }

    pub fn set_repl_session(&self, persona: &str, session_id: &str) -> Result<()> {
        self.set_persona_session_pointer(REPL_SESSION_POINTER, persona, session_id)
    }

    /// Claims persona-less sessions (schema-v2 migrated rows) for the given
    /// persona scope. Called once at daemon startup with the active persona.
    pub fn adopt_sessions_for_persona(&self, persona: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET persona = ?1 WHERE persona = ''",
            params![persona],
        )?;
        Ok(())
    }

    pub fn rename_persona_scope(&self, old_scope: &str, new_scope: &str) -> Result<()> {
        if old_scope == new_scope {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE persona = ?1)",
            params![new_scope],
            |row| row.get(0),
        )?;
        if target_exists {
            bail!("persona scope already has sessions: {new_scope}");
        }
        let old_key = format!("current_session_persona:{old_scope}");
        let new_key = format!("current_session_persona:{new_scope}");
        let target_pointer_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM app_state WHERE key = ?1)",
            params![new_key],
            |row| row.get(0),
        )?;
        if target_pointer_exists {
            bail!("persona scope already has a current-session pointer: {new_scope}");
        }
        let old_affection_key = format!("affection_profile:{old_scope}");
        let new_affection_key = format!("affection_profile:{new_scope}");
        let target_affection_exists: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM platform_plugin_kv
                 WHERE plugin_id = 'real_context' AND key = ?1
             )",
            params![new_affection_key],
            |row| row.get(0),
        )?;
        if target_affection_exists {
            bail!("persona scope already has affection state: {new_scope}");
        }

        tx.execute(
            "UPDATE platform_session_bindings SET persona = ?2 WHERE persona = ?1",
            params![old_scope, new_scope],
        )?;
        tx.execute(
            "UPDATE sessions SET persona = ?2 WHERE persona = ?1",
            params![old_scope, new_scope],
        )?;
        tx.execute(
            "UPDATE app_state SET key = ?2 WHERE key = ?1",
            params![old_key, new_key],
        )?;
        tx.execute(
            "UPDATE platform_plugin_kv SET key = ?2
              WHERE plugin_id = 'real_context' AND key = ?1",
            params![old_affection_key, new_affection_key],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_persona_scope(&self, scope: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM sessions WHERE persona = ?1", params![scope])?;
        tx.execute(
            "DELETE FROM app_state WHERE key = ?1",
            params![format!("current_session_persona:{scope}")],
        )?;
        tx.execute(
            "DELETE FROM platform_plugin_kv
              WHERE plugin_id = 'real_context' AND key = ?1",
            params![format!("affection_profile:{scope}")],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn session_record(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE session_id = ?1"),
                params![session_id],
                session_record_from_row,
            )
            .optional()?)
    }

    /// User-facing sessions of a persona, most recently updated first.
    /// Subagent sessions (`kind != 'user'`) are excluded.
    pub fn list_sessions(&self, persona: &str) -> Result<Vec<SessionOverview>> {
        self.list_sessions_filtered(persona, false)
    }

    /// Local user sessions suitable for CLI/WebUI navigation. Sessions
    /// owned by a messaging-platform binding keep their history but are not
    /// exposed as local conversations.
    pub fn list_local_sessions(&self, persona: &str) -> Result<Vec<SessionOverview>> {
        self.list_sessions_filtered(persona, true)
    }

    fn list_sessions_filtered(
        &self,
        persona: &str,
        local_only: bool,
    ) -> Result<Vec<SessionOverview>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS},
                    (SELECT count(*) FROM turns
                      WHERE turns.session_id = sessions.session_id
                        AND hidden = 0 AND is_summary = 0) AS turn_count,
                    (SELECT display_content FROM turns
                      WHERE turns.session_id = sessions.session_id
                        AND hidden = 0 AND is_summary = 0
                      ORDER BY seq DESC LIMIT 1) AS last_user_content
             FROM sessions
             WHERE persona = ?1 AND kind = 'user'
               AND (?2 = 0 OR NOT EXISTS (
                    SELECT 1 FROM platform_session_bindings
                    WHERE platform_session_bindings.session_id = sessions.session_id
               ))
             ORDER BY updated_at DESC"
        ))?;
        let rows = stmt.query_map(params![persona, local_only], |row| {
            Ok(SessionOverview {
                record: session_record_from_row(row)?,
                turn_count: row.get("turn_count")?,
                last_user_content: row.get("last_user_content")?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 最老可见轮的用户时间戳(排除指定回合;Utc RFC3339)。联想自回声
    /// 过滤用它当"仍在眼前"的下界:被 compact 藏起的轮不算。
    pub fn oldest_visible_turn_timestamp(
        &self,
        session_id: &str,
        excluding_turn_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT MIN(user_timestamp) FROM turns
              WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0 AND turn_id != ?2",
            params![session_id, excluding_turn_id],
            |row| row.get::<_, Option<String>>(0),
        )?)
    }

    pub fn is_platform_session(&self, session_id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM platform_session_bindings WHERE session_id = ?1
            )",
            params![session_id],
            |row| row.get(0),
        )?)
    }

    pub fn persona_reset_session_ids(&self, persona: &str, platform: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "WITH RECURSIVE targets(session_id) AS (
                 SELECT sessions.session_id
                   FROM sessions
                  WHERE sessions.persona = ?1
                    AND sessions.kind = 'user'
                    AND (
                        (NOT EXISTS (
                            SELECT 1 FROM platform_session_bindings
                             WHERE platform_session_bindings.session_id = sessions.session_id
                        ))
                        OR EXISTS (
                            SELECT 1 FROM platform_session_bindings
                             WHERE platform_session_bindings.session_id = sessions.session_id
                               AND platform_session_bindings.platform = ?2
                        )
                    )
                 UNION
                 SELECT child.session_id
                   FROM sessions child
                   JOIN targets parent ON child.parent_session_id = parent.session_id
                  WHERE child.persona = ?1
             )
             SELECT session_id FROM targets ORDER BY session_id",
        )?;
        let rows = stmt.query_map(params![persona, platform], |row| row.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn platform_session_bindings(
        &self,
        persona: &str,
        platform: &str,
    ) -> Result<Vec<PlatformSessionBinding>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT platform, account_id, conversation_kind, conversation_id,
                    participant_id, persona, session_id
               FROM platform_session_bindings
              WHERE persona = ?1 AND platform = ?2
              ORDER BY account_id, conversation_kind, conversation_id, participant_id",
        )?;
        let rows = stmt.query_map(params![persona, platform], |row| {
            let participant_id: String = row.get(4)?;
            Ok(PlatformSessionBinding {
                key: PlatformSessionBindingKey {
                    platform: row.get(0)?,
                    account_id: row.get(1)?,
                    conversation_kind: row.get(2)?,
                    conversation_id: row.get(3)?,
                    participant_id: (!participant_id.is_empty()).then_some(participant_id),
                    persona: row.get(5)?,
                },
                session_id: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn create_session(
        &self,
        persona: &str,
        name: &str,
        kind: &str,
        parent_session_id: Option<&str>,
    ) -> Result<SessionRecord> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        let session_id = format!(
            "sess_{}_{:08x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0),
            rand::random::<u32>()
        );
        conn.execute(
            "INSERT INTO sessions (session_id, persona, name, kind, parent_session_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![session_id, persona, name, kind, parent_session_id, now],
        )?;
        drop(conn);
        Ok(self
            .session_record(&session_id)?
            .expect("session row just inserted"))
    }

    pub fn create_or_get_platform_session(
        &self,
        key: &PlatformSessionBindingKey,
        name: &str,
    ) -> Result<(SessionRecord, bool)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(session_id) = tx
            .query_row(
                "SELECT session_id FROM platform_session_bindings
                 WHERE platform = ?1 AND account_id = ?2
                   AND conversation_kind = ?3 AND conversation_id = ?4
                   AND participant_id = ?5 AND persona = ?6",
                params![
                    key.platform,
                    key.account_id,
                    key.conversation_kind,
                    key.conversation_id,
                    key.normalized_participant_id(),
                    key.persona,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let record = tx.query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE session_id = ?1"),
                params![session_id],
                session_record_from_row,
            )?;
            tx.commit()?;
            return Ok((record, false));
        }

        let now = Utc::now().to_rfc3339();
        let session_id = format!(
            "sess_{}_{:08x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0),
            rand::random::<u32>()
        );
        tx.execute(
            "INSERT INTO sessions (session_id, persona, name, kind, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'user', ?4, ?4)",
            params![session_id, key.persona, name, now],
        )?;
        tx.execute(
            "INSERT INTO platform_session_bindings (
                platform, account_id, conversation_kind, conversation_id,
                participant_id, persona, session_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                key.platform,
                key.account_id,
                key.conversation_kind,
                key.conversation_id,
                key.normalized_participant_id(),
                key.persona,
                session_id,
                now,
            ],
        )?;
        let record = SessionRecord {
            session_id,
            persona: key.persona.clone(),
            name: name.to_string(),
            kind: "user".to_string(),
            parent_session_id: None,
            workspace: None,
            archived: false,
            created_at: now.clone(),
            updated_at: now,
        };
        tx.commit()?;
        Ok((record, true))
    }

    pub fn rename_session(&self, session_id: &str, name: &str) -> Result<()> {
        self.update_session_field(session_id, "name", Some(name))
    }

    pub fn set_session_workspace(&self, session_id: &str, workspace: Option<&str>) -> Result<()> {
        self.update_session_field(session_id, "workspace", workspace)
    }

    /// JSON-encoded per-session model pool override
    /// (`[{"provider_id": ..., "model": ...}, ...]`); None follows the global
    /// active pool.
    pub fn session_model_override(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let value = conn
            .query_row(
                "SELECT model_override FROM sessions WHERE session_id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        Ok(value.flatten())
    }

    pub fn set_session_model_override(&self, session_id: &str, value: Option<&str>) -> Result<()> {
        self.update_session_field(session_id, "model_override", value)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        // queued_prompts gained session_id through an ALTER TABLE migration,
        // so existing databases cannot rely on an ON DELETE foreign key.
        tx.execute(
            "DELETE FROM queued_prompts WHERE session_id = ?1",
            params![session_id],
        )?;
        let deleted = tx.execute(
            "DELETE FROM sessions WHERE session_id = ?1",
            params![session_id],
        )?;
        if deleted == 0 {
            bail!("session not found: {session_id}");
        }
        tx.commit()?;
        Ok(())
    }

    pub fn touch_session(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE session_id = ?1",
            params![session_id, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn find_session_by_name(&self, persona: &str, name: &str) -> Result<Option<SessionRecord>> {
        self.find_session_by_name_filtered(persona, name, false)
    }

    pub fn find_local_session_by_name(
        &self,
        persona: &str,
        name: &str,
    ) -> Result<Option<SessionRecord>> {
        self.find_session_by_name_filtered(persona, name, true)
    }

    fn find_session_by_name_filtered(
        &self,
        persona: &str,
        name: &str,
        local_only: bool,
    ) -> Result<Option<SessionRecord>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM sessions
                      WHERE persona = ?1 AND kind = 'user' AND name = ?2 COLLATE NOCASE
                        AND (?3 = 0 OR NOT EXISTS (
                            SELECT 1 FROM platform_session_bindings
                             WHERE platform_session_bindings.session_id = sessions.session_id
                        ))
                      ORDER BY archived ASC, updated_at DESC LIMIT 1"
                ),
                params![persona, name, local_only],
                session_record_from_row,
            )
            .optional()?)
    }

    pub fn find_platform_session_binding(
        &self,
        key: &PlatformSessionBindingKey,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT session_id FROM platform_session_bindings
                 WHERE platform = ?1 AND account_id = ?2
                   AND conversation_kind = ?3 AND conversation_id = ?4
                   AND participant_id = ?5 AND persona = ?6",
                params![
                    key.platform,
                    key.account_id,
                    key.conversation_kind,
                    key.conversation_id,
                    key.normalized_participant_id(),
                    key.persona,
                ],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Binds an external conversation identity to a session in one immediate
    /// transaction. A key may be reassigned, but a session already owned by a
    /// different key is never stolen.
    pub fn bind_platform_session(
        &self,
        key: &PlatformSessionBindingKey,
        session_id: &str,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
            params![session_id],
            |row| row.get(0),
        )?;
        if !session_exists {
            bail!("session not found: {session_id}");
        }

        let owned_by_another_key: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM platform_session_bindings
                 WHERE session_id = ?7
                   AND NOT (
                       platform = ?1 AND account_id = ?2
                       AND conversation_kind = ?3 AND conversation_id = ?4
                       AND participant_id = ?5 AND persona = ?6
                   )
             )",
            params![
                key.platform,
                key.account_id,
                key.conversation_kind,
                key.conversation_id,
                key.normalized_participant_id(),
                key.persona,
                session_id,
            ],
            |row| row.get(0),
        )?;
        if owned_by_another_key {
            bail!("session is already bound to another platform conversation: {session_id}");
        }

        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO platform_session_bindings (
                platform, account_id, conversation_kind, conversation_id,
                participant_id, persona, session_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT (
                platform, account_id, conversation_kind, conversation_id,
                participant_id, persona
             ) DO UPDATE SET
                session_id = excluded.session_id,
                updated_at = excluded.updated_at",
            params![
                key.platform,
                key.account_id,
                key.conversation_kind,
                key.conversation_id,
                key.normalized_participant_id(),
                key.persona,
                session_id,
                now,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Claims an unbound external key without replacing an existing binding.
    /// Returns the winning session id so concurrent first messages converge
    /// on one history instead of creating two active sessions.
    pub fn claim_platform_session(
        &self,
        key: &PlatformSessionBindingKey,
        candidate_session_id: &str,
    ) -> Result<String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = tx
            .query_row(
                "SELECT session_id FROM platform_session_bindings
                 WHERE platform = ?1 AND account_id = ?2
                   AND conversation_kind = ?3 AND conversation_id = ?4
                   AND participant_id = ?5 AND persona = ?6",
                params![
                    key.platform,
                    key.account_id,
                    key.conversation_kind,
                    key.conversation_id,
                    key.normalized_participant_id(),
                    key.persona,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            tx.commit()?;
            return Ok(existing);
        }
        let session_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
            params![candidate_session_id],
            |row| row.get(0),
        )?;
        if !session_exists {
            bail!("session not found: {candidate_session_id}");
        }
        let already_owned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM platform_session_bindings WHERE session_id = ?1)",
            params![candidate_session_id],
            |row| row.get(0),
        )?;
        if already_owned {
            bail!(
                "session is already bound to another platform conversation: {candidate_session_id}"
            );
        }
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO platform_session_bindings (
                platform, account_id, conversation_kind, conversation_id,
                participant_id, persona, session_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                key.platform,
                key.account_id,
                key.conversation_kind,
                key.conversation_id,
                key.normalized_participant_id(),
                key.persona,
                candidate_session_id,
                now,
            ],
        )?;
        tx.commit()?;
        Ok(candidate_session_id.to_string())
    }

    pub fn unbind_platform_session(&self, key: &PlatformSessionBindingKey) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM platform_session_bindings
             WHERE platform = ?1 AND account_id = ?2
               AND conversation_kind = ?3 AND conversation_id = ?4
               AND participant_id = ?5 AND persona = ?6",
            params![
                key.platform,
                key.account_id,
                key.conversation_kind,
                key.conversation_id,
                key.normalized_participant_id(),
                key.persona,
            ],
        )?;
        Ok(deleted != 0)
    }

    pub fn platform_access_grants(
        &self,
        platform: Option<&str>,
    ) -> Result<Vec<PlatformAccessGrant>> {
        let conn = self.conn.lock().unwrap();
        let mut statement = conn.prepare(
            "SELECT
                 platform, account_scope, permission, subject_kind, subject_id,
                 granted_by_platform, granted_by_account_id, granted_by_user_id,
                 granted_conversation_kind, granted_conversation_id,
                 granted_message_id, created_at
             FROM platform_access_grants
             WHERE (?1 IS NULL OR platform = ?1)
             ORDER BY platform, account_scope, permission, subject_kind, subject_id",
        )?;
        let rows = statement.query_map(params![platform], |row| {
            Ok(PlatformAccessGrant {
                key: PlatformAccessGrantKey {
                    platform: row.get("platform")?,
                    account_scope: row.get("account_scope")?,
                    permission: row.get("permission")?,
                    subject_kind: row.get("subject_kind")?,
                    subject_id: row.get("subject_id")?,
                },
                granted_by: PlatformAccessActor {
                    platform: row.get("granted_by_platform")?,
                    account_id: row.get("granted_by_account_id")?,
                    user_id: row.get("granted_by_user_id")?,
                    conversation_kind: row.get("granted_conversation_kind")?,
                    conversation_id: row.get("granted_conversation_id")?,
                    message_id: row.get("granted_message_id")?,
                },
                created_at: row.get("created_at")?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn add_platform_access_grant(
        &self,
        key: &PlatformAccessGrantKey,
        actor: &PlatformAccessActor,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let created_at = Utc::now().to_rfc3339();
        let inserted = tx.execute(
            "INSERT INTO platform_access_grants (
                 platform, account_scope, permission, subject_kind, subject_id,
                 granted_by_platform, granted_by_account_id, granted_by_user_id,
                 granted_conversation_kind, granted_conversation_id,
                 granted_message_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT (
                 platform, account_scope, permission, subject_kind, subject_id
             ) DO NOTHING",
            params![
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
        if inserted != 0 {
            insert_platform_access_audit(&tx, "grant", key, actor, &created_at)?;
        }
        tx.commit()?;
        Ok(inserted != 0)
    }

    pub fn remove_platform_access_grant(
        &self,
        key: &PlatformAccessGrantKey,
        actor: &PlatformAccessActor,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = tx.execute(
            "DELETE FROM platform_access_grants
             WHERE platform = ?1 AND account_scope = ?2 AND permission = ?3
               AND subject_kind = ?4 AND subject_id = ?5",
            params![
                key.platform,
                key.account_scope,
                key.permission,
                key.subject_kind,
                key.subject_id,
            ],
        )?;
        if deleted != 0 {
            let created_at = Utc::now().to_rfc3339();
            insert_platform_access_audit(&tx, "revoke", key, actor, &created_at)?;
        }
        tx.commit()?;
        Ok(deleted != 0)
    }

    pub fn plugin_get_json<T: DeserializeOwned>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
    ) -> Result<Option<T>> {
        let conn = self.conn.lock().unwrap();
        let value_json = conn
            .query_row(
                "SELECT value_json FROM platform_plugin_kv
                 WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
                   AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
                params![
                    scope.plugin_id,
                    scope.platform,
                    scope.account_id,
                    scope.conversation_kind,
                    scope.conversation_id,
                    key,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        drop(conn);
        value_json
            .map(|value| serde_json::from_str(&value).context("invalid platform plugin JSON state"))
            .transpose()
    }

    pub fn plugin_json_revision(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT updated_at FROM platform_plugin_kv
             WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
               AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
            params![
                scope.plugin_id,
                scope.platform,
                scope.account_id,
                scope.conversation_kind,
                scope.conversation_id,
                key,
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn plugin_get_json_with_revision<T: DeserializeOwned>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
    ) -> Result<Option<(T, String)>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT value_json, updated_at FROM platform_plugin_kv
                 WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
                   AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
                params![
                    scope.plugin_id,
                    scope.platform,
                    scope.account_id,
                    scope.conversation_kind,
                    scope.conversation_id,
                    key,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        drop(conn);
        row.map(|(value, revision)| {
            serde_json::from_str(&value)
                .context("invalid platform plugin JSON state")
                .map(|value| (value, revision))
        })
        .transpose()
    }

    pub fn plugin_put_json<T: Serialize + ?Sized>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
        value: &T,
    ) -> Result<()> {
        let value_json =
            serde_json::to_string(value).context("failed to serialize platform plugin state")?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO platform_plugin_kv (
                plugin_id, platform, account_id, conversation_kind,
                conversation_id, key, value_json, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (
                plugin_id, platform, account_id, conversation_kind,
                conversation_id, key
             ) DO UPDATE SET
                value_json = excluded.value_json,
                updated_at = excluded.updated_at",
            params![
                scope.plugin_id,
                scope.platform,
                scope.account_id,
                scope.conversation_kind,
                scope.conversation_id,
                key,
                value_json,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Atomically replaces one plugin value. Returning `None` deletes it.
    /// The callback runs inside an immediate transaction and must not re-enter
    /// this database connection.
    pub fn plugin_update_json<T, F>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
        update: F,
    ) -> Result<Option<T>>
    where
        T: DeserializeOwned + Serialize,
        F: FnOnce(Option<T>) -> Result<Option<T>>,
    {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value_json = tx
            .query_row(
                "SELECT value_json FROM platform_plugin_kv
                 WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
                   AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
                params![
                    scope.plugin_id,
                    scope.platform,
                    scope.account_id,
                    scope.conversation_kind,
                    scope.conversation_id,
                    key,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let current = value_json
            .map(|value| serde_json::from_str(&value).context("invalid platform plugin JSON state"))
            .transpose()?;
        let current_json = current
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("failed to serialize platform plugin state")?;
        let next = update(current)?;
        let next_json = next
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("failed to serialize platform plugin state")?;
        if next_json == current_json {
            tx.commit()?;
            return Ok(next);
        }
        if let Some(value_json) = next_json {
            tx.execute(
                "INSERT INTO platform_plugin_kv (
                    plugin_id, platform, account_id, conversation_kind,
                    conversation_id, key, value_json, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (
                    plugin_id, platform, account_id, conversation_kind,
                    conversation_id, key
                 ) DO UPDATE SET
                    value_json = excluded.value_json,
                    updated_at = excluded.updated_at",
                params![
                    scope.plugin_id,
                    scope.platform,
                    scope.account_id,
                    scope.conversation_kind,
                    scope.conversation_id,
                    key,
                    value_json,
                    Utc::now().to_rfc3339(),
                ],
            )?;
        } else {
            tx.execute(
                "DELETE FROM platform_plugin_kv
                 WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
                   AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
                params![
                    scope.plugin_id,
                    scope.platform,
                    scope.account_id,
                    scope.conversation_kind,
                    scope.conversation_id,
                    key,
                ],
            )?;
        }
        tx.commit()?;
        Ok(next)
    }

    pub fn plugin_delete_key(&self, scope: &PlatformPluginScopeKey, key: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM platform_plugin_kv
             WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
               AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
            params![
                scope.plugin_id,
                scope.platform,
                scope.account_id,
                scope.conversation_kind,
                scope.conversation_id,
                key,
            ],
        )?;
        Ok(deleted != 0)
    }

    pub fn plugin_delete_scope(&self, scope: &PlatformPluginScopeKey) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM platform_plugin_kv
             WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
               AND conversation_kind = ?4 AND conversation_id = ?5",
            params![
                scope.plugin_id,
                scope.platform,
                scope.account_id,
                scope.conversation_kind,
                scope.conversation_id,
            ],
        )?)
    }

    pub fn put_platform_meme_ref(&self, record: &PlatformMemeRefRecord) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO platform_meme_refs (
                platform, account_id, conversation_kind, conversation_id,
                message_id, library, meme_id, direction, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (
                platform, account_id, conversation_kind, conversation_id,
                message_id, library, meme_id
             ) DO UPDATE SET
                direction = excluded.direction,
                created_at = excluded.created_at",
            params![
                record.platform,
                record.account_id,
                record.conversation_kind,
                record.conversation_id,
                record.message_id,
                record.library,
                record.meme_id,
                record.direction,
                record.created_at,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn platform_meme_refs_for_message(
        &self,
        platform: &str,
        account_id: &str,
        conversation_kind: &str,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Vec<PlatformMemeRefRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT platform, account_id, conversation_kind, conversation_id,
                    message_id, library, meme_id, direction, created_at
             FROM platform_meme_refs
             WHERE platform = ?1 AND account_id = ?2
               AND conversation_kind = ?3 AND conversation_id = ?4
               AND message_id = ?5
             ORDER BY created_at ASC, library ASC, meme_id ASC",
        )?;
        let records = stmt
            .query_map(
                params![
                    platform,
                    account_id,
                    conversation_kind,
                    conversation_id,
                    message_id
                ],
                |row| {
                    Ok(PlatformMemeRefRecord {
                        platform: row.get(0)?,
                        account_id: row.get(1)?,
                        conversation_kind: row.get(2)?,
                        conversation_id: row.get(3)?,
                        message_id: row.get(4)?,
                        library: row.get(5)?,
                        meme_id: row.get(6)?,
                        direction: row.get(7)?,
                        created_at: row.get(8)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn delete_platform_meme_ref(&self, library: &str, meme_id: &str) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = tx.execute(
            "DELETE FROM platform_meme_refs WHERE library = ?1 AND meme_id = ?2",
            params![library, meme_id],
        )?;
        tx.commit()?;
        Ok(deleted)
    }

    /// Records the model identity and token usage a subagent session actually
    /// used (audit columns on `sessions`).
    /// Writes a subagent row the way builds before v19 did: usage present,
    /// `cache_read_tokens` left NULL.
    #[cfg(test)]
    pub fn record_legacy_subagent_usage_for_test(
        &self,
        session_id: &str,
        prompt_tokens: i64,
        total_tokens: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET prompt_tokens = ?2, total_tokens = ?3,
                    cache_read_tokens = NULL
             WHERE session_id = ?1",
            params![session_id, prompt_tokens, total_tokens],
        )?;
        Ok(())
    }

    pub fn record_subagent_usage(
        &self,
        session_id: &str,
        provider_id: Option<&str>,
        model: Option<&str>,
        context_window: Option<i64>,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
        cache_read_tokens: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET provider_id = ?2, model = ?3, context_window = ?4,
                    prompt_tokens = ?5, completion_tokens = ?6, total_tokens = ?7,
                    updated_at = ?8, cache_read_tokens = ?9
             WHERE session_id = ?1",
            params![
                session_id,
                provider_id,
                model,
                context_window,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                Utc::now().to_rfc3339(),
                cache_read_tokens,
            ],
        )?;
        Ok(())
    }

    /// Deletes subagent audit sessions older than the retention window;
    /// their turns/images/queues cascade away.
    pub fn delete_subagent_sessions_older_than(&self, days: i64) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 与 delete_ask_sessions_older_than 同理:queued_prompts.session_id
        // 经 ALTER 而来没有级联外键,必须先手动清,否则留孤儿行。
        tx.execute(
            "DELETE FROM queued_prompts WHERE session_id IN (
                 SELECT session_id FROM sessions
                 WHERE kind = 'subagent'
                   AND datetime(updated_at) < datetime('now', '-' || ?1 || ' days'))",
            params![days],
        )?;
        let deleted = tx.execute(
            "DELETE FROM sessions
             WHERE kind = 'subagent'
               AND datetime(updated_at) < datetime('now', '-' || ?1 || ' days')",
            params![days],
        )?;
        tx.commit()?;
        Ok(deleted)
    }

    /// Deletes abandoned one-shot sessions older than the retention window. A
    /// `miyu ask` turn deletes its own session; anything still here was
    /// orphaned by a client that died mid-turn (Ctrl+C, SIGKILL).
    pub fn delete_ask_sessions_older_than(&self, hours: i64) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // queued_prompts.session_id arrived via ALTER and has no cascading FK,
        // so its rows have to go first (same reason as `delete_session`).
        tx.execute(
            "DELETE FROM queued_prompts WHERE session_id IN (
                 SELECT session_id FROM sessions
                 WHERE kind = ?1
                   AND datetime(updated_at) < datetime('now', '-' || ?2 || ' hours'))",
            params![super::ASK_SESSION_KIND, hours],
        )?;
        let deleted = tx.execute(
            "DELETE FROM sessions
             WHERE kind = ?1
               AND datetime(updated_at) < datetime('now', '-' || ?2 || ' hours')",
            params![super::ASK_SESSION_KIND, hours],
        )?;
        tx.commit()?;
        Ok(deleted)
    }

    fn update_session_field(
        &self,
        session_id: &str,
        field: &'static str,
        value: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            &format!("UPDATE sessions SET {field} = ?2, updated_at = ?3 WHERE session_id = ?1"),
            params![session_id, value, Utc::now().to_rfc3339()],
        )?;
        if updated == 0 {
            bail!("session not found: {session_id}");
        }
        Ok(())
    }

    pub fn start_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        user_content: &str,
        display_content: &str,
        owner_pid: u32,
        queue_session_id: &str,
        workspace: Option<&str>,
        attachment_run_id: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seq = self.next_seq_locked(&tx, session_id)?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO turns (turn_id, session_id, seq, user_content, display_content, user_timestamp, assistant_content, status, owner_pid, queue_session_id, workspace)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8, ?9, ?10)",
            params![
                turn_id,
                session_id,
                seq,
                user_content,
                display_content,
                now,
                PENDING_PLACEHOLDER,
                owner_pid as i64,
                queue_session_id,
                workspace
            ],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, 0, 0, 'running', ?2)",
            params![turn_id, now],
        )?;
        if let Some(run_id) = attachment_run_id {
            tx.execute(
                "UPDATE user_attachments SET run_id = NULL, turn_id = ?1
                 WHERE session_id = ?2 AND run_id = ?3",
                params![turn_id, session_id, run_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn insert_user_attachment(
        &self,
        session_id: &str,
        attachment: &UserAttachment,
        data: &[u8],
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO user_attachments
                (attachment_id, session_id, file_name, mime, kind, size_bytes,
                 width, height, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                attachment.attachment_id,
                session_id,
                attachment.file_name,
                attachment.mime,
                attachment.kind,
                attachment.size_bytes as i64,
                i64::from(attachment.width),
                i64::from(attachment.height),
                data,
                attachment.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn load_user_attachment(
        &self,
        session_id: &str,
        attachment_id: &str,
    ) -> Result<Option<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at, data
             FROM user_attachments WHERE session_id = ?1 AND attachment_id = ?2",
            params![session_id, attachment_id],
            |row| {
                Ok(UserAttachmentData {
                    attachment: map_user_attachment_row(row)?,
                    bytes: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn load_user_attachment_by_id(
        &self,
        attachment_id: &str,
    ) -> Result<Option<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at, data
             FROM user_attachments WHERE attachment_id = ?1",
            params![attachment_id],
            |row| {
                Ok(UserAttachmentData {
                    attachment: map_user_attachment_row(row)?,
                    bytes: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn load_user_attachment_data_for_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Vec<UserAttachmentData>> {
        self.load_bound_user_attachment_data(session_id, "turn_id", turn_id)
    }

    pub fn load_user_attachment_data_for_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
    ) -> Result<Vec<UserAttachmentData>> {
        self.load_bound_user_attachment_data(session_id, "prompt_id", prompt_id)
    }

    fn load_bound_user_attachment_data(
        &self,
        session_id: &str,
        field: &'static str,
        value: &str,
    ) -> Result<Vec<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at, data
             FROM user_attachments
             WHERE session_id = ?1 AND {field} = ?2
             ORDER BY created_at, attachment_id"
        ))?;
        let attachments = stmt
            .query_map(params![session_id, value], |row| {
                Ok(UserAttachmentData {
                    attachment: map_user_attachment_row(row)?,
                    bytes: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(attachments)
    }

    pub fn load_user_attachments(
        &self,
        session_id: &str,
        attachment_ids: &[String],
    ) -> Result<Vec<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        let mut attachments = Vec::with_capacity(attachment_ids.len());
        for attachment_id in attachment_ids {
            let attachment = conn
                .query_row(
                    "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                            height, created_at, data
                     FROM user_attachments
                     WHERE session_id = ?1 AND attachment_id = ?2
                       AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
                    params![session_id, attachment_id],
                    |row| {
                        Ok(UserAttachmentData {
                            attachment: map_user_attachment_row(row)?,
                            bytes: row.get(8)?,
                        })
                    },
                )
                .optional()?;
            let Some(attachment) = attachment else {
                bail!("attachment is unavailable: {attachment_id}");
            };
            attachments.push(attachment);
        }
        Ok(attachments)
    }

    pub fn reserve_user_attachments(
        &self,
        session_id: &str,
        attachment_ids: &[String],
        run_id: &str,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for attachment_id in attachment_ids {
            let affected = tx.execute(
                "UPDATE user_attachments SET run_id = ?1
                 WHERE session_id = ?2 AND attachment_id = ?3
                   AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
                params![run_id, session_id, attachment_id],
            )?;
            if affected != 1 {
                bail!("attachment changed before it could be submitted: {attachment_id}");
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn release_user_attachments_for_run(&self, run_id: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "UPDATE user_attachments SET run_id = NULL WHERE run_id = ?1",
            params![run_id],
        )?)
    }

    pub fn delete_staged_user_attachment(
        &self,
        session_id: &str,
        attachment_id: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM user_attachments
             WHERE session_id = ?1 AND attachment_id = ?2
               AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
            params![session_id, attachment_id],
        )? == 1)
    }

    pub fn purge_stale_user_attachments(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM user_attachments
             WHERE turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL
               AND datetime(created_at) < datetime('now', '-1 day')",
            [],
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append_turn_journal_event(
        &self,
        turn_id: &str,
        revision: i64,
        segment_index: i64,
        kind: &str,
        call_id: Option<&str>,
        name: Option<&str>,
        text_payload: Option<&str>,
        blob_payload: Option<&[u8]>,
        ok: Option<bool>,
    ) -> Result<()> {
        if text_payload.is_some_and(|payload| payload.len() > MAX_JOURNAL_TEXT_EVENT_BYTES) {
            bail!("turn journal text event exceeds the 64 MiB limit");
        }
        if blob_payload.is_some_and(|payload| payload.len() > MAX_JOURNAL_BLOB_EVENT_BYTES) {
            bail!("turn journal binary event exceeds the 8 MiB limit");
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let valid: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM turns t
                 INNER JOIN turn_journal_segments s
                   ON s.turn_id = t.turn_id AND s.revision = t.revision
                  AND s.segment_index = ?3
                 WHERE t.turn_id = ?1 AND t.revision = ?2
                   AND t.status = 'running' AND s.status != 'superseded'
             )",
            params![turn_id, revision, segment_index],
            |row| row.get(0),
        )?;
        if !valid {
            bail!("turn journal generation is no longer active");
        }
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, call_id, name,
                 text_payload, blob_payload, ok, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                turn_id,
                revision,
                segment_index,
                kind,
                call_id,
                name,
                text_payload,
                blob_payload,
                ok.map(i64::from),
                Utc::now().to_rfc3339(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn supersede_turn_journal_segment(
        &self,
        turn_id: &str,
        revision: i64,
        segment_index: i64,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let affected = tx.execute(
            "UPDATE turn_journal_segments
             SET status = 'superseded', finished_at = ?1
             WHERE turn_id = ?2 AND revision = ?3 AND segment_index = ?4
               AND status = 'running'",
            params![Utc::now().to_rfc3339(), turn_id, revision, segment_index],
        )?;
        if affected != 1 {
            bail!("turn journal segment changed before supersession");
        }
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, created_at)
             VALUES (?1, ?2, ?3, 'generation_superseded', ?4)",
            params![turn_id, revision, segment_index, Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn append_tool_reports(&self, turn_id: &str, reports: &[String]) -> Result<()> {
        if reports.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        let existing: String = conn.query_row(
            "SELECT tool_reports FROM turns WHERE turn_id = ?1",
            params![turn_id],
            |row| row.get(0),
        )?;
        let mut all: Vec<String> = serde_json::from_str(&existing).unwrap_or_default();
        all.extend(reports.iter().cloned());
        conn.execute(
            "UPDATE turns SET tool_reports = ?1 WHERE turn_id = ?2",
            params![serde_json::to_string(&all)?, turn_id],
        )?;
        Ok(())
    }

    /// Stores the fossilized transient tail for a turn (v7 append-only).
    pub fn set_turn_context_messages(
        &self,
        turn_id: &str,
        messages: &[ChatMessage],
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE turns SET context_messages = ?1 WHERE turn_id = ?2",
            params![serde_json::to_string(messages)?, turn_id],
        )?;
        Ok(())
    }

    /// 完成后落一次结构化工具流。独立 UPDATE 而非扩 complete 签名:调用点多,
    /// 且流为空(无工具回合)时根本不写。
    pub fn set_turn_tool_flow(&self, turn_id: &str, flow: &[ToolFlowRound]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE turns SET tool_flow = ?1 WHERE turn_id = ?2",
            params![serde_json::to_string(flow)?, turn_id],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn complete_turn(
        &self,
        turn_id: &str,
        content: &str,
        reasoning: Option<&str>,
    ) -> Result<()> {
        self.complete_turn_with_usage(
            turn_id,
            content,
            reasoning,
            None,
            None,
            TurnTokens::default(),
            false,
        )
    }

    pub fn complete_turn_with_usage(
        &self,
        turn_id: &str,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        let token_usage_estimated = i64::from(token_usage_estimated);
        let affected = tx.execute(
            "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                    assistant_provider_id = ?3, assistant_model = ?4, assistant_timestamp = ?5,
                    status = 'completed', token_total = ?6, token_usage_estimated = ?7,
                    token_prompt = ?9, token_cache_read = ?10
              WHERE turn_id = ?8 AND status = 'running'",
            params![
                content,
                reasoning,
                provider_id,
                model,
                now,
                tokens.total as i64,
                token_usage_estimated,
                turn_id,
                tokens.prompt as i64,
                tokens.cache_read as i64
            ],
        )?;
        if affected != 1 {
            bail!("turn changed before it could be completed");
        }
        bump_completion_seq_locked(&tx, turn_id)?;
        // Snapshot the display transcript before the journal goes: the tables
        // below are load-bearing for in-flight turn recovery, so they keep
        // being wiped on completion exactly as before.
        store_replay_journal(&tx, turn_id)?;
        tx.execute(
            "DELETE FROM turn_journal_segments WHERE turn_id = ?1",
            params![turn_id],
        )?;
        touch_session_last_request(&tx, turn_id)?;
        tx.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_turn_revision_with_usage(
        &self,
        turn_id: &str,
        revision: i64,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        let affected = tx.execute(
            "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                    assistant_provider_id = ?3, assistant_model = ?4, assistant_timestamp = ?5,
                    status = 'completed', token_total = ?6, token_usage_estimated = ?7,
                    token_prompt = ?10, token_cache_read = ?11
             WHERE turn_id = ?8 AND revision = ?9 AND status = 'running'",
            params![
                content,
                reasoning,
                provider_id,
                model,
                now,
                tokens.total as i64,
                i64::from(token_usage_estimated),
                turn_id,
                revision,
                tokens.prompt as i64,
                tokens.cache_read as i64
            ],
        )?;
        if affected != 1 {
            bail!("redo generation changed before it could be completed");
        }
        tx.execute(
            "DELETE FROM turn_redo_backups WHERE turn_id = ?1 AND revision = ?2",
            params![turn_id, revision],
        )?;
        // redo 重写了整个回合:先清掉旧修订的重放转写再按新修订快照,
        // 否则重开 REPL 仍显示被弃用的旧回复(空 journal 时也必须清)。
        tx.execute(
            "UPDATE turns SET replay_journal = NULL WHERE turn_id = ?1",
            params![turn_id],
        )?;
        store_replay_journal(&tx, turn_id)?;
        tx.execute(
            "DELETE FROM turn_journal_segments WHERE turn_id = ?1",
            params![turn_id],
        )?;
        touch_session_last_request(&tx, turn_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn interrupt_turn(&self, turn_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision: Option<i64> = tx
            .query_row(
                "SELECT revision FROM turns WHERE turn_id = ?1 AND status = 'running'",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(revision) = revision else {
            tx.commit()?;
            return Ok(());
        };
        let now = Utc::now().to_rfc3339();
        let (content, reasoning) = interrupted_projection_locked(&tx, turn_id, revision)?;
        tx.execute(
            "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                    assistant_timestamp = ?3, status = 'interrupted'
             WHERE turn_id = ?4 AND revision = ?5 AND status = 'running'",
            params![content, reasoning, now, turn_id, revision],
        )?;
        bump_completion_seq_locked(&tx, turn_id)?;
        tx.execute(
            "UPDATE turn_journal_segments
             SET status = 'interrupted', finished_at = ?1
             WHERE turn_id = ?2 AND revision = ?3 AND status = 'running'",
            params![now, turn_id, revision],
        )?;
        touch_session_last_request(&tx, turn_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn interrupt_turn_revision(&self, turn_id: &str, revision: i64) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let restored = restore_redo_backup_locked(&tx, turn_id, revision)?;
        if !restored {
            let (content, reasoning) = interrupted_projection_locked(&tx, turn_id, revision)?;
            let now = Utc::now().to_rfc3339();
            tx.execute(
                "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                        assistant_timestamp = ?3, status = 'interrupted'
                 WHERE turn_id = ?4 AND revision = ?5 AND status = 'running'",
                params![content, reasoning, now, turn_id, revision],
            )?;
            tx.execute(
                "UPDATE turn_journal_segments
                 SET status = 'interrupted', finished_at = ?1
                 WHERE turn_id = ?2 AND revision = ?3 AND status = 'running'",
                params![now, turn_id, revision],
            )?;
        }
        tx.commit()?;
        Ok(restored)
    }

    /// Unions `delta` into the turn's stored footprint. Read-modify-write is
    /// safe here: the turn is running and owned by exactly one process.
    pub fn merge_turn_footprint(&self, turn_id: &str, delta: &ToolFootprint) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        let existing: Option<Option<String>> = conn
            .query_row(
                "SELECT tool_footprint FROM turns WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(existing) = existing else {
            return Ok(());
        };
        let mut footprint = existing
            .as_deref()
            .and_then(|json| serde_json::from_str::<ToolFootprint>(json).ok())
            .unwrap_or_default();
        footprint.merge(delta.clone());
        conn.execute(
            "UPDATE turns SET tool_footprint = ?1 WHERE turn_id = ?2",
            params![serde_json::to_string(&footprint)?, turn_id],
        )?;
        Ok(())
    }

    /// Merged footprint across the given turns (summary rows included — they
    /// carry the accumulated footprint of everything they folded).
    pub fn load_merged_footprint(
        &self,
        session_id: &str,
        turn_ids: &[String],
    ) -> Result<ToolFootprint> {
        let conn = self.conn.lock().unwrap();
        let mut merged = ToolFootprint::default();
        let mut stmt = conn.prepare(
            "SELECT tool_footprint FROM turns WHERE session_id = ?1 AND turn_id = ?2",
        )?;
        for turn_id in turn_ids {
            let value: Option<Option<String>> = stmt
                .query_row(params![session_id, turn_id], |row| row.get(0))
                .optional()?;
            if let Some(Some(json)) = value {
                if let Ok(footprint) = serde_json::from_str::<ToolFootprint>(&json) {
                    merged.merge(footprint);
                }
            }
        }
        Ok(merged)
    }

    /// Unix seconds of this session's most recent completed/interrupted
    /// request write-point. None on legacy sessions (cold-resume prune skips).
    pub fn session_last_request_at(&self, session_id: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let value: Option<Option<i64>> = conn
            .query_row(
                "SELECT last_request_at FROM sessions WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.flatten())
    }

    pub fn append_tool_report(&self, turn_id: &str, report: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let existing: Option<String> = conn
            .query_row(
                "SELECT tool_reports FROM turns WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let mut reports: Vec<String> = existing
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        reports.push(report.to_string());
        let encoded = serde_json::to_string(&reports)?;
        conn.execute(
            "UPDATE turns SET tool_reports = ?1 WHERE turn_id = ?2",
            params![encoded, turn_id],
        )?;
        Ok(())
    }

    pub fn insert_image_asset(&self, asset: &ImageAsset, data: &[u8]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO image_assets
                (asset_id, turn_id, tool_id, mime, width, height, alt, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                asset.asset_id,
                asset.turn_id,
                asset.tool_id,
                asset.mime,
                i64::from(asset.width),
                i64::from(asset.height),
                asset.alt,
                data,
                asset.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn load_image_assets(&self, session_id: &str) -> Result<Vec<ImageAsset>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT a.asset_id, a.turn_id, a.tool_id, a.mime, a.width, a.height, a.alt, a.created_at
             FROM image_assets a
             INNER JOIN turns t ON t.turn_id = a.turn_id
             WHERE t.session_id = ?1
             ORDER BY a.turn_id ASC, a.created_at ASC, a.asset_id ASC",
        )?;
        let assets = stmt
            .query_map(params![session_id], map_image_asset_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(assets)
    }

    pub fn load_image_asset(&self, asset_id: &str) -> Result<Option<ImageAssetData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT asset_id, turn_id, tool_id, mime, width, height, alt, created_at, data
             FROM image_assets WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                Ok(ImageAssetData {
                    asset: map_image_asset_row(row)?,
                    bytes: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn upsert_artifact_asset(&self, asset: &ArtifactAsset, data: &[u8]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO artifact_assets
                (asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
                 size_bytes, data, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
              ON CONFLICT(turn_id, source_key) DO UPDATE SET
                tool_id = excluded.tool_id,
                file_name = excluded.file_name,
                mime = excluded.mime,
                kind = excluded.kind,
                size_bytes = excluded.size_bytes,
                data = excluded.data,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at
              ON CONFLICT(asset_id) DO UPDATE SET
                turn_id = excluded.turn_id,
                tool_id = excluded.tool_id,
                source_key = excluded.source_key,
                file_name = excluded.file_name,
                mime = excluded.mime,
                kind = excluded.kind,
                size_bytes = excluded.size_bytes,
                data = excluded.data,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at",
            params![
                asset.asset_id,
                asset.turn_id,
                asset.tool_id,
                asset.source_key,
                asset.file_name,
                asset.mime,
                asset.kind,
                asset.size_bytes as i64,
                data,
                asset.created_at,
                asset.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn load_artifact_assets(&self, session_id: &str) -> Result<Vec<ArtifactAsset>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT a.asset_id, a.turn_id, a.tool_id, a.source_key, a.file_name,
                    a.mime, a.kind, a.size_bytes, a.created_at, a.updated_at
             FROM artifact_assets a
             INNER JOIN turns t ON t.turn_id = a.turn_id
             WHERE t.session_id = ?1
             ORDER BY a.turn_id, a.updated_at, a.asset_id",
        )?;
        let assets = stmt
            .query_map(params![session_id], map_artifact_asset_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(assets)
    }

    pub fn load_artifact_asset(&self, asset_id: &str) -> Result<Option<ArtifactAssetData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
                    size_bytes, created_at, updated_at, data
             FROM artifact_assets WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                Ok(ArtifactAssetData {
                    asset: map_artifact_asset_row(row)?,
                    bytes: row.get(10)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn load_artifact_asset_data_for_turn(
        &self,
        turn_id: &str,
    ) -> Result<Vec<ArtifactAssetData>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
                    size_bytes, created_at, updated_at, data
             FROM artifact_assets WHERE turn_id = ?1 ORDER BY updated_at, asset_id",
        )?;
        let assets = stmt
            .query_map(params![turn_id], |row| {
                Ok(ArtifactAssetData {
                    asset: map_artifact_asset_row(row)?,
                    bytes: row.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(assets)
    }

    pub fn turn_session_id(&self, turn_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT session_id FROM turns WHERE turn_id = ?1",
            params![turn_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn append_question_exchange(
        &self,
        turn_id: &str,
        exchange: &QuestionExchange,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let next_index: i64 = conn.query_row(
            "SELECT COALESCE(MAX(exchange_index), -1) + 1
             FROM question_exchanges WHERE turn_id = ?1",
            params![turn_id],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO question_exchanges (turn_id, exchange_index, payload)
             VALUES (?1, ?2, ?3)",
            params![turn_id, next_index, serde_json::to_string(exchange)?],
        )?;
        Ok(())
    }

    pub fn enqueue_prompt(
        &self,
        session_id: &str,
        target_turn_id: Option<&str>,
        prompt_id: &str,
        content: &str,
        display_content: &str,
        attachments: &[QueuedPromptAttachment],
        uploaded_attachment_ids: &[String],
        queue_session_id: &str,
        owner_pid: u32,
    ) -> Result<QueuedPrompt> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target_running: bool = match target_turn_id {
            Some(turn_id) => tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM turns
                     WHERE session_id = ?1 AND turn_id = ?2 AND status = 'running'
                       AND queue_session_id = ?3 AND owner_pid = ?4
                 )",
                params![session_id, turn_id, queue_session_id, owner_pid as i64],
                |row| row.get(0),
            )?,
            None => true,
        };
        if !target_running {
            bail!("the target turn is no longer accepting follow-up messages");
        }
        let submitted_at = Utc::now().to_rfc3339();
        let attachments_json = serde_json::to_string(attachments)?;
        tx.execute(
            "INSERT INTO queued_prompts
                (session_id, prompt_id, content, display_content, attachments, status, submitted_at,
                 queue_session_id, owner_pid)
             VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?8)",
            params![
                session_id,
                prompt_id,
                content,
                display_content,
                attachments_json,
                submitted_at,
                queue_session_id,
                owner_pid as i64
            ],
        )?;
        let seq = tx.last_insert_rowid();
        for attachment_id in uploaded_attachment_ids {
            let affected = tx.execute(
                "UPDATE user_attachments SET prompt_id = ?1
                 WHERE session_id = ?2 AND attachment_id = ?3
                   AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
                params![prompt_id, session_id, attachment_id],
            )?;
            if affected != 1 {
                bail!("attachment changed before it could be queued: {attachment_id}");
            }
        }
        tx.commit()?;
        drop(conn);
        let uploaded_attachments = self.user_attachments_for_prompt(prompt_id)?;
        Ok(QueuedPrompt {
            prompt_id: prompt_id.to_string(),
            seq,
            content: content.to_string(),
            display_content: display_content.to_string(),
            attachments: attachments.to_vec(),
            uploaded_attachments,
            submitted_at,
        })
    }

    pub fn load_queued_prompts(
        &self,
        session_id: &str,
        queue_session_id: &str,
    ) -> Result<Vec<QueuedPrompt>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT prompt_id, seq, content, display_content, attachments, submitted_at
             FROM queued_prompts
             WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2
             ORDER BY seq ASC",
        )?;
        let mut prompts = stmt
            .query_map(params![session_id, queue_session_id], |row| {
                let attachments_json: String = row.get(4)?;
                let attachments = serde_json::from_str(&attachments_json).unwrap_or_default();
                Ok(QueuedPrompt {
                    prompt_id: row.get(0)?,
                    seq: row.get(1)?,
                    content: row.get(2)?,
                    display_content: row.get(3)?,
                    attachments,
                    uploaded_attachments: Vec::new(),
                    submitted_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_prompt_attachments_locked(&conn, &mut prompts)?;
        Ok(prompts)
    }

    fn user_attachments_for_prompt(&self, prompt_id: &str) -> Result<Vec<UserAttachment>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at FROM user_attachments
             WHERE prompt_id = ?1 ORDER BY created_at, attachment_id",
        )?;
        let attachments = stmt
            .query_map(params![prompt_id], map_user_attachment_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(attachments)
    }

    pub fn consume_queued_prompts(
        &self,
        session_id: &str,
        turn_id: &str,
        prompts: &[(String, String)],
        preceding_assistant_content: Option<&str>,
        preceding_assistant_reasoning: Option<&str>,
        preceding_assistant_provider_id: Option<&str>,
        preceding_assistant_model: Option<&str>,
        queue_session_id: &str,
    ) -> Result<()> {
        self.consume_queued_prompts_with_checkpoint(
            session_id,
            turn_id,
            prompts,
            preceding_assistant_content,
            preceding_assistant_reasoning,
            preceding_assistant_provider_id,
            preceding_assistant_model,
            queue_session_id,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn consume_queued_prompts_with_checkpoint(
        &self,
        session_id: &str,
        turn_id: &str,
        prompts: &[(String, String)],
        preceding_assistant_content: Option<&str>,
        preceding_assistant_reasoning: Option<&str>,
        preceding_assistant_provider_id: Option<&str>,
        preceding_assistant_model: Option<&str>,
        queue_session_id: &str,
        mut checkpoint: Option<TurnRedoCheckpointPayload>,
    ) -> Result<()> {
        if prompts.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE turn_id = ?1 AND status = 'running')",
            params![turn_id],
            |row| row.get(0),
        )?;
        if !running {
            bail!("cannot consume queued prompts into a non-running turn");
        }
        if let Some(checkpoint) = checkpoint.as_mut() {
            checkpoint.prefix_question_count = tx.query_row(
                "SELECT COUNT(*) FROM question_exchanges WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get::<_, i64>(0),
            )? as usize;
            checkpoint.prefix_image_asset_ids = {
                let mut stmt = tx.prepare(
                    "SELECT asset_id FROM image_assets WHERE turn_id = ?1 ORDER BY created_at, asset_id",
                )?;
                let rows = stmt
                    .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
            checkpoint.prefix_artifact_asset_ids = {
                let mut stmt = tx.prepare(
                    "SELECT asset_id FROM artifact_assets
                     WHERE turn_id = ?1 ORDER BY updated_at, asset_id",
                )?;
                let rows = stmt
                    .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
            checkpoint.loaded_items = {
                let mut stmt = tx.prepare(
                    "SELECT kind, name, source_turn_id FROM session_loaded_items
                     WHERE session_id = ?1 ORDER BY kind, name",
                )?;
                let rows = stmt
                    .query_map(params![session_id], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
        }
        let consumed_at = Utc::now().to_rfc3339();
        for (index, (prompt_id, context_content)) in prompts.iter().enumerate() {
            let preceding_content = (index == 0)
                .then_some(preceding_assistant_content)
                .flatten();
            let preceding_reasoning = (index == 0)
                .then_some(preceding_assistant_reasoning)
                .flatten();
            let affected = tx.execute(
                "UPDATE queued_prompts
                  SET status = 'consumed', consumed_at = ?1, turn_id = ?2,
                      context_content = ?3, preceding_assistant_content = ?4,
                      preceding_assistant_reasoning = ?5,
                      preceding_assistant_provider_id = ?6,
                      preceding_assistant_model = ?7
                   WHERE prompt_id = ?8 AND status = 'queued' AND session_id = ?9
                     AND queue_session_id = ?10",
                params![
                    consumed_at,
                    turn_id,
                    context_content,
                    preceding_content,
                    preceding_reasoning,
                    preceding_assistant_provider_id,
                    preceding_assistant_model,
                    prompt_id,
                    session_id,
                    queue_session_id
                ],
            )?;
            if affected != 1 {
                bail!("queued prompt changed before it could be consumed: {prompt_id}");
            }
        }
        let batch_prompt_ids = prompts
            .iter()
            .map(|(prompt_id, _)| prompt_id.as_str())
            .collect::<Vec<_>>();
        let batch_prompt_ids = serde_json::to_string(&batch_prompt_ids)?;
        let (payload, unavailable_reason) = match checkpoint {
            Some(checkpoint) => {
                let payload = serde_json::to_vec(&checkpoint)?;
                if payload.len() <= MAX_REDO_CHECKPOINT_BYTES {
                    (Some(payload), None)
                } else {
                    (
                        None,
                        Some(format!(
                            "replay checkpoint exceeds the {} byte limit",
                            MAX_REDO_CHECKPOINT_BYTES
                        )),
                    )
                }
            }
            None => (None, Some("replay checkpoint was not captured".to_string())),
        };
        tx.execute(
            "INSERT INTO turn_redo_checkpoints
                (turn_id, version, batch_prompt_ids, payload, unavailable_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(turn_id) DO UPDATE SET
                version = excluded.version,
                batch_prompt_ids = excluded.batch_prompt_ids,
                payload = excluded.payload,
                unavailable_reason = excluded.unavailable_reason,
                created_at = excluded.created_at",
            params![
                turn_id,
                REDO_CHECKPOINT_VERSION,
                batch_prompt_ids,
                payload,
                unavailable_reason,
                consumed_at
            ],
        )?;
        let revision: i64 = tx.query_row(
            "SELECT revision FROM turns WHERE turn_id = ?1 AND status = 'running'",
            params![turn_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, ?2, 0, 'running', ?3)",
            params![turn_id, revision, consumed_at],
        )?;
        let (segment_index, segment_status): (i64, String) = tx.query_row(
            "SELECT segment_index, status FROM turn_journal_segments
             WHERE turn_id = ?1 AND revision = ?2
             ORDER BY segment_index DESC LIMIT 1",
            params![turn_id, revision],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let next_segment = segment_index.saturating_add(1);
        let prompt_payload =
            serde_json::to_string(&prompts.iter().map(|(id, _)| id).collect::<Vec<_>>())?;
        if segment_status == "superseded" {
            tx.execute(
                "INSERT INTO turn_journal_segments
                    (turn_id, revision, segment_index, status, started_at)
                 VALUES (?1, ?2, ?3, 'running', ?4)",
                params![turn_id, revision, next_segment, consumed_at],
            )?;
            tx.execute(
                "INSERT INTO turn_journal_events
                    (turn_id, revision, segment_index, kind, text_payload, created_at)
                 VALUES (?1, ?2, ?3, 'queued_prompts_consumed', ?4, ?5)",
                params![turn_id, revision, next_segment, prompt_payload, consumed_at],
            )?;
        } else {
            tx.execute(
                "INSERT INTO turn_journal_events
                    (turn_id, revision, segment_index, kind, text_payload, created_at)
                 VALUES (?1, ?2, ?3, 'queued_prompts_consumed', ?4, ?5)",
                params![
                    turn_id,
                    revision,
                    segment_index,
                    prompt_payload,
                    consumed_at
                ],
            )?;
        }
        if segment_status == "running" {
            tx.execute(
                "UPDATE turn_journal_segments
                 SET status = 'completed', finished_at = ?1
                 WHERE turn_id = ?2 AND revision = ?3 AND segment_index = ?4",
                params![consumed_at, turn_id, revision, segment_index],
            )?;
        }
        if segment_status != "superseded" {
            tx.execute(
                "INSERT INTO turn_journal_segments
                    (turn_id, revision, segment_index, status, started_at)
                 VALUES (?1, ?2, ?3, 'running', ?4)",
                params![turn_id, revision, next_segment, consumed_at],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn redo_candidate(&self, session_id: &str) -> Result<Option<RedoCandidate>> {
        let conn = self.conn.lock().unwrap();
        let last = conn
            .query_row(
                "SELECT turn_id, revision, display_content, status
                 FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                 ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((turn_id, revision, display_content, status)) = last else {
            return Ok(None);
        };
        if status == "running" {
            return Ok(None);
        }

        let consumed = {
            let mut stmt = conn.prepare(
                "SELECT prompt_id, display_content
                 FROM queued_prompts
                 WHERE turn_id = ?1 AND status = 'consumed'
                 ORDER BY seq ASC",
            )?;
            let rows = stmt
                .query_map(params![turn_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        if consumed.is_empty() {
            return Ok(Some(RedoCandidate {
                input_id: turn_id.clone(),
                turn_id,
                revision,
                input_kind: RedoInputKind::Initial,
                display_content,
                batch_prompt_ids: Vec::new(),
            }));
        }

        let checkpoint = load_redo_checkpoint_locked(&conn, &turn_id)?;
        let Some(checkpoint) = checkpoint.filter(|checkpoint| checkpoint.payload.is_some()) else {
            return Ok(None);
        };
        if checkpoint.batch_prompt_ids.is_empty()
            || checkpoint.batch_prompt_ids.len() > consumed.len()
        {
            return Ok(None);
        }
        let suffix = &consumed[consumed.len() - checkpoint.batch_prompt_ids.len()..];
        if !suffix
            .iter()
            .map(|(prompt_id, _)| prompt_id)
            .eq(checkpoint.batch_prompt_ids.iter())
        {
            return Ok(None);
        }
        let (input_id, display_content) = suffix.last().cloned().expect("non-empty suffix");
        Ok(Some(RedoCandidate {
            turn_id,
            revision,
            input_id,
            input_kind: RedoInputKind::Followup,
            display_content,
            batch_prompt_ids: checkpoint.batch_prompt_ids,
        }))
    }

    pub fn load_redo_batch_prompts(
        &self,
        session_id: &str,
        turn_id: &str,
        prompt_ids: &[String],
    ) -> Result<Vec<QueuedPrompt>> {
        if prompt_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT prompt_id, seq, COALESCE(context_content, content), display_content,
                    attachments, submitted_at
             FROM queued_prompts
             WHERE session_id = ?1 AND turn_id = ?2 AND status = 'consumed'
             ORDER BY seq ASC",
        )?;
        let mut prompts = stmt
            .query_map(params![session_id, turn_id], |row| {
                Ok(QueuedPrompt {
                    prompt_id: row.get(0)?,
                    seq: row.get(1)?,
                    content: row.get(2)?,
                    display_content: row.get(3)?,
                    attachments: serde_json::from_str(&row.get::<_, String>(4)?)
                        .unwrap_or_default(),
                    uploaded_attachments: Vec::new(),
                    submitted_at: row.get(5)?,
                })
            })?
            .filter_map(|row| match row {
                Ok(prompt) if prompt_ids.contains(&prompt.prompt_id) => Some(Ok(prompt)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        if prompts.len() != prompt_ids.len()
            || !prompts
                .iter()
                .map(|prompt| &prompt.prompt_id)
                .eq(prompt_ids.iter())
        {
            bail!("redo follow-up batch changed before it could be loaded");
        }
        attach_prompt_attachments_locked(&conn, &mut prompts)?;
        Ok(prompts)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn begin_redo(
        &self,
        session_id: &str,
        turn_id: &str,
        input_id: &str,
        input_kind: RedoInputKind,
        expected_revision: i64,
        content: &str,
        display_content: &str,
        owner_pid: u32,
        queue_session_id: &str,
    ) -> Result<RedoStart> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest = tx
            .query_row(
                "SELECT turn_id, revision, status
                 FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                 ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((latest_turn_id, revision, status)) = latest else {
            bail!("redo target no longer exists");
        };
        if latest_turn_id != turn_id || revision != expected_revision || status == "running" {
            bail!("conversation changed before redo could start");
        }
        let other_running: i64 = tx.query_row(
            "SELECT COUNT(*) FROM turns
             WHERE session_id = ?1 AND status = 'running' AND turn_id != ?2",
            params![session_id, turn_id],
            |row| row.get(0),
        )?;
        if other_running != 0 {
            bail!("another turn is already running in this conversation");
        }

        let (
            user_content,
            old_display_content,
            assistant_content,
            assistant_reasoning,
            assistant_provider_id,
            assistant_model,
            assistant_timestamp,
            tool_reports,
            old_owner_pid,
            old_queue_session_id,
            token_total,
            token_usage_estimated,
            token_prompt,
            token_cache_read,
        ) = tx.query_row(
            "SELECT user_content, display_content, assistant_content, assistant_reasoning,
                    assistant_provider_id, assistant_model, assistant_timestamp, tool_reports,
                    owner_pid, queue_session_id, token_total, token_usage_estimated,
                    token_prompt, token_cache_read
             FROM turns WHERE turn_id = ?1",
            params![turn_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                ))
            },
        )?;
        let followup = if input_kind == RedoInputKind::Followup {
            tx.query_row(
                "SELECT content, display_content, context_content
                 FROM queued_prompts
                 WHERE prompt_id = ?1 AND turn_id = ?2 AND status = 'consumed'",
                params![input_id, turn_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
        } else {
            None
        };
        let loaded_items = {
            let mut stmt = tx.prepare(
                "SELECT kind, name, source_turn_id, created_at, updated_at
                 FROM session_loaded_items WHERE session_id = ?1 ORDER BY kind, name",
            )?;
            let rows = stmt
                .query_map(params![session_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let consumed_prompt_ids = {
            let mut stmt = tx.prepare(
                "SELECT prompt_id FROM queued_prompts
                 WHERE turn_id = ?1 AND status = 'consumed' ORDER BY seq",
            )?;
            let rows = stmt
                .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let checkpoint_backup = tx
            .query_row(
                "SELECT version, batch_prompt_ids, payload, unavailable_reason, created_at
                 FROM turn_redo_checkpoints WHERE turn_id = ?1",
                params![turn_id],
                |row| {
                    Ok(RedoCheckpointBackup {
                        version: row.get(0)?,
                        batch_prompt_ids: row.get(1)?,
                        payload: row.get(2)?,
                        unavailable_reason: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()?;
        let backup = TurnRedoBackup {
            status,
            user_content,
            display_content: old_display_content,
            followup_content: followup.as_ref().map(|value| value.0.clone()),
            followup_display_content: followup.as_ref().map(|value| value.1.clone()),
            followup_context_content: followup.and_then(|value| value.2),
            assistant_content,
            assistant_reasoning,
            assistant_provider_id,
            assistant_model,
            assistant_timestamp,
            tool_reports,
            owner_pid: old_owner_pid,
            queue_session_id: old_queue_session_id,
            token_total,
            token_prompt,
            token_cache_read,
            token_usage_estimated,
            loaded_items,
            consumed_prompt_ids,
            checkpoint: checkpoint_backup,
        };
        let backup_payload = serde_json::to_vec(&backup)?;
        let redo_revision = expected_revision.saturating_add(1);
        let backup_created_at = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO turn_redo_backups (turn_id, revision, payload, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![turn_id, redo_revision, backup_payload, backup_created_at],
        )?;
        tx.execute(
            "INSERT INTO turn_redo_question_backups (turn_id, exchange_index, payload)
             SELECT turn_id, exchange_index, payload FROM question_exchanges WHERE turn_id = ?1",
            params![turn_id],
        )?;
        tx.execute(
            "INSERT INTO turn_redo_image_backups
                (turn_id, asset_id, tool_id, mime, width, height, alt, data, created_at)
             SELECT turn_id, asset_id, tool_id, mime, width, height, alt, data, created_at
             FROM image_assets WHERE turn_id = ?1",
            params![turn_id],
        )?;
        tx.execute(
            "INSERT INTO turn_redo_artifact_backups
                (turn_id, asset_id, tool_id, source_key, file_name, mime, kind,
                 size_bytes, data, created_at, updated_at)
             SELECT ?1, asset_id, tool_id, source_key, file_name, mime, kind,
                    size_bytes, data, created_at, updated_at
             FROM artifact_assets WHERE turn_id = ?1",
            params![turn_id],
        )?;

        let checkpoint = match input_kind {
            RedoInputKind::Initial => {
                if input_id != turn_id {
                    bail!("redo input no longer matches the initial prompt");
                }
                let followups: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM queued_prompts
                     WHERE turn_id = ?1 AND status = 'consumed'",
                    params![turn_id],
                    |row| row.get(0),
                )?;
                if followups != 0 {
                    bail!("the last input changed before redo could start");
                }
                tx.execute(
                    "DELETE FROM question_exchanges WHERE turn_id = ?1",
                    params![turn_id],
                )?;
                tx.execute(
                    "DELETE FROM image_assets WHERE turn_id = ?1",
                    params![turn_id],
                )?;
                tx.execute(
                    "DELETE FROM artifact_assets WHERE turn_id = ?1",
                    params![turn_id],
                )?;
                tx.execute(
                    "DELETE FROM session_loaded_items
                     WHERE session_id = ?1 AND source_turn_id = ?2",
                    params![session_id, turn_id],
                )?;
                tx.execute(
                    "UPDATE turns SET user_content = ?1, display_content = ?2
                     WHERE turn_id = ?3",
                    params![content, display_content, turn_id],
                )?;
                None
            }
            RedoInputKind::Followup => {
                let checkpoint = load_redo_checkpoint_locked(&tx, turn_id)?
                    .and_then(|checkpoint| checkpoint.payload)
                    .context("redo checkpoint is unavailable")?;
                let row = tx
                    .query_row(
                        "SELECT prompt_id FROM queued_prompts
                         WHERE turn_id = ?1 AND status = 'consumed'
                         ORDER BY seq DESC LIMIT 1",
                        params![turn_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if row.as_deref() != Some(input_id) {
                    bail!("the last follow-up changed before redo could start");
                }
                tx.execute(
                    "DELETE FROM question_exchanges
                     WHERE turn_id = ?1 AND exchange_index >= ?2",
                    params![turn_id, checkpoint.prefix_question_count as i64],
                )?;
                let prefix_assets = checkpoint
                    .prefix_image_asset_ids
                    .iter()
                    .collect::<std::collections::HashSet<_>>();
                let current_assets = {
                    let mut stmt =
                        tx.prepare("SELECT asset_id FROM image_assets WHERE turn_id = ?1")?;
                    let rows = stmt
                        .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    rows
                };
                for asset_id in current_assets {
                    if !prefix_assets.contains(&asset_id) {
                        tx.execute(
                            "DELETE FROM image_assets WHERE asset_id = ?1",
                            params![asset_id],
                        )?;
                    }
                }
                let prefix_artifacts = checkpoint
                    .prefix_artifact_asset_ids
                    .iter()
                    .collect::<std::collections::HashSet<_>>();
                let current_artifacts = {
                    let mut stmt =
                        tx.prepare("SELECT asset_id FROM artifact_assets WHERE turn_id = ?1")?;
                    let rows = stmt
                        .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    rows
                };
                for asset_id in current_artifacts {
                    if !prefix_artifacts.contains(&asset_id) {
                        tx.execute(
                            "DELETE FROM artifact_assets WHERE asset_id = ?1",
                            params![asset_id],
                        )?;
                    }
                }
                tx.execute(
                    "DELETE FROM session_loaded_items WHERE session_id = ?1",
                    params![session_id],
                )?;
                let now = Utc::now().to_rfc3339();
                for (kind, name, source_turn_id) in &checkpoint.loaded_items {
                    tx.execute(
                        "INSERT INTO session_loaded_items
                            (session_id, kind, name, source_turn_id, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                        params![session_id, kind, name, source_turn_id, now],
                    )?;
                }
                tx.execute(
                    "UPDATE queued_prompts
                     SET content = ?1, display_content = ?2, context_content = ?1
                     WHERE prompt_id = ?3 AND turn_id = ?4 AND status = 'consumed'",
                    params![content, display_content, input_id, turn_id],
                )?;
                Some(checkpoint)
            }
        };

        let prefix_reports = checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.prefix_tool_reports.as_slice())
            .unwrap_or_default();
        let prefix_reports = serde_json::to_string(prefix_reports)?;
        let prefix_question_count = checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.prefix_question_count)
            .unwrap_or(0);
        let now = Utc::now().to_rfc3339();
        let affected = tx.execute(
            "UPDATE turns SET
                assistant_content = ?1,
                assistant_reasoning = NULL,
                assistant_provider_id = NULL,
                assistant_model = NULL,
                assistant_timestamp = NULL,
                status = 'running',
                tool_reports = ?2,
                owner_pid = ?3,
                queue_session_id = ?4,
                token_total = 0,
                token_usage_estimated = 0,
                revision = revision + 1
             WHERE turn_id = ?5 AND session_id = ?6 AND revision = ?7 AND status != 'running'",
            params![
                PENDING_PLACEHOLDER,
                prefix_reports,
                owner_pid as i64,
                queue_session_id,
                turn_id,
                session_id,
                expected_revision
            ],
        )?;
        if affected != 1 {
            bail!("conversation changed before redo could be claimed");
        }
        tx.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE session_id = ?1",
            params![session_id, now],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, ?2, 0, 'running', ?3)",
            params![turn_id, redo_revision, now],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, text_payload, created_at)
             VALUES (?1, ?2, 0, 'redo_prefix_question_count', ?3, ?4)",
            params![
                turn_id,
                redo_revision,
                prefix_question_count.to_string(),
                now
            ],
        )?;
        tx.commit()?;
        Ok(RedoStart {
            revision: expected_revision.saturating_add(1),
            checkpoint,
        })
    }

    pub fn discard_queued_prompts(
        &self,
        session_id: &str,
        queue_session_id: &str,
    ) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let turn = tx
            .query_row(
                "SELECT turn_id, status, revision, assistant_content, assistant_reasoning
                 FROM turns
                 WHERE session_id = ?1 AND queue_session_id = ?2
                 ORDER BY seq DESC LIMIT 1",
                params![session_id, queue_session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((turn_id, status, revision, assistant_content, assistant_reasoning)) = turn else {
            let deleted = tx.execute(
                "DELETE FROM queued_prompts
                 WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2",
                params![session_id, queue_session_id],
            )?;
            tx.commit()?;
            return Ok(deleted);
        };
        if status == "running" {
            let deleted = tx.execute(
                "DELETE FROM queued_prompts
                 WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2",
                params![session_id, queue_session_id],
            )?;
            tx.commit()?;
            return Ok(deleted);
        }

        let now = Utc::now().to_rfc3339();
        let preceding_content = if status == "interrupted" {
            interrupted_prefix(&assistant_content)
        } else {
            assistant_content
        };
        let preceding_content = (!preceding_content.trim().is_empty()).then_some(preceding_content);
        let mut stmt = tx.prepare(
            "SELECT prompt_id FROM queued_prompts
             WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2
             ORDER BY seq",
        )?;
        let prompt_ids = stmt
            .query_map(params![session_id, queue_session_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        for (index, prompt_id) in prompt_ids.iter().enumerate() {
            tx.execute(
                "UPDATE queued_prompts
                 SET status = 'consumed', consumed_at = ?1, turn_id = ?2,
                     context_content = content,
                     preceding_assistant_content = ?3,
                     preceding_assistant_reasoning = ?4
                 WHERE prompt_id = ?5 AND status = 'queued'",
                params![
                    now,
                    turn_id,
                    (index == 0)
                        .then_some(preceding_content.as_deref())
                        .flatten(),
                    (index == 0)
                        .then_some(assistant_reasoning.as_deref())
                        .flatten(),
                    prompt_id,
                ],
            )?;
        }
        if status == "interrupted" && !prompt_ids.is_empty() {
            let next_segment: i64 = tx.query_row(
                "SELECT COALESCE(MAX(segment_index), -1) + 1
                 FROM turn_journal_segments WHERE turn_id = ?1 AND revision = ?2",
                params![turn_id, revision],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO turn_journal_segments
                    (turn_id, revision, segment_index, status, started_at, finished_at)
                 VALUES (?1, ?2, ?3, 'interrupted', ?4, ?4)",
                params![turn_id, revision, next_segment, now],
            )?;
            tx.execute(
                "INSERT INTO turn_journal_events
                    (turn_id, revision, segment_index, kind, text_payload, created_at)
                 VALUES (?1, ?2, ?3, 'queued_prompts_consumed', ?4, ?5)",
                params![
                    turn_id,
                    revision,
                    next_segment,
                    serde_json::to_string(&prompt_ids)?,
                    now
                ],
            )?;
        }
        tx.commit()?;
        Ok(prompt_ids.len())
    }

    pub fn remove_queued_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
        queue_session_id: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM queued_prompts
             WHERE prompt_id = ?1 AND status = 'queued' AND session_id = ?2
               AND queue_session_id = ?3",
            params![prompt_id, session_id, queue_session_id],
        )? == 1)
    }

    /// Hard-drop every still-queued prompt of a queue session and return
    /// their ids. Unlike `discard_queued_prompts` this never folds prompts
    /// into the conversation: it backs an explicit user cancel, where the
    /// queued follow-ups are withdrawn rather than preserved as context.
    pub fn delete_queued_prompts(
        &self,
        session_id: &str,
        queue_session_id: &str,
    ) -> Result<Vec<String>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prompt_ids = {
            let mut stmt = tx.prepare(
                "SELECT prompt_id FROM queued_prompts
                 WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2
                 ORDER BY seq",
            )?;
            let prompt_ids = stmt
                .query_map(params![session_id, queue_session_id], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            prompt_ids
        };
        if !prompt_ids.is_empty() {
            tx.execute(
                "DELETE FROM queued_prompts
                 WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2",
                params![session_id, queue_session_id],
            )?;
        }
        tx.commit()?;
        Ok(prompt_ids)
    }

    pub fn discard_stale_queued_prompts(
        &self,
        current_session_id: &str,
        _current_pid: u32,
    ) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT q.prompt_id, q.queue_session_id, q.owner_pid,
                    EXISTS(
                        SELECT 1 FROM turns t
                        WHERE t.status = 'running'
                          AND t.queue_session_id = q.queue_session_id
                    )
             FROM queued_prompts q WHERE q.status = 'queued'",
        )?;
        let queued_prompts = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let stale_prompt_ids = queued_prompts
            .into_iter()
            .filter_map(|row| {
                let (prompt_id, session_id, owner_pid, belongs_to_running_turn) = row;
                if session_id.as_deref() == Some(current_session_id) {
                    return None;
                }
                if belongs_to_running_turn {
                    return None;
                }
                let owner_pid = owner_pid.and_then(|pid| u32::try_from(pid).ok());
                // Multiple stores in the daemon share a PID. A different
                // queue identity owned by this live process may belong to an
                // active parent turn, so only dead owners are stale here.
                let stale =
                    session_id.is_none() || !owner_pid.is_some_and(crate::alarm::process_exists);
                stale.then_some(prompt_id)
            })
            .collect::<Vec<_>>();
        drop(stmt);
        if stale_prompt_ids.is_empty() {
            return Ok(0);
        }
        let tx = conn.transaction()?;
        let mut discarded = 0usize;
        for prompt_id in stale_prompt_ids {
            discarded += tx.execute(
                "DELETE FROM queued_prompts WHERE prompt_id = ?1 AND status = 'queued'",
                params![prompt_id],
            )?;
        }
        tx.commit()?;
        Ok(discarded)
    }

    pub fn load_session_loaded_items(
        &self,
        session_id: &str,
        kind: &str,
    ) -> Result<std::collections::BTreeSet<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name FROM session_loaded_items
             WHERE session_id = ?1 AND kind = ?2 ORDER BY name ASC",
        )?;
        let items = stmt
            .query_map(params![session_id, kind], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<std::collections::BTreeSet<_>, _>>()?;
        Ok(items)
    }

    pub fn load_session_loaded_items_with_sources(
        &self,
        session_id: &str,
        kind: &str,
    ) -> Result<Vec<(String, Option<String>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, source_turn_id FROM session_loaded_items
             WHERE session_id = ?1 AND kind = ?2 ORDER BY name ASC",
        )?;
        let items = stmt
            .query_map(params![session_id, kind], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn add_session_loaded_items(
        &self,
        session_id: &str,
        kind: &str,
        names: &[String],
        source_turn_id: Option<&str>,
    ) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        let mut affected = 0usize;
        for name in names
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
        {
            affected += conn.execute(
                "INSERT INTO session_loaded_items (session_id, kind, name, source_turn_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(session_id, kind, name) DO UPDATE SET
                    source_turn_id = COALESCE(excluded.source_turn_id, session_loaded_items.source_turn_id),
                    updated_at = excluded.updated_at",
                params![session_id, kind, name, source_turn_id, now],
            )?;
        }
        Ok(affected)
    }

    pub fn load_turns(&self, session_id: &str) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 ORDER BY seq ASC",
        )?;
        let mut turns = stmt
            .query_map(params![session_id], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_turn_children_locked(&conn, &mut turns)?;
        Ok(turns)
    }

    #[allow(dead_code)]
    pub fn load_turns_excluding(
        &self,
        session_id: &str,
        exclude_turn_id: &str,
    ) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 AND turn_id != ?2 ORDER BY seq ASC",
        )?;
        let mut turns = stmt
            .query_map(params![session_id, exclude_turn_id], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_turn_children_locked(&conn, &mut turns)?;
        Ok(turns)
    }

    #[allow(dead_code)]
    pub fn load_turns_for_context(&self, session_id: &str) -> Result<Vec<Turn>> {
        self.load_turns(session_id)
    }

    pub fn load_visible_turns(&self, session_id: &str) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 AND hidden = 0 ORDER BY seq ASC",
        )?;
        let mut turns = stmt
            .query_map(params![session_id], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_turn_children_locked(&conn, &mut turns)?;
        Ok(turns)
    }

    pub fn load_visible_turns_excluding(
        &self,
        session_id: &str,
        exclude_turn_id: &str,
    ) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 AND hidden = 0 AND turn_id != ?2 ORDER BY seq ASC",
        )?;
        let mut turns = stmt
            .query_map(params![session_id, exclude_turn_id], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_turn_children_locked(&conn, &mut turns)?;
        Ok(turns)
    }

    #[allow(dead_code)]
    pub fn hide_turns_before_seq(&self, session_id: &str, seq: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let affected = conn.execute(
            "UPDATE turns SET hidden = 1 WHERE session_id = ?1 AND seq <= ?2",
            params![session_id, seq],
        )?;
        Ok(affected)
    }

    #[allow(dead_code)]
    pub fn insert_summary_turn(
        &self,
        session_id: &str,
        summary: &str,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let turn_id = format!(
            "summary_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            rand::random::<u16>()
        );
        let seq = self.next_seq_locked(&conn, session_id)?;
        let now = Utc::now().to_rfc3339();
        let token_usage_estimated = i64::from(token_usage_estimated);
        conn.execute(
            "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content, assistant_timestamp, status, tool_reports, hidden, is_summary, token_total, token_usage_estimated, token_prompt, token_cache_read)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'completed', '[]', 0, 1, ?8, ?9, ?10, ?11)",
            params![turn_id, session_id, seq, "[conversation summary]", now, summary, now, tokens.total as i64, token_usage_estimated, tokens.prompt as i64, tokens.cache_read as i64],
        )?;
        Ok(())
    }

    pub fn load_last_summary(&self, session_id: &str) -> Result<Option<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 AND is_summary = 1 AND hidden = 0 ORDER BY seq DESC LIMIT 1",
        )?;
        let turn = stmt
            .query_map(params![session_id], map_turn_row)?
            .next()
            .transpose()?;
        Ok(turn)
    }

    #[allow(dead_code)]
    pub fn count_turns(&self, session_id: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    #[allow(dead_code)]
    pub fn total_chars(&self, session_id: &str) -> Result<usize> {
        let turns = self.load_turns(session_id)?;
        Ok(turns.iter().map(|t| turn_chars(t)).sum())
    }

    #[allow(dead_code)]
    pub fn trim_oldest_turns(&self, session_id: &str, count: usize) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 AND is_summary = 0 ORDER BY seq ASC LIMIT ?2",
        )?;
        let mut to_remove: Vec<Turn> = stmt
            .query_map(params![session_id, count as i64], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        attach_turn_children_locked(&conn, &mut to_remove)?;
        for turn in &to_remove {
            conn.execute(
                "DELETE FROM turns WHERE turn_id = ?1",
                params![turn.turn_id],
            )?;
        }
        Ok(to_remove)
    }

    pub fn oldest_evictable_visible_turns(
        &self,
        session_id: &str,
        count: usize,
    ) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns
             WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0 AND status != 'running'
             ORDER BY seq ASC LIMIT ?2",
        )?;
        let count = i64::try_from(count).unwrap_or(i64::MAX);
        let mut turns = stmt
            .query_map(params![session_id, count], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_turn_children_locked(&conn, &mut turns)?;
        Ok(turns)
    }

    pub fn delete_visible_turns(&self, session_id: &str, turn_ids: &[String]) -> Result<usize> {
        self.delete_visible_turns_checked(session_id, turn_ids, None)
    }

    pub fn delete_visible_turns_checked(
        &self,
        session_id: &str,
        turn_ids: &[String],
        expected_loaded_tools: Option<&[(String, Option<String>)]>,
    ) -> Result<usize> {
        if turn_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        verify_loaded_tool_sources(&tx, session_id, expected_loaded_tools)?;
        let affected = delete_visible_turns_in_transaction(&tx, session_id, turn_ids)?;
        tx.commit()?;
        Ok(affected)
    }

    pub fn archive_and_delete_visible_turns(
        &self,
        session_id: &str,
        archive_db: &Path,
        turns: &[EvictedTurn],
        turn_ids: &[String],
        expected_loaded_tools: Option<&[(String, Option<String>)]>,
    ) -> Result<usize> {
        if turn_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn.lock().unwrap();
        let archive_db = archive_db.to_string_lossy().into_owned();
        let archive_alias = format!("evicted_context_{}", rand::random::<u32>());
        conn.execute(
            &format!("ATTACH DATABASE ?1 AS {archive_alias}"),
            params![archive_db],
        )?;
        let insert_sql = format!(
            "INSERT OR IGNORE INTO {archive_alias}.evicted_turns
             (source_id, timestamp, role, content, created_at,
              visibility, owner_principal, owner_display_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        );
        let operation = (|| -> Result<usize> {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            verify_loaded_tool_sources(&tx, session_id, expected_loaded_tools)?;
            let created_at = Utc::now().to_rfc3339();
            for turn in turns {
                tx.execute(
                    &insert_sql,
                    params![
                        turn.source_id,
                        turn.timestamp,
                        turn.role,
                        turn.content,
                        created_at,
                        turn.visibility,
                        turn.owner_principal,
                        turn.owner_display_name,
                    ],
                )?;
            }
            let affected = delete_visible_turns_in_transaction(&tx, session_id, turn_ids)?;
            tx.commit()?;
            Ok(affected)
        })();
        let detach = conn.execute_batch(&format!("DETACH DATABASE {archive_alias}"));
        if let Err(detach_err) = detach {
            tracing::warn!(
                error = %detach_err,
                archive_alias,
                "{}",
                crate::i18n::text(
                    "failed to detach evicted-context database",
                    "分离已移出上下文的数据库失败",
                )
            );
        }
        operation
    }

    /// Mechanical prune: replaces old visible turns' tool_reports with a
    /// one-line placeholder (tool output is re-derivable — files can be
    /// re-read, commands re-run). All-or-nothing behind a harvest gate:
    /// rewriting history is a prefix-cache reset, so it only happens when the
    /// batch saves enough to pay for that reset. Write-once archive keeps the
    /// original JSON; a turn with an archive is never rewritten again, which
    /// makes the prune monotonic (repeat calls never re-crater the cache).
    pub fn prune_stale_tool_reports(
        &self,
        session_id: &str,
        protect_recent: usize,
        min_saved_chars: usize,
    ) -> Result<PruneStats> {
        const MIN_PRUNE_BYTES: usize = 1024;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let rows: Vec<(String, String, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT turn_id, tool_reports, tool_reports_archive FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                   AND status = 'completed'
                 ORDER BY seq ASC",
            )?;
            let rows = stmt
                .query_map(params![session_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let eligible = rows.len().saturating_sub(protect_recent);
        let mut updates = Vec::new();
        let mut saved_chars = 0usize;
        for (turn_id, reports_json, archive) in rows.into_iter().take(eligible) {
            if archive.is_some() {
                continue;
            }
            let reports: Vec<String> =
                serde_json::from_str(&reports_json).unwrap_or_default();
            if reports.is_empty() {
                continue;
            }
            let total: usize = reports.iter().map(|report| report.len()).sum();
            if total < MIN_PRUNE_BYTES {
                continue;
            }
            let placeholder = format!(
                "[{} 条旧工具记录已折叠以释放上下文 — 原文已归档；需要该数据时请重新调用工具 / {} old tool report(s) elided to free context — re-run the tool if the data is needed again]",
                reports.len(),
                reports.len(),
            );
            saved_chars += total.saturating_sub(placeholder.len());
            let new_json = serde_json::to_string(&vec![placeholder])?;
            updates.push((turn_id, reports_json, new_json));
        }
        if updates.is_empty() || saved_chars < min_saved_chars {
            tx.rollback()?;
            return Ok(PruneStats::default());
        }
        let turns = updates.len();
        {
            let mut stmt = tx.prepare(
                "UPDATE turns SET tool_reports_archive = ?2, tool_reports = ?3
                 WHERE turn_id = ?1 AND session_id = ?4",
            )?;
            for (turn_id, original, replacement) in &updates {
                stmt.execute(params![turn_id, original, replacement, session_id])?;
            }
        }
        tx.commit()?;
        Ok(PruneStats { turns, saved_chars })
    }

    pub fn replace_visible_with_summary(
        &self,
        session_id: &str,
        fold_turn_ids: &[String],
        visible_turn_ids: &[String],
        summary: &str,
        tokens: TurnTokens,
        token_usage_estimated: bool,
        footprint_json: Option<&str>,
    ) -> Result<()> {
        if summary.trim().is_empty() {
            bail!("compact returned an empty summary");
        }
        if fold_turn_ids.is_empty() {
            bail!("compact selected no turns to fold");
        }

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let current_turn_ids = {
            let mut stmt = tx.prepare(
                "SELECT turn_id FROM turns
                 WHERE session_id = ?1 AND hidden = 0 ORDER BY seq ASC",
            )?;
            let turn_ids = stmt
                .query_map(params![session_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            turn_ids
        };
        if current_turn_ids != visible_turn_ids {
            bail!("conversation changed while compact was running");
        }
        // The previous summary (if any) is superseded by the merged one and
        // folds together with the selected turns. Tail turns keep lower seqs
        // than the old summary row, so membership is by explicit id, not by a
        // seq watermark.
        let prior_summary_ids = {
            let mut stmt = tx.prepare(
                "SELECT turn_id FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 1",
            )?;
            let ids = stmt
                .query_map(params![session_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ids
        };
        let parent_summary_seq: Option<i64> = tx.query_row(
            "SELECT MAX(seq) FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 1",
            params![session_id],
            |row| row.get(0),
        )?;
        let mut hidden_ids: Vec<String> = fold_turn_ids.to_vec();
        for id in prior_summary_ids {
            if !hidden_ids.contains(&id) {
                hidden_ids.push(id);
            }
        }
        let mut hidden = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE turns SET hidden = 1
                 WHERE session_id = ?1 AND hidden = 0 AND turn_id = ?2",
            )?;
            for id in &hidden_ids {
                hidden += stmt.execute(params![session_id, id])?;
            }
        }
        if hidden == 0 {
            bail!("conversation changed before compact could be saved");
        }
        let hidden_json = serde_json::to_string(&hidden_ids)?;

        let turn_id = format!(
            "summary_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            rand::random::<u16>()
        );
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        let now = Utc::now().to_rfc3339();
        let token_total = tokens.total as i64;
        let token_usage_estimated = i64::from(token_usage_estimated);
        tx.execute(
            "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content, assistant_timestamp, status, tool_reports, hidden, is_summary, token_total, token_usage_estimated, token_prompt, token_cache_read, compact_reversible, compact_parent_summary_seq, compact_hidden_json, tool_footprint)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'completed', '[]', 0, 1, ?8, ?9, ?13, ?14, 1, ?10, ?11, ?12)",
            params![turn_id, session_id, seq, "[conversation summary]", now, summary, now, token_total, token_usage_estimated, parent_summary_seq, hidden_json, footprint_json, tokens.prompt as i64, tokens.cache_read as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn reset(&self, session_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM queued_prompts WHERE session_id = ?1",
            params![session_id],
        )?;
        tx.execute(
            "DELETE FROM turns WHERE session_id = ?1",
            params![session_id],
        )?;
        tx.execute(
            "DELETE FROM session_loaded_items WHERE session_id = ?1",
            params![session_id],
        )?;
        // Subagent audit sessions now count toward this session's Σ, so a
        // reset that left them behind would zero the history and still report
        // a running total. They are records of a conversation that no longer
        // exists; they go with it.
        tx.execute(
            "DELETE FROM sessions WHERE parent_session_id = ?1 AND kind = 'subagent'",
            params![session_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn reset_persona_contexts(&self, persona: &str, platform: &str) -> Result<Vec<String>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target_sql = "WITH RECURSIVE targets(session_id) AS (
                 SELECT sessions.session_id
                   FROM sessions
                  WHERE sessions.persona = ?1
                    AND sessions.kind = 'user'
                    AND (
                        (NOT EXISTS (
                            SELECT 1 FROM platform_session_bindings
                             WHERE platform_session_bindings.session_id = sessions.session_id
                        ))
                        OR EXISTS (
                            SELECT 1 FROM platform_session_bindings
                             WHERE platform_session_bindings.session_id = sessions.session_id
                               AND platform_session_bindings.platform = ?2
                        )
                    )
                 UNION
                 SELECT child.session_id
                   FROM sessions child
                   JOIN targets parent ON child.parent_session_id = parent.session_id
                  WHERE child.persona = ?1
             )";
        let session_ids = {
            let mut stmt = tx.prepare(&format!(
                "{target_sql} SELECT session_id FROM targets ORDER BY session_id"
            ))?;
            let rows = stmt.query_map(params![persona, platform], |row| row.get(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for table in ["queued_prompts", "turns", "session_loaded_items"] {
            tx.execute(
                &format!(
                    "{target_sql} DELETE FROM {table} WHERE session_id IN (SELECT session_id FROM targets)"
                ),
                params![persona, platform],
            )?;
        }
        // Subagent runs bill to the session that launched them, and their usage
        // lives on the session row rather than in `turns` — deleting the turns
        // alone would leave every Σ still carrying the subagent totals of a
        // conversation that no longer exists.
        tx.execute(
            &format!(
                "{target_sql} DELETE FROM sessions
                  WHERE kind = 'subagent' AND session_id IN (SELECT session_id FROM targets)"
            ),
            params![persona, platform],
        )?;
        tx.commit()?;
        Ok(session_ids)
    }

    /// Lifetime token total of one session, summed over every turn row —
    /// including hidden (compacted) turns and summary rows, so the counter
    /// keeps growing across compactions and only /reset (which deletes the
    /// rows) brings it back to zero.
    pub fn session_token_total(&self, session_id: &str) -> Result<u64> {
        Ok(self.session_token_totals(session_id)?.total)
    }

    /// Session-lifetime sums behind the Σ meter. Returned together because the
    /// cumulative cache rate is `cache_read / prompt` and reading the two
    /// halves through separate locks could straddle a turn commit.
    pub fn session_token_totals(&self, session_id: &str) -> Result<TurnTokens> {
        let conn = self.conn.lock().unwrap();
        let (total, prompt, cache_read): (i64, i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(token_total), 0), COALESCE(SUM(token_prompt), 0),
                    COALESCE(SUM(token_cache_read), 0)
             FROM turns WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        // Subagents bill to the session that launched them: their audit
        // sessions hang off this one, and a Σ that ignored them would hide the
        // single biggest thing a turn can spend. Estimated runs land in
        // `total_tokens` only — `prompt_tokens` stays 0 when the provider
        // reported nothing — so a guessed number can inflate Σ but never
        // reaches the cache rate's denominator.
        let (sub_total, sub_prompt, sub_cache): (i64, i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(total_tokens), 0),
                    COALESCE(SUM(CASE WHEN cache_read_tokens IS NULL THEN 0
                                      ELSE prompt_tokens END), 0),
                    COALESCE(SUM(cache_read_tokens), 0)
             FROM sessions WHERE parent_session_id = ?1 AND kind = 'subagent'",
            rusqlite::params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        Ok(TurnTokens {
            total: total.saturating_add(sub_total).max(0) as u64,
            prompt: prompt.saturating_add(sub_prompt).max(0) as u64,
            cache_read: cache_read.saturating_add(sub_cache).max(0) as u64,
        })
    }

    pub fn reset_history(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM turns WHERE session_id = ?1",
            params![session_id],
        )?;
        conn.execute(
            "DELETE FROM session_loaded_items WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    pub fn undo_last_turn(&self, session_id: &str) -> Result<(usize, Option<String>)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let running: i64 = tx.query_row(
            "SELECT COUNT(*) FROM turns WHERE session_id = ?1 AND hidden = 0 AND status = 'running'",
            params![session_id],
            |row| row.get(0),
        )?;
        if running > 0 {
            tx.rollback()?;
            return Ok((0, None));
        }
        let last: Option<(String, i64, String, bool, bool, Option<i64>, Option<String>)> = tx
            .query_row(
                "SELECT turn_id, seq, user_content, is_summary,
                        compact_reversible, compact_parent_summary_seq, compact_hidden_json
                 FROM turns WHERE session_id = ?1 AND hidden = 0 ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, i64>(3)? != 0,
                        row.get::<_, i64>(4)? != 0,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;
        match last {
            Some((turn_id, _, user_content, false, _, _, _)) => {
                tx.execute("DELETE FROM turns WHERE turn_id = ?1", params![turn_id])?;
                tx.commit()?;
                Ok((1, Some(user_content)))
            }
            Some((_, _, _, true, false, _, _)) => {
                tx.rollback()?;
                Ok((0, None))
            }
            Some((turn_id, _summary_seq, _, true, true, _, Some(hidden_json))) => {
                // Tail-retention era summary: restore exactly the set this
                // compaction hid (folded turns + the superseded summary row).
                let hidden_ids: Vec<String> =
                    serde_json::from_str(&hidden_json).unwrap_or_default();
                if hidden_ids.is_empty() {
                    tx.rollback()?;
                    return Ok((0, None));
                }
                let mut restored = 0usize;
                {
                    let mut stmt = tx.prepare(
                        "UPDATE turns SET hidden = 0
                         WHERE session_id = ?1 AND hidden = 1 AND turn_id = ?2",
                    )?;
                    for id in &hidden_ids {
                        restored += stmt.execute(params![session_id, id])?;
                    }
                }
                if restored == 0 {
                    tx.rollback()?;
                    return Ok((0, None));
                }
                tx.execute("DELETE FROM turns WHERE turn_id = ?1", params![turn_id])?;
                tx.commit()?;
                Ok((1, None))
            }
            Some((turn_id, summary_seq, _, true, true, parent_summary_seq, None)) => {
                let restorable: i64 = match parent_summary_seq {
                    Some(previous_seq) => tx.query_row(
                        "SELECT COUNT(*) FROM turns
                         WHERE session_id = ?1 AND hidden = 1 AND seq < ?2
                           AND (seq = ?3 OR (is_summary = 0 AND seq > ?3))",
                        params![session_id, summary_seq, previous_seq],
                        |row| row.get(0),
                    )?,
                    None => tx.query_row(
                        "SELECT COUNT(*) FROM turns
                         WHERE session_id = ?1 AND hidden = 1 AND is_summary = 0 AND seq < ?2",
                        params![session_id, summary_seq],
                        |row| row.get(0),
                    )?,
                };
                if restorable == 0 {
                    tx.rollback()?;
                    return Ok((0, None));
                }

                tx.execute("DELETE FROM turns WHERE turn_id = ?1", params![turn_id])?;
                match parent_summary_seq {
                    Some(previous_seq) => {
                        tx.execute(
                            "UPDATE turns SET hidden = 0
                             WHERE session_id = ?1 AND hidden = 1 AND seq < ?2
                               AND (seq = ?3 OR (is_summary = 0 AND seq > ?3))",
                            params![session_id, summary_seq, previous_seq],
                        )?;
                    }
                    None => {
                        tx.execute(
                            "UPDATE turns SET hidden = 0
                             WHERE session_id = ?1 AND hidden = 1 AND is_summary = 0 AND seq < ?2",
                            params![session_id, summary_seq],
                        )?;
                    }
                }
                tx.commit()?;
                Ok((1, None))
            }
            None => Ok((0, None)),
        }
    }

    #[allow(dead_code)]
    /// Completed background-command wake turns after `after_seq`, oldest
    /// first: (seq, user display content, assistant reply).
    pub fn background_report_replies_after(
        &self,
        session_id: &str,
        after_seq: i64,
    ) -> Result<Vec<(i64, String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT seq, turn_id, display_content,
                    CASE WHEN status = 'completed' THEN assistant_content
                         WHEN length(trim(assistant_content)) > 0 THEN assistant_content
                         ELSE '（自动跟进未能完成：模型请求失败或被中断，可用 job_status 查看任务输出）'
                    END
             FROM turns
             WHERE session_id = ?1 AND seq > ?2 AND status IN ('completed', 'failed', 'interrupted')
               AND user_content LIKE '<background-job-report>%'
             ORDER BY seq ASC LIMIT 8",
        )?;
        let rows = stmt
            .query_map(params![session_id, after_seq], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Largest turn seq in a session (0 when empty).
    pub fn latest_turn_seq(&self, session_id: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?)
    }

    pub fn has_running_turns(&self, session_id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM turns WHERE session_id = ?1 AND status = 'running'",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn has_any_running_turns(&self) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM turns WHERE status = 'running'",
            [],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn running_turn_queue_target(
        &self,
        session_id: &str,
    ) -> Result<Option<(String, Option<String>, Option<u32>)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT turns.turn_id,
                    COALESCE(
                        turns.queue_session_id,
                        (SELECT queued_prompts.queue_session_id
                           FROM queued_prompts
                          WHERE queued_prompts.owner_pid = turns.owner_pid
                            AND queued_prompts.queue_session_id IS NOT NULL
                          ORDER BY queued_prompts.seq DESC
                          LIMIT 1)
                    ),
                    turns.owner_pid
               FROM turns
              WHERE turns.session_id = ?1 AND turns.status = 'running'
              ORDER BY turns.seq DESC
              LIMIT 1",
            params![session_id],
            |row| {
                let owner_pid = row
                    .get::<_, Option<i64>>(2)?
                    .and_then(|pid| u32::try_from(pid).ok());
                Ok((row.get(0)?, row.get(1)?, owner_pid))
            },
        )
        .optional()
        .map_err(Into::into)
    }

    #[allow(dead_code)]
    pub fn running_turn_summaries(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_content FROM turns
             WHERE session_id = ?1 AND status = 'running' ORDER BY seq ASC",
        )?;
        let summaries = stmt
            .query_map(params![session_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(summaries)
    }

    pub fn running_turn_summaries_excluding(
        &self,
        session_id: &str,
        exclude_turn_id: &str,
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_content FROM turns
             WHERE session_id = ?1 AND status = 'running' AND turn_id != ?2 ORDER BY seq ASC",
        )?;
        let summaries = stmt
            .query_map(params![session_id, exclude_turn_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(summaries)
    }

    pub fn recover_stale_running_turns(&self) -> Result<Vec<StaleTurnRecovery>> {
        let mut conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, session_id, owner_pid, revision, queue_session_id
             FROM turns WHERE status = 'running'",
        )?;
        let stale_turn_ids: Vec<(String, String, i64, Option<String>)> = stmt
            .query_map([], |row| {
                let turn_id: String = row.get(0)?;
                let session_id: String = row.get(1)?;
                let owner_pid: Option<i64> = row.get(2)?;
                let revision: i64 = row.get(3)?;
                let queue_session_id: Option<String> = row.get(4)?;
                Ok((turn_id, session_id, owner_pid, revision, queue_session_id))
            })?
            .filter_map(|row| {
                let (turn_id, session_id, owner_pid, revision, queue_session_id) = row.ok()?;
                let alive = owner_pid
                    .map(|pid| crate::alarm::process_exists(pid as u32))
                    .unwrap_or(false);
                if alive {
                    None
                } else {
                    Some((turn_id, session_id, revision, queue_session_id))
                }
            })
            .collect();
        drop(stmt);
        if stale_turn_ids.is_empty() {
            return Ok(Vec::new());
        }
        let tx = conn.transaction()?;
        let now = Utc::now().to_rfc3339();
        let mut recoveries = Vec::with_capacity(stale_turn_ids.len());
        for (turn_id, session_id, revision, queue_session_id) in &stale_turn_ids {
            if restore_redo_backup_locked(&tx, turn_id, *revision)? {
                recoveries.push(StaleTurnRecovery {
                    turn_id: turn_id.clone(),
                    session_id: session_id.clone(),
                    restored_redo: true,
                });
                continue;
            }
            consume_stale_queued_prompts_locked(
                &tx,
                turn_id,
                *revision,
                queue_session_id.as_deref(),
                &now,
            )?;
            let (content, reasoning) = interrupted_projection_locked(&tx, turn_id, *revision)?;
            let turn_affected = tx.execute(
                "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                        assistant_timestamp = ?3, status = 'interrupted'
                 WHERE turn_id = ?4 AND revision = ?5 AND status = 'running'",
                params![content, reasoning, now, turn_id, revision],
            )?;
            if turn_affected == 1 {
                bump_completion_seq_locked(&tx, turn_id)?;
                tx.execute(
                    "UPDATE turn_journal_segments
                     SET status = 'interrupted', finished_at = ?1
                     WHERE turn_id = ?2 AND revision = ?3 AND status = 'running'",
                    params![now, turn_id, revision],
                )?;
                recoveries.push(StaleTurnRecovery {
                    turn_id: turn_id.clone(),
                    session_id: session_id.clone(),
                    restored_redo: false,
                });
            }
        }
        tx.commit()?;
        Ok(recoveries)
    }

    fn next_seq_locked(&self, conn: &Connection, session_id: &str) -> Result<i64> {
        let next_seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(next_seq)
    }

    #[allow(dead_code)]
    pub fn migrate_from_jsonl(&self, session_id: &str, jsonl_path: &Path) -> Result<usize> {
        if !jsonl_path.exists() {
            return Ok(0);
        }
        let turns = self.load_turns(session_id)?;
        if !turns.is_empty() {
            return Ok(0);
        }
        let file = std::fs::File::open(jsonl_path)?;
        use std::io::{BufRead, BufReader};
        let mut migrated = 0usize;
        let mut pending_user: Option<(String, String)> = None;
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let role = entry.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = entry.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let timestamp = entry
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let reasoning = entry
                .get("reasoning")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if role == "user" {
                if let Some((prev_ts, prev_content)) = pending_user.take() {
                    let turn_id = format!("migrated_{}", migrated);
                    let conn = self.conn.lock().unwrap();
                    let seq = self.next_seq_locked(&conn, session_id)?;
                    conn.execute(
                        "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content, status)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'completed')",
                        params![turn_id, session_id, seq, prev_content, prev_ts, "(migrated without reply)"],
                    )?;
                    drop(conn);
                    migrated += 1;
                }
                pending_user = Some((timestamp, content.to_string()));
            } else if role == "assistant" {
                if let Some((user_ts, user_content)) = pending_user.take() {
                    let turn_id = format!("migrated_{}", migrated);
                    let conn = self.conn.lock().unwrap();
                    let seq = self.next_seq_locked(&conn, session_id)?;
                    let now = Utc::now().to_rfc3339();
                    conn.execute(
                        "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp,
                         assistant_content, assistant_reasoning, assistant_timestamp, status)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'completed')",
                        params![
                            turn_id,
                            session_id,
                            seq,
                            user_content,
                            user_ts,
                            content,
                            reasoning,
                            now
                        ],
                    )?;
                    drop(conn);
                    migrated += 1;
                }
            }
        }
        if let Some((user_ts, user_content)) = pending_user {
            let turn_id = format!("migrated_{}", migrated);
            let conn = self.conn.lock().unwrap();
            let seq = self.next_seq_locked(&conn, session_id)?;
            conn.execute(
                "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'interrupted')",
                params![
                    turn_id,
                    session_id,
                    seq,
                    user_content,
                    user_ts,
                    "上一轮响应已中断，未完成。不要继续执行上一轮任务，除非用户重新要求。"
                ],
            )?;
            drop(conn);
            migrated += 1;
        }
        Ok(migrated)
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

fn interrupted_prefix(content: &str) -> String {
    let suffix = format!("\n\n{INTERRUPTED_TEXT}");
    content
        .strip_suffix(&suffix)
        .unwrap_or_else(|| content.strip_suffix(INTERRUPTED_TEXT).unwrap_or(content))
        .to_string()
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

#[allow(dead_code)]
pub fn pending_placeholder() -> &'static str {
    PENDING_PLACEHOLDER
}

#[allow(dead_code)]
pub fn interrupted_text() -> &'static str {
    INTERRUPTED_TEXT
}

fn map_turn_row(row: &rusqlite::Row) -> rusqlite::Result<Turn> {
    let tool_reports_json: String = row.get(11)?;
    let tool_reports: Vec<String> = serde_json::from_str(&tool_reports_json).unwrap_or_default();
    let context_messages_json: String = row.get::<_, Option<String>>(18)?.unwrap_or_default();
    let context_messages: Vec<ChatMessage> =
        serde_json::from_str(&context_messages_json).unwrap_or_default();
    let tool_flow_json: String = row.get::<_, Option<String>>(21)?.unwrap_or_default();
    let tool_flow: Vec<ToolFlowRound> = serde_json::from_str(&tool_flow_json).unwrap_or_default();
    Ok(Turn {
        turn_id: row.get(0)?,
        seq: row.get(1)?,
        user_content: row.get(2)?,
        display_content: row.get(3)?,
        user_timestamp: row.get(4)?,
        assistant_content: row.get(5)?,
        assistant_reasoning: row.get(6)?,
        assistant_provider_id: row.get(7)?,
        assistant_model: row.get(8)?,
        assistant_timestamp: row.get(9)?,
        status: TurnStatus::from_str(row.get::<_, String>(10)?.as_str()),
        tool_reports,
        tool_flow,
        question_exchanges: Vec::new(),
        followups: Vec::new(),
        attachments: Vec::new(),
        hidden: row.get::<_, i64>(12)? != 0,
        is_summary: row.get::<_, i64>(13)? != 0,
        owner_pid: row.get(14)?,
        token_total: row.get::<_, i64>(15)?.max(0) as u64,
        token_prompt: row.get::<_, i64>(19)?.max(0) as u64,
        token_cache_read: row.get::<_, i64>(20)?.max(0) as u64,
        token_usage_estimated: row.get::<_, i64>(16)? != 0,
        revision: row.get(17)?,
        journal_events: Vec::new(),
        context_messages,
    })
}

fn map_user_attachment_row(row: &rusqlite::Row) -> rusqlite::Result<UserAttachment> {
    Ok(UserAttachment {
        attachment_id: row.get(0)?,
        file_name: row.get(1)?,
        mime: row.get(2)?,
        kind: row.get(3)?,
        size_bytes: row.get::<_, i64>(4)?.max(0) as u64,
        width: row.get::<_, i64>(5)?.max(0) as u32,
        height: row.get::<_, i64>(6)?.max(0) as u32,
        created_at: row.get(7)?,
    })
}

fn map_image_asset_row(row: &rusqlite::Row) -> rusqlite::Result<ImageAsset> {
    Ok(ImageAsset {
        asset_id: row.get(0)?,
        turn_id: row.get(1)?,
        tool_id: row.get(2)?,
        mime: row.get(3)?,
        width: row.get::<_, i64>(4)?.max(0) as u32,
        height: row.get::<_, i64>(5)?.max(0) as u32,
        alt: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn map_artifact_asset_row(row: &rusqlite::Row) -> rusqlite::Result<ArtifactAsset> {
    Ok(ArtifactAsset {
        asset_id: row.get(0)?,
        turn_id: row.get(1)?,
        tool_id: row.get(2)?,
        source_key: row.get(3)?,
        file_name: row.get(4)?,
        mime: row.get(5)?,
        kind: row.get(6)?,
        size_bytes: row.get::<_, i64>(7)?.max(0) as u64,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn attach_turn_children_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    attach_question_exchanges_locked(conn, turns)?;
    attach_followups_locked(conn, turns)?;
    attach_turn_attachments_locked(conn, turns)?;
    attach_turn_journal_events_locked(conn, turns)
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
            // The `LIKE` marks daemon-synthesized background-job wake turns.
            // They are not user prompts and must not be replayed as one — same
            // test the wake-report poller uses.
            "SELECT display_content, assistant_content, replay_journal,
                    user_content LIKE '<background-job-report>%'
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
                    is_job_wake: row.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }
}

/// One replayable turn: the prompt echo plus either its ordered transcript or,
/// for turns predating the transcript column, just the final reply.
#[derive(Clone, Debug, Default)]
pub struct TurnReplay {
    /// What the user saw as the prompt — or, for a wake turn, the
    /// `[后台任务完成] …` headline.
    pub display_content: String,
    pub assistant_content: String,
    pub entries: Vec<ReplayEntry>,
    /// Daemon-synthesized follow-up to a finished background job, not a
    /// prompt anybody typed.
    pub is_job_wake: bool,
}

/// Folds the live journal of a just-finished turn into `turns.replay_journal`.
/// Everything only the live view needed — reasoning, progress ticks, command
/// output blobs — is dropped; what is left is the ordered prose/tool sequence
/// the REPL redraws when the session is reopened.
fn store_replay_journal(tx: &Transaction, turn_id: &str) -> Result<()> {
    // 只取当前修订的事件:被 redo 的 interrupted 回合会同时残留新旧两个
    // revision 的事件(interrupt 不删 segments),混着快照会串台。
    let mut stmt = tx.prepare(
        "SELECT kind, call_id, name, text_payload, ok
           FROM turn_journal_events
          WHERE turn_id = ?1
            AND revision = (SELECT revision FROM turns WHERE turn_id = ?1)
            AND kind IN ('assistant_content', 'tool_call', 'tool_result')
          ORDER BY event_id",
    )?;
    let rows = stmt
        .query_map(params![turn_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let mut entries: Vec<ReplayEntry> = Vec::new();
    let mut call_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut text = String::new();
    let flush_text = |entries: &mut Vec<ReplayEntry>, text: &mut String| {
        if !text.trim().is_empty() {
            entries.push(ReplayEntry::Text {
                text: truncate_chars_owned(text, REPLAY_ENTRY_MAX_CHARS),
            });
        }
        text.clear();
    };
    for (kind, call_id, name, payload, ok) in rows {
        match kind.as_str() {
            "assistant_content" => text.push_str(payload.as_deref().unwrap_or_default()),
            "tool_call" => {
                flush_text(&mut entries, &mut text);
                let Some(name) = name else { continue };
                if let Some(call_id) = call_id {
                    call_names.insert(call_id, name.clone());
                }
                entries.push(ReplayEntry::ToolCall {
                    name,
                    arguments: truncate_chars_owned(
                        payload.as_deref().unwrap_or_default(),
                        REPLAY_ENTRY_MAX_CHARS,
                    ),
                });
            }
            "tool_result" => {
                flush_text(&mut entries, &mut text);
                let name = call_id
                    .as_deref()
                    .and_then(|id| call_names.get(id).cloned())
                    .or(name)
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                entries.push(ReplayEntry::ToolResult {
                    name,
                    ok: ok.unwrap_or(1) != 0,
                    output: truncate_chars_owned(
                        payload.as_deref().unwrap_or_default(),
                        REPLAY_ENTRY_MAX_CHARS,
                    ),
                });
            }
            _ => {}
        }
    }
    flush_text(&mut entries, &mut text);
    if entries.is_empty() {
        return Ok(());
    }
    // Whole-turn budget: drop the oldest entries, so what survives is the tail
    // the user was actually looking at when the turn ended.
    let mut encoded = serde_json::to_string(&entries)?;
    while encoded.len() > REPLAY_JOURNAL_MAX_CHARS && entries.len() > 1 {
        entries.remove(0);
        encoded = serde_json::to_string(&entries)?;
    }
    tx.execute(
        "UPDATE turns SET replay_journal = ?1 WHERE turn_id = ?2",
        params![encoded, turn_id],
    )?;
    Ok(())
}

fn truncate_chars_owned(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let kept: String = value.chars().take(max).collect();
    format!("{kept}…")
}

fn attach_turn_journal_events_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    // BTreeMap keeps the chunking below deterministic; HashMap iteration order
    // would shuffle turn ids across the 900-id chunks between calls.
    let indexes = turns
        .iter()
        .enumerate()
        .filter(|(_, turn)| turn.status != TurnStatus::Completed)
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::BTreeMap<_, _>>();
    if indexes.is_empty() {
        return Ok(());
    }
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT e.turn_id, e.event_id, e.revision, e.segment_index, e.kind,
                    e.call_id, e.name, e.text_payload, e.blob_payload, e.ok
             FROM turn_journal_events e
             INNER JOIN turn_journal_segments s
               ON s.turn_id = e.turn_id AND s.revision = e.revision
              AND s.segment_index = e.segment_index
             INNER JOIN turns t ON t.turn_id = e.turn_id AND t.revision = e.revision
             WHERE e.turn_id IN ({placeholders})
                AND (
                    s.status != 'superseded'
                    OR (
                        e.kind IN (
                            'tool_call', 'tool_result', 'tool_progress',
                            'command_stdout', 'command_stderr', 'image', 'artifact'
                        )
                        AND EXISTS(
                            SELECT 1 FROM turn_journal_events result_event
                            WHERE result_event.turn_id = e.turn_id
                              AND result_event.revision = e.revision
                              AND result_event.segment_index = e.segment_index
                              AND result_event.kind = 'tool_result'
                              AND result_event.call_id = e.call_id
                        )
                    )
                )
             ORDER BY e.event_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                TurnJournalEvent {
                    event_id: row.get(1)?,
                    revision: row.get(2)?,
                    segment_index: row.get(3)?,
                    kind: row.get(4)?,
                    call_id: row.get(5)?,
                    name: row.get(6)?,
                    text_payload: row.get(7)?,
                    blob_payload: row.get(8)?,
                    ok: row.get::<_, Option<i64>>(9)?.map(|value| value != 0),
                },
            ))
        })?;
        for row in rows {
            let (turn_id, event) = row?;
            if let Some(index) = indexes.get(&turn_id).copied() {
                turns[index].journal_events.push(event);
            }
        }
    }
    Ok(())
}

fn attach_turn_attachments_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    if turns.is_empty() {
        return Ok(());
    }
    let indexes = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT turn_id, attachment_id, file_name, mime, kind, size_bytes,
                    width, height, created_at FROM user_attachments
             WHERE turn_id IN ({placeholders}) ORDER BY created_at, attachment_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                UserAttachment {
                    attachment_id: row.get(1)?,
                    file_name: row.get(2)?,
                    mime: row.get(3)?,
                    kind: row.get(4)?,
                    size_bytes: row.get::<_, i64>(5)?.max(0) as u64,
                    width: row.get::<_, i64>(6)?.max(0) as u32,
                    height: row.get::<_, i64>(7)?.max(0) as u32,
                    created_at: row.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (turn_id, attachment) = row?;
            if let Some(index) = indexes.get(&turn_id).copied() {
                turns[index].attachments.push(attachment);
            }
        }
    }
    Ok(())
}

fn attach_question_exchanges_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    if turns.is_empty() {
        return Ok(());
    }
    let indexes = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT turn_id, payload FROM question_exchanges
             WHERE turn_id IN ({placeholders}) ORDER BY turn_id, exchange_index"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (turn_id, payload) = row?;
            let Some(index) = indexes.get(&turn_id).copied() else {
                continue;
            };
            let exchange = serde_json::from_str::<QuestionExchange>(&payload)
                .with_context(|| format!("invalid question exchange for turn {turn_id}"))?;
            turns[index].question_exchanges.push(exchange);
        }
    }
    Ok(())
}

fn attach_followups_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    if turns.is_empty() {
        return Ok(());
    }
    let indexes = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT prompt_id, turn_id, COALESCE(context_content, content), display_content,
                    attachments, submitted_at, preceding_assistant_content,
                    preceding_assistant_reasoning, preceding_assistant_provider_id,
                    preceding_assistant_model
             FROM queued_prompts
             WHERE status = 'consumed' AND turn_id IN ({placeholders})
             ORDER BY seq ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(1)?,
                TurnFollowup {
                    prompt_id: row.get(0)?,
                    content: row.get(2)?,
                    display_content: row.get(3)?,
                    attachments: serde_json::from_str(&row.get::<_, String>(4)?)
                        .unwrap_or_default(),
                    uploaded_attachments: Vec::new(),
                    submitted_at: row.get(5)?,
                    preceding_assistant_content: row.get(6)?,
                    preceding_assistant_reasoning: row.get(7)?,
                    preceding_assistant_provider_id: row.get(8)?,
                    preceding_assistant_model: row.get(9)?,
                },
            ))
        })?;
        for row in rows {
            let (turn_id, followup) = row?;
            let Some(index) = indexes.get(&turn_id).copied() else {
                continue;
            };
            turns[index].followups.push(followup);
        }
    }
    attach_followup_attachments_locked(conn, turns)?;
    Ok(())
}

fn attach_prompt_attachments_locked(conn: &Connection, prompts: &mut [QueuedPrompt]) -> Result<()> {
    let indexes = prompts
        .iter()
        .enumerate()
        .map(|(index, prompt)| (prompt.prompt_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    if indexes.is_empty() {
        return Ok(());
    }
    let prompt_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in prompt_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT prompt_id, attachment_id, file_name, mime, kind, size_bytes,
                    width, height, created_at FROM user_attachments
             WHERE prompt_id IN ({placeholders}) ORDER BY created_at, attachment_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                UserAttachment {
                    attachment_id: row.get(1)?,
                    file_name: row.get(2)?,
                    mime: row.get(3)?,
                    kind: row.get(4)?,
                    size_bytes: row.get::<_, i64>(5)?.max(0) as u64,
                    width: row.get::<_, i64>(6)?.max(0) as u32,
                    height: row.get::<_, i64>(7)?.max(0) as u32,
                    created_at: row.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (prompt_id, attachment) = row?;
            if let Some(index) = indexes.get(&prompt_id).copied() {
                prompts[index].uploaded_attachments.push(attachment);
            }
        }
    }
    Ok(())
}

fn attach_followup_attachments_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    let mut locations = std::collections::HashMap::new();
    for (turn_index, turn) in turns.iter().enumerate() {
        for (followup_index, followup) in turn.followups.iter().enumerate() {
            locations.insert(followup.prompt_id.clone(), (turn_index, followup_index));
        }
    }
    if locations.is_empty() {
        return Ok(());
    }
    let prompt_ids = locations.keys().collect::<Vec<_>>();
    for chunk in prompt_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT prompt_id, attachment_id, file_name, mime, kind, size_bytes,
                    width, height, created_at FROM user_attachments
             WHERE prompt_id IN ({placeholders}) ORDER BY created_at, attachment_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                UserAttachment {
                    attachment_id: row.get(1)?,
                    file_name: row.get(2)?,
                    mime: row.get(3)?,
                    kind: row.get(4)?,
                    size_bytes: row.get::<_, i64>(5)?.max(0) as u64,
                    width: row.get::<_, i64>(6)?.max(0) as u32,
                    height: row.get::<_, i64>(7)?.max(0) as u32,
                    created_at: row.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (prompt_id, attachment) = row?;
            if let Some((turn_index, followup_index)) = locations.get(&prompt_id).copied() {
                turns[turn_index].followups[followup_index]
                    .uploaded_attachments
                    .push(attachment);
            }
        }
    }
    Ok(())
}

