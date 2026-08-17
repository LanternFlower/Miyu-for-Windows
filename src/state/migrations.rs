//! Versioned schema migrations for conversation.db.
//!
//! Uses `PRAGMA user_version` to track the applied schema version. Each
//! migration runs inside an immediate transaction; the version is bumped in
//! the same transaction so a crash mid-migration leaves the database at the
//! previous version and the migration re-runs on next open.
//!
//! Version 1 is the idempotent baseline: it absorbs the historical
//! `CREATE TABLE IF NOT EXISTS` + `add_column_if_missing` logic so that any
//! legacy database (at any historical column state) converges to the same
//! schema. Later migrations may assume the baseline and use destructive
//! operations such as table rebuilds.

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, TransactionBehavior};

struct Migration {
    version: i64,
    name: &'static str,
    apply: fn(&Connection) -> Result<()>,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "baseline",
        apply: apply_v1_baseline,
    },
    Migration {
        version: 2,
        name: "sessions",
        apply: apply_v2_sessions,
    },
    Migration {
        version: 3,
        name: "platform_sessions_and_plugin_state",
        apply: apply_v3_platform_sessions_and_plugin_state,
    },
    Migration {
        version: 4,
        name: "platform_meme_refs",
        apply: apply_v4_platform_meme_refs,
    },
    Migration {
        version: 5,
        name: "user_attachments",
        apply: apply_v5_user_attachments,
    },
    Migration {
        version: 6,
        name: "turn_redo_checkpoints",
        apply: apply_v6_turn_redo_checkpoints,
    },
    Migration {
        version: 7,
        name: "turn_redo_backups",
        apply: apply_v7_turn_redo_backups,
    },
    Migration {
        version: 8,
        name: "artifact_assets",
        apply: apply_v8_artifact_assets,
    },
    Migration {
        version: 9,
        name: "platform_access_control",
        apply: apply_v9_platform_access_control,
    },
    Migration {
        version: 10,
        name: "turn_generation_journal",
        apply: apply_v10_turn_generation_journal,
    },
    Migration {
        version: 11,
        name: "session_model_override",
        apply: apply_v11_session_model_override,
    },
    Migration {
        version: 12,
        name: "turn_context_messages",
        apply: apply_v12_turn_context_messages,
    },
    Migration {
        version: 13,
        name: "compact_hidden_turns",
        apply: apply_v13_compact_hidden_turns,
    },
    Migration {
        version: 14,
        name: "tool_reports_archive",
        apply: apply_v14_tool_reports_archive,
    },
    Migration {
        version: 15,
        name: "session_last_request_at",
        apply: apply_v15_session_last_request_at,
    },
    Migration {
        version: 16,
        name: "turn_tool_footprint",
        apply: apply_v16_turn_tool_footprint,
    },
    Migration {
        version: 17,
        name: "turn_replay_journal",
        apply: apply_v17_turn_replay_journal,
    },
    Migration {
        version: 18,
        name: "turn_cache_tokens",
        apply: apply_v18_turn_cache_tokens,
    },
    Migration {
        version: 19,
        name: "session_cache_tokens",
        apply: apply_v19_session_cache_tokens,
    },
    Migration {
        version: 20,
        name: "turn_tool_flow",
        apply: apply_v20_turn_tool_flow,
    },
    Migration {
        version: 21,
        name: "rename_default_session",
        apply: apply_v21_rename_default_session,
    },
    Migration {
        version: 22,
        name: "session_goals",
        apply: apply_v22_session_goals,
    },
    Migration {
        version: 23,
        name: "retire_session_archiving",
        apply: apply_v23_retire_session_archiving,
    },
    Migration {
        version: 24,
        name: "retire_session_goals",
        apply: apply_v24_retire_session_goals,
    },
];

/// Latest schema version this build produces.
pub const LATEST_VERSION: i64 = 24;

/// Returns the schema version currently recorded in the database.
pub fn current_version(conn: &Connection) -> Result<i64> {
    user_version(conn)
}

/// Runs all pending migrations. Called from `ConversationDb::open` while the
/// connection is still exclusively owned by the caller.
///
/// Foreign-key enforcement is disabled for the duration: table rebuilds drop
/// and recreate parent tables, and with enforcement on the implicit
/// `DELETE FROM` of `DROP TABLE` would cascade into child tables. Integrity is
/// re-checked with `foreign_key_check` inside each migration's transaction.
pub fn run_migrations(conn: &mut Connection) -> Result<()> {
    let current = user_version(conn)?;
    let latest = MIGRATIONS.last().map(|m| m.version).unwrap_or(0);
    if current > latest {
        bail!(
            "conversation.db schema version {current} is newer than this build supports ({latest}); refusing to open"
        );
    }
    if current == latest {
        return Ok(());
    }
    conn.pragma_update(None, "foreign_keys", false)?;
    let result = apply_pending(conn, current);
    let restore = conn.pragma_update(None, "foreign_keys", true);
    result?;
    restore?;
    Ok(())
}

fn apply_pending(conn: &mut Connection, current: i64) -> Result<()> {
    for migration in MIGRATIONS.iter().filter(|m| m.version > current) {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .with_context(|| format!("failed to begin migration '{}'", migration.name))?;
        (migration.apply)(&tx)
            .with_context(|| format!("schema migration '{}' failed", migration.name))?;
        let violations: i64 =
            tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if violations > 0 {
            bail!(
                "schema migration '{}' left {violations} foreign-key violations; rolling back",
                migration.name
            );
        }
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()
            .with_context(|| format!("failed to commit migration '{}'", migration.name))?;
    }
    Ok(())
}

fn user_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// v1: idempotent baseline schema. Safe to run on an empty database or on any
/// legacy database created before versioned migrations existed.
fn apply_v1_baseline(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS turns (
            turn_id          TEXT PRIMARY KEY,
            seq              INTEGER NOT NULL UNIQUE,
            user_content     TEXT NOT NULL,
            user_timestamp   TEXT NOT NULL,
            assistant_content TEXT NOT NULL,
            assistant_reasoning TEXT,
            assistant_timestamp TEXT,
            status           TEXT NOT NULL DEFAULT 'running',
            tool_reports     TEXT NOT NULL DEFAULT '[]'
        );
        CREATE INDEX IF NOT EXISTS idx_turns_seq ON turns(seq);
        CREATE INDEX IF NOT EXISTS idx_turns_status ON turns(status);",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS question_exchanges (
            turn_id         TEXT NOT NULL,
            exchange_index  INTEGER NOT NULL,
            payload         TEXT NOT NULL,
            PRIMARY KEY (turn_id, exchange_index),
            FOREIGN KEY (turn_id) REFERENCES turns(turn_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_question_exchanges_turn
            ON question_exchanges(turn_id, exchange_index);",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS image_assets (
            asset_id    TEXT PRIMARY KEY,
            turn_id     TEXT NOT NULL,
            tool_id     TEXT,
            mime        TEXT NOT NULL,
            width       INTEGER NOT NULL DEFAULT 0,
            height      INTEGER NOT NULL DEFAULT 0,
            alt         TEXT NOT NULL DEFAULT '',
            data        BLOB NOT NULL,
            created_at  TEXT NOT NULL,
            FOREIGN KEY (turn_id) REFERENCES turns(turn_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_image_assets_turn
            ON image_assets(turn_id, created_at, asset_id);",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS queued_prompts (
            seq                         INTEGER PRIMARY KEY AUTOINCREMENT,
            prompt_id                   TEXT NOT NULL UNIQUE,
            content                     TEXT NOT NULL,
            display_content             TEXT NOT NULL,
            attachments                 TEXT NOT NULL DEFAULT '[]',
            status                      TEXT NOT NULL DEFAULT 'queued',
            submitted_at                TEXT NOT NULL,
            queue_session_id             TEXT,
            owner_pid                    INTEGER,
            consumed_at                 TEXT,
            turn_id                     TEXT,
            context_content              TEXT,
            preceding_assistant_content  TEXT,
            preceding_assistant_reasoning TEXT,
            FOREIGN KEY (turn_id) REFERENCES turns(turn_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_queued_prompts_status_seq
            ON queued_prompts(status, seq);
        CREATE INDEX IF NOT EXISTS idx_queued_prompts_turn_seq
            ON queued_prompts(turn_id, seq);",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_loaded_items (
            kind            TEXT NOT NULL,
            name            TEXT NOT NULL,
            source_turn_id  TEXT,
            created_at      TEXT NOT NULL,
            updated_at      TEXT NOT NULL,
            PRIMARY KEY (kind, name)
        );
        CREATE INDEX IF NOT EXISTS idx_session_loaded_items_source_turn
            ON session_loaded_items(source_turn_id);",
    )?;
    add_column_if_missing(conn, "turns", "hidden", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "turns", "is_summary", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "turns", "owner_pid", "INTEGER")?;
    add_column_if_missing(conn, "turns", "queue_session_id", "TEXT")?;
    add_column_if_missing(conn, "turns", "token_total", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(
        conn,
        "turns",
        "token_usage_estimated",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        conn,
        "turns",
        "compact_reversible",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "turns", "compact_parent_summary_seq", "INTEGER")?;
    add_column_if_missing(conn, "turns", "assistant_provider_id", "TEXT")?;
    add_column_if_missing(conn, "turns", "assistant_model", "TEXT")?;
    add_column_if_missing(conn, "queued_prompts", "queue_session_id", "TEXT")?;
    add_column_if_missing(conn, "queued_prompts", "owner_pid", "INTEGER")?;
    add_column_if_missing(
        conn,
        "queued_prompts",
        "preceding_assistant_provider_id",
        "TEXT",
    )?;
    add_column_if_missing(conn, "queued_prompts", "preceding_assistant_model", "TEXT")?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_turns_visible_seq ON turns(hidden, seq);
         CREATE INDEX IF NOT EXISTS idx_turns_visible_summary_seq
             ON turns(is_summary, hidden, seq);
         CREATE INDEX IF NOT EXISTS idx_queued_prompts_session_status_seq
             ON queued_prompts(queue_session_id, status, seq);",
    )?;
    Ok(())
}

/// Well-known id of the session that pre-session history is migrated into and
/// that fresh databases start with.
pub const DEFAULT_SESSION_ID: &str = "default";

/// v2: introduce the session dimension.
///
/// - `sessions` table (persona-namespaced chat topics; `kind` distinguishes
///   user sessions from subagent audit sessions).
/// - `app_state` key-value table; `current_session` holds the global default
///   session pointer.
/// - `turns` rebuilt: `session_id` column, per-session `UNIQUE(session_id,
///   seq)` replaces the global `seq UNIQUE`, plus a per-turn `workspace`
///   column recording where the turn actually executed.
/// - `session_loaded_items` rebuilt with a `(session_id, kind, name)` key.
/// - `queued_prompts` gains a nullable `session_id`.
///
/// All existing rows are assigned to the default session. The default
/// session's `persona` starts empty; the session manager stamps the active
/// persona scope on first use.
fn apply_v2_sessions(conn: &Connection) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute_batch(
        "CREATE TABLE sessions (
            session_id        TEXT PRIMARY KEY,
            persona           TEXT NOT NULL,
            name              TEXT NOT NULL,
            kind              TEXT NOT NULL DEFAULT 'user',
            parent_session_id TEXT REFERENCES sessions(session_id) ON DELETE CASCADE,
            workspace         TEXT,
            archived          INTEGER NOT NULL DEFAULT 0,
            created_at        TEXT NOT NULL,
            updated_at        TEXT NOT NULL,
            provider_id       TEXT,
            model             TEXT,
            context_window    INTEGER,
            prompt_tokens     INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens      INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX idx_sessions_persona
            ON sessions(persona, kind, archived, updated_at);
        CREATE INDEX idx_sessions_parent ON sessions(parent_session_id);
        CREATE TABLE app_state (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;
    conn.execute(
        "INSERT INTO sessions (session_id, persona, name, kind, created_at, updated_at)
         VALUES (?1, '', ?2, 'user', ?3, ?3)",
        rusqlite::params![
            DEFAULT_SESSION_ID,
            crate::i18n::text("Terminal session", "终端集成会话"),
            now
        ],
    )?;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_session', ?1)",
        [DEFAULT_SESSION_ID],
    )?;
    conn.execute_batch(&format!(
        "CREATE TABLE turns_v2 (
            turn_id          TEXT PRIMARY KEY,
            session_id       TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
            seq              INTEGER NOT NULL,
            user_content     TEXT NOT NULL,
            user_timestamp   TEXT NOT NULL,
            assistant_content TEXT NOT NULL,
            assistant_reasoning TEXT,
            assistant_timestamp TEXT,
            status           TEXT NOT NULL DEFAULT 'running',
            tool_reports     TEXT NOT NULL DEFAULT '[]',
            hidden           INTEGER NOT NULL DEFAULT 0,
            is_summary       INTEGER NOT NULL DEFAULT 0,
            owner_pid        INTEGER,
            queue_session_id TEXT,
            token_total      INTEGER NOT NULL DEFAULT 0,
            token_usage_estimated INTEGER NOT NULL DEFAULT 0,
            compact_reversible INTEGER NOT NULL DEFAULT 0,
            compact_parent_summary_seq INTEGER,
            assistant_provider_id TEXT,
            assistant_model  TEXT,
            workspace        TEXT,
            UNIQUE(session_id, seq)
        );
        INSERT INTO turns_v2 (
            turn_id, session_id, seq, user_content, user_timestamp,
            assistant_content, assistant_reasoning, assistant_timestamp,
            status, tool_reports, hidden, is_summary, owner_pid,
            queue_session_id, token_total, token_usage_estimated,
            compact_reversible, compact_parent_summary_seq,
            assistant_provider_id, assistant_model
        )
        SELECT
            turn_id, '{DEFAULT_SESSION_ID}', seq, user_content, user_timestamp,
            assistant_content, assistant_reasoning, assistant_timestamp,
            status, tool_reports, hidden, is_summary, owner_pid,
            queue_session_id, token_total, token_usage_estimated,
            compact_reversible, compact_parent_summary_seq,
            assistant_provider_id, assistant_model
        FROM turns;
        DROP TABLE turns;
        ALTER TABLE turns_v2 RENAME TO turns;
        CREATE INDEX idx_turns_status ON turns(status);
        CREATE INDEX idx_turns_visible_seq ON turns(session_id, hidden, seq);
        CREATE INDEX idx_turns_visible_summary_seq
            ON turns(session_id, is_summary, hidden, seq);"
    ))?;
    conn.execute_batch(&format!(
        "CREATE TABLE session_loaded_items_v2 (
            session_id      TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
            kind            TEXT NOT NULL,
            name            TEXT NOT NULL,
            source_turn_id  TEXT,
            created_at      TEXT NOT NULL,
            updated_at      TEXT NOT NULL,
            PRIMARY KEY (session_id, kind, name)
        );
        INSERT INTO session_loaded_items_v2 (
            session_id, kind, name, source_turn_id, created_at, updated_at
        )
        SELECT '{DEFAULT_SESSION_ID}', kind, name, source_turn_id, created_at, updated_at
        FROM session_loaded_items;
        DROP TABLE session_loaded_items;
        ALTER TABLE session_loaded_items_v2 RENAME TO session_loaded_items;
        CREATE INDEX idx_session_loaded_items_source_turn
            ON session_loaded_items(source_turn_id);"
    ))?;
    add_column_if_missing(conn, "queued_prompts", "session_id", "TEXT")?;
    conn.execute(
        "UPDATE queued_prompts SET session_id = ?1 WHERE session_id IS NULL",
        [DEFAULT_SESSION_ID],
    )?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_queued_prompts_session
            ON queued_prompts(session_id, status, seq);",
    )?;
    Ok(())
}

/// v3: stable platform-conversation bindings and platform plugin state.
///
/// Platform session bindings include the persona and optional participant so
/// chat history can remain isolated, while plugin state deliberately excludes
/// both dimensions and is shared by every persona in the external conversation.
fn apply_v3_platform_sessions_and_plugin_state(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE platform_session_bindings (
            platform          TEXT NOT NULL,
            account_id        TEXT NOT NULL,
            conversation_kind TEXT NOT NULL,
            conversation_id   TEXT NOT NULL,
            participant_id    TEXT NOT NULL DEFAULT '',
            persona           TEXT NOT NULL,
            session_id        TEXT NOT NULL UNIQUE
                              REFERENCES sessions(session_id) ON DELETE CASCADE,
            created_at        TEXT NOT NULL,
            updated_at        TEXT NOT NULL,
            PRIMARY KEY (
                platform, account_id, conversation_kind, conversation_id,
                participant_id, persona
            )
        );

        CREATE TABLE platform_plugin_kv (
            plugin_id         TEXT NOT NULL,
            platform          TEXT NOT NULL,
            account_id        TEXT NOT NULL,
            conversation_kind TEXT NOT NULL,
            conversation_id   TEXT NOT NULL,
            key               TEXT NOT NULL,
            value_json        TEXT NOT NULL,
            updated_at        TEXT NOT NULL,
            PRIMARY KEY (
                plugin_id, platform, account_id, conversation_kind,
                conversation_id, key
            )
        );",
    )?;
    Ok(())
}

/// v4: persistent links between platform messages and meme-library entries.
fn apply_v4_platform_meme_refs(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE platform_meme_refs (
            platform          TEXT NOT NULL,
            account_id        TEXT NOT NULL,
            conversation_kind TEXT NOT NULL,
            conversation_id   TEXT NOT NULL,
            message_id        TEXT NOT NULL,
            library           TEXT NOT NULL,
            meme_id           TEXT NOT NULL,
            direction         TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound')),
            created_at        TEXT NOT NULL,
            PRIMARY KEY (
                platform, account_id, conversation_kind, conversation_id,
                message_id, library, meme_id
            )
        );
        CREATE INDEX idx_platform_meme_refs_meme
            ON platform_meme_refs(library, meme_id);",
    )?;
    Ok(())
}

/// v5: durable WebUI user attachments and separate user-visible turn text.
fn apply_v5_user_attachments(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "turns", "display_content", "TEXT NOT NULL DEFAULT ''")?;
    conn.execute(
        "UPDATE turns SET display_content = user_content WHERE display_content = ''",
        [],
    )?;
    conn.execute_batch(
        "CREATE TABLE user_attachments (
            attachment_id TEXT PRIMARY KEY,
            session_id    TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
            turn_id       TEXT REFERENCES turns(turn_id) ON DELETE CASCADE,
            prompt_id     TEXT REFERENCES queued_prompts(prompt_id) ON DELETE CASCADE,
            run_id        TEXT,
            file_name     TEXT NOT NULL,
            mime          TEXT NOT NULL,
            kind          TEXT NOT NULL CHECK (kind IN ('image', 'text')),
            size_bytes    INTEGER NOT NULL CHECK (size_bytes >= 0),
            width         INTEGER NOT NULL DEFAULT 0,
            height        INTEGER NOT NULL DEFAULT 0,
            data          BLOB NOT NULL,
            created_at    TEXT NOT NULL,
            CHECK (
                (turn_id IS NOT NULL) + (prompt_id IS NOT NULL) + (run_id IS NOT NULL) <= 1
            )
        );
        CREATE INDEX idx_user_attachments_session
            ON user_attachments(session_id, created_at, attachment_id);
        CREATE INDEX idx_user_attachments_turn
            ON user_attachments(turn_id, created_at, attachment_id);
        CREATE INDEX idx_user_attachments_prompt
            ON user_attachments(prompt_id, created_at, attachment_id);
        CREATE INDEX idx_user_attachments_run ON user_attachments(run_id);",
    )?;
    Ok(())
}

/// v6: optimistic turn revisions and a bounded replay checkpoint for redoing
/// the last consumed follow-up batch without storing the full conversation.
fn apply_v6_turn_redo_checkpoints(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "turns", "revision", "INTEGER NOT NULL DEFAULT 0")?;
    conn.execute_batch(
        "CREATE TABLE turn_redo_checkpoints (
            turn_id          TEXT PRIMARY KEY REFERENCES turns(turn_id) ON DELETE CASCADE,
            version          INTEGER NOT NULL,
            batch_prompt_ids TEXT NOT NULL,
            payload          BLOB,
            unavailable_reason TEXT,
            created_at       TEXT NOT NULL,
            CHECK ((payload IS NULL) != (unavailable_reason IS NULL))
        );",
    )?;
    create_turn_redo_backup_tables(conn)?;
    Ok(())
}

/// v7 repairs databases that reached v6 before redo failure backups were
/// introduced. Fresh databases already have these tables through v6.
fn apply_v7_turn_redo_backups(conn: &Connection) -> Result<()> {
    create_turn_redo_backup_tables(conn)
}

fn apply_v8_artifact_assets(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS artifact_assets (
            asset_id    TEXT PRIMARY KEY,
            turn_id     TEXT NOT NULL REFERENCES turns(turn_id) ON DELETE CASCADE,
            tool_id     TEXT,
            source_key  TEXT NOT NULL,
            file_name   TEXT NOT NULL,
            mime        TEXT NOT NULL,
            kind        TEXT NOT NULL,
            size_bytes  INTEGER NOT NULL,
            data        BLOB NOT NULL,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL,
            UNIQUE(turn_id, source_key)
        );
        CREATE INDEX IF NOT EXISTS idx_artifact_assets_turn
            ON artifact_assets(turn_id, updated_at, asset_id);",
    )?;
    Ok(())
}

/// v9: durable platform access grants and an append-only audit trail.
///
/// The account scope is deliberately separate from the platform account id:
/// `*` represents a grant shared by every account on a platform, while a
/// concrete id leaves room for narrower policies later without changing the
/// schema. Miyu currently writes only the global scope for QQ.
fn apply_v9_platform_access_control(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS platform_access_grants (
            platform                    TEXT NOT NULL,
            account_scope               TEXT NOT NULL,
            permission                  TEXT NOT NULL,
            subject_kind                TEXT NOT NULL,
            subject_id                  TEXT NOT NULL,
            granted_by_platform         TEXT NOT NULL,
            granted_by_account_id       TEXT NOT NULL,
            granted_by_user_id          TEXT NOT NULL,
            granted_conversation_kind  TEXT NOT NULL,
            granted_conversation_id    TEXT NOT NULL,
            granted_message_id         TEXT NOT NULL,
            created_at                  TEXT NOT NULL,
            PRIMARY KEY (
                platform, account_scope, permission, subject_kind, subject_id
            )
        );

        CREATE TABLE IF NOT EXISTS platform_access_audit (
            audit_id                   TEXT PRIMARY KEY,
            operation                  TEXT NOT NULL,
            platform                   TEXT NOT NULL,
            account_scope              TEXT NOT NULL,
            permission                 TEXT NOT NULL,
            subject_kind               TEXT NOT NULL,
            subject_id                 TEXT NOT NULL,
            actor_platform             TEXT NOT NULL,
            actor_account_id           TEXT NOT NULL,
            actor_user_id              TEXT NOT NULL,
            actor_conversation_kind    TEXT NOT NULL,
            actor_conversation_id      TEXT NOT NULL,
            actor_message_id           TEXT NOT NULL,
            created_at                 TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_platform_access_audit_target
            ON platform_access_audit(
                platform, account_scope, permission, subject_kind, subject_id,
                created_at
            );",
    )?;
    Ok(())
}

/// v10: append-only semantic events for streamed turn recovery.
///
/// The existing columns on `turns` remain the compatibility projection used
/// by completed conversations. These tables are the durable source for a
/// running/interrupted generation, so a partial response never requires
/// rewriting an ever-growing JSON value.
fn apply_v10_turn_generation_journal(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS turn_journal_segments (
            turn_id       TEXT NOT NULL REFERENCES turns(turn_id) ON DELETE CASCADE,
            revision      INTEGER NOT NULL,
            segment_index INTEGER NOT NULL,
            status        TEXT NOT NULL DEFAULT 'running'
                          CHECK (status IN ('running', 'completed', 'interrupted', 'superseded')),
            started_at    TEXT NOT NULL,
            finished_at   TEXT,
            PRIMARY KEY (turn_id, revision, segment_index)
        );
        CREATE INDEX IF NOT EXISTS idx_turn_journal_segments_active
            ON turn_journal_segments(turn_id, revision, status, segment_index);

        CREATE TABLE IF NOT EXISTS turn_journal_events (
            event_id      INTEGER PRIMARY KEY,
            turn_id       TEXT NOT NULL,
            revision      INTEGER NOT NULL,
            segment_index INTEGER NOT NULL,
            kind          TEXT NOT NULL,
            call_id       TEXT,
            name          TEXT,
            text_payload  TEXT,
            blob_payload  BLOB,
            ok            INTEGER,
            created_at    TEXT NOT NULL,
            FOREIGN KEY (turn_id, revision, segment_index)
                REFERENCES turn_journal_segments(turn_id, revision, segment_index)
                ON DELETE CASCADE,
            CHECK (text_payload IS NOT NULL OR blob_payload IS NOT NULL OR kind IN (
                'reasoning_start', 'reasoning_reset', 'reasoning_part_start',
                'reasoning_part_end', 'generation_superseded'
            ))
        );
        CREATE INDEX IF NOT EXISTS idx_turn_journal_events_order
            ON turn_journal_events(turn_id, revision, segment_index, event_id);

        CREATE TABLE IF NOT EXISTS turn_redo_artifact_backups (
            turn_id    TEXT NOT NULL REFERENCES turn_redo_backups(turn_id) ON DELETE CASCADE,
            asset_id   TEXT NOT NULL,
            tool_id    TEXT,
            source_key TEXT NOT NULL,
            file_name  TEXT NOT NULL,
            mime       TEXT NOT NULL,
            kind       TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            data       BLOB NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (turn_id, asset_id)
        );

        INSERT OR IGNORE INTO turn_journal_segments
            (turn_id, revision, segment_index, status, started_at)
        SELECT turn_id, revision, 0,
               CASE status WHEN 'completed' THEN 'completed'
                           WHEN 'interrupted' THEN 'interrupted'
                           ELSE 'running' END,
               COALESCE(user_timestamp, datetime('now'))
        FROM turns
        WHERE status IN ('running', 'interrupted');",
    )?;
    Ok(())
}

fn create_turn_redo_backup_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS turn_redo_backups (
            turn_id    TEXT PRIMARY KEY REFERENCES turns(turn_id) ON DELETE CASCADE,
            revision   INTEGER NOT NULL,
            payload    BLOB NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS turn_redo_question_backups (
            turn_id        TEXT NOT NULL REFERENCES turn_redo_backups(turn_id) ON DELETE CASCADE,
            exchange_index INTEGER NOT NULL,
            payload        TEXT NOT NULL,
            PRIMARY KEY (turn_id, exchange_index)
        );
        CREATE TABLE IF NOT EXISTS turn_redo_image_backups (
            turn_id   TEXT NOT NULL REFERENCES turn_redo_backups(turn_id) ON DELETE CASCADE,
            asset_id  TEXT NOT NULL,
            tool_id   TEXT,
            mime      TEXT NOT NULL,
            width     INTEGER NOT NULL,
            height    INTEGER NOT NULL,
            alt       TEXT NOT NULL,
            data      BLOB NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (turn_id, asset_id)
        );",
    )?;
    Ok(())
}

/// Per-session model pool override: a JSON array of
/// `{"provider_id": ..., "model": ...}` objects. NULL follows the global
/// active pool.
fn apply_v11_session_model_override(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "sessions", "model_override", "TEXT")
}

/// v7 append-only fossilization: the transient system tail that rode behind
/// the user message in the live request (runtime stamp, trusted transport
/// context, hints, associative memory, meme reminder) is archived verbatim so
/// history replay stays a byte-exact extension of what the provider already
/// cached ("注入了就别删"). JSON array of ChatMessage values; '[]' when none.
fn apply_v12_turn_context_messages(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "turns", "context_messages", "TEXT NOT NULL DEFAULT '[]'")
}

/// Compact tail retention: a summary turn no longer swallows every visible
/// turn, so "hidden = seq <= summary_seq" stops describing the folded set.
/// The summary row records the exact turn_ids it hid (JSON array) so undo can
/// restore precisely that set. NULL on legacy rows keeps the old undo path.
fn apply_v13_compact_hidden_turns(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "turns", "compact_hidden_json", "TEXT")
}

/// Mechanical prune (free half of context management): old turns'
/// tool_reports can be replaced by a placeholder because tool output is
/// re-derivable. The original JSON is archived here (write-once) before the
/// first rewrite so the prune is reversible and auditable.
fn apply_v14_tool_reports_archive(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "turns", "tool_reports_archive", "TEXT")
}

/// Unix seconds of the session's most recent completed/interrupted LLM turn.
/// Drives cold-resume pruning: a session idle past the provider cache TTL
/// resumes against a cold cache, so a history rewrite at that moment costs
/// no extra misses — it only shrinks the full-price first request. NULL on
/// legacy sessions means "unknown, skip".
fn apply_v15_session_last_request_at(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "sessions", "last_request_at", "INTEGER")
}

/// Deterministic tool footprint per turn (JSON {read, modified, memories}):
/// file paths and memory names are facts the code knows exactly, so the
/// compactor appends them to summaries itself instead of trusting the LLM to
/// not drop or misspell them. Summary rows carry the merged footprint for
/// cross-compaction accumulation.
fn apply_v16_turn_tool_footprint(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "turns", "tool_footprint", "TEXT")
}

/// Display transcript of a finished turn (JSON: ordered text / tool-call /
/// tool-result records). The live journal tables are wiped when a turn
/// completes because they carry whole command logs; this keeps just enough,
/// in order, for the REPL to redraw a reopened session the way it looked.
fn apply_v17_turn_replay_journal(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "turns", "replay_journal", "TEXT")
}

/// Prompt and cache-read halves of a turn's usage. `token_total` alone cannot
/// express a cache hit rate: hits are an input-side property (output tokens
/// only enter the prompt on the *next* turn), so the rate needs the prompt as
/// its denominator, not the total. Turns recorded before this migration keep
/// zeros, which read as "the provider reported no cache" and so display
/// nothing rather than a fake 0%.
fn apply_v18_turn_cache_tokens(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "turns", "token_prompt", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(
        conn,
        "turns",
        "token_cache_read",
        "INTEGER NOT NULL DEFAULT 0",
    )
}

/// Cache-read half of a subagent run's usage. Subagent sessions already carry
/// prompt/completion/total on the session row; the cumulative cache rate needs
/// the hits too, or folding subagent prompts into the denominator would make a
/// healthy cache read as broken.
///
/// Deliberately nullable: rows written before this migration have an *unknown*
/// cache figure, not a zero one. Defaulting them to 0 would drag their prompt
/// tokens into the rate's denominator with no hits to match — on a real
/// database that turned a measured 24% into 1%. NULL keeps them in the Σ total
/// and out of the rate.
fn apply_v19_session_cache_tokens(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "sessions", "cache_read_tokens", "INTEGER")
}

/// v20:回合的结构化工具流(JSON)。照抄 dsh 的结构保真原则:assistant 的
/// tool_calls(含模型原样 JSON 参数)与各调用的模型侧输出逐字保留,回放时
/// 还原为原生 tool_calls + role:"tool" 消息,不再压扁成系统备忘。
fn apply_v20_turn_tool_flow(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "turns", "tool_flow", "TEXT")
}

/// 「默认会话」实为 shellhook/CLI 快捷入口专属的 lane,改叫「终端集成
/// 会话」;只重命名仍叫旧默认名的行,用户手动改过的名字不动。
fn apply_v21_rename_default_session(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET name = ?1
         WHERE session_id = ?2 AND name IN ('默认会话', 'Default session')",
        rusqlite::params![
            crate::i18n::text("Terminal session", "终端集成会话"),
            DEFAULT_SESSION_ID
        ],
    )?;
    Ok(())
}

/// v22 曾建 goals 表;goal 功能于 08-16 整体移除,此迁移改为空操作
/// (老库的表由 v24 落掉,新库从未建过)。
fn apply_v22_session_goals(_conn: &Connection) -> Result<()> {
    Ok(())
}

/// 归档会话功能整体移除:解冻所有存量归档行,否则它们在没有解档入口的
/// 新版里永远不可见。列本身保留(0 值),避免无谓的表重建。
fn apply_v23_retire_session_archiving(conn: &Connection) -> Result<()> {
    conn.execute("UPDATE sessions SET archived = 0 WHERE archived != 0", [])?;
    Ok(())
}

/// goal 功能整体移除:老库落掉 goals 表。
fn apply_v24_retire_session_goals(conn: &Connection) -> Result<()> {
    conn.execute("DROP TABLE IF EXISTS goals", [])?;
    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(());
        }
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_migrated() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    #[test]
    fn fresh_database_migrates_to_latest_version() {
        let conn = open_migrated();
        let version = user_version(&conn).unwrap();
        assert_eq!(version, MIGRATIONS.last().unwrap().version);
    }

    #[test]
    fn migrations_are_idempotent_on_reopen() {
        let mut conn = open_migrated();
        // A second run must be a no-op.
        run_migrations(&mut conn).unwrap();
        assert_eq!(
            user_version(&conn).unwrap(),
            MIGRATIONS.last().unwrap().version
        );
    }

    #[test]
    fn v7_repairs_v6_database_missing_redo_backup_tables() {
        let mut conn = open_migrated();
        conn.execute_batch(
            "DROP TABLE turn_redo_image_backups;
             DROP TABLE turn_redo_question_backups;
             DROP TABLE turn_redo_backups;
             PRAGMA user_version = 6;",
        )
        .unwrap();

        run_migrations(&mut conn).unwrap();

        assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
        for table in [
            "turn_redo_checkpoints",
            "turn_redo_backups",
            "turn_redo_question_backups",
            "turn_redo_image_backups",
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "missing repaired table: {table}");
        }
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
    }

    #[test]
    fn baseline_converges_legacy_database() {
        // Simulate a legacy pre-versioning database: base turns table without
        // the later ALTER-added columns and user_version 0.
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE turns (
                turn_id          TEXT PRIMARY KEY,
                seq              INTEGER NOT NULL UNIQUE,
                user_content     TEXT NOT NULL,
                user_timestamp   TEXT NOT NULL,
                assistant_content TEXT NOT NULL,
                assistant_reasoning TEXT,
                assistant_timestamp TEXT,
                status           TEXT NOT NULL DEFAULT 'running',
                tool_reports     TEXT NOT NULL DEFAULT '[]'
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO turns (turn_id, seq, user_content, user_timestamp, assistant_content)
             VALUES ('t1', 1, 'hi', 'now', 'hello')",
            [],
        )
        .unwrap();
        run_migrations(&mut conn).unwrap();
        // Legacy row survives and the ALTER-added columns exist with defaults.
        let (hidden, model): (i64, Option<String>) = conn
            .query_row(
                "SELECT hidden, assistant_model FROM turns WHERE turn_id = 't1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(hidden, 0);
        assert_eq!(model, None);
    }

    #[test]
    fn newer_database_version_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 9999).unwrap();
        let err = run_migrations(&mut conn).unwrap_err();
        assert!(err.to_string().contains("newer"));
    }

    #[test]
    fn v2_moves_existing_history_into_the_default_session() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        // Build a v1 database with a turn and a dependent child row.
        conn.pragma_update(None, "user_version", 0).unwrap();
        apply_v1_baseline(&conn).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO turns (turn_id, seq, user_content, user_timestamp, assistant_content)
             VALUES ('t1', 7, 'hi', 'now', 'hello')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO question_exchanges (turn_id, exchange_index, payload)
             VALUES ('t1', 0, '{}')",
            [],
        )
        .unwrap();
        run_migrations(&mut conn).unwrap();

        let (session_id, seq): (String, i64) = conn
            .query_row(
                "SELECT session_id, seq FROM turns WHERE turn_id = 't1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(session_id, DEFAULT_SESSION_ID);
        assert_eq!(seq, 7);
        // The FK-off rebuild must not cascade-delete child rows.
        let exchanges: i64 = conn
            .query_row("SELECT count(*) FROM question_exchanges", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(exchanges, 1);
        let current: String = conn
            .query_row(
                "SELECT value FROM app_state WHERE key = 'current_session'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(current, DEFAULT_SESSION_ID);
        // Per-session seq uniqueness: same seq in another session is fine.
        conn.execute(
            "INSERT INTO sessions (session_id, persona, name, created_at, updated_at)
             VALUES ('other', '', 'x', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content)
             VALUES ('t2', 'other', 7, 'hi', 'now', '')",
            [],
        )
        .unwrap();
        // …but duplicated seq within one session is rejected.
        assert!(conn
            .execute(
                "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content)
                 VALUES ('t3', 'other', 7, 'hi', 'now', '')",
                [],
            )
            .is_err());
    }

    #[test]
    fn v3_platform_tables_enforce_uniqueness_and_session_cascade() {
        let conn = open_migrated();
        assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
        conn.execute(
            "INSERT INTO sessions (session_id, persona, name, created_at, updated_at)
             VALUES ('platform-session', 'miyu', 'platform', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO platform_session_bindings (
                platform, account_id, conversation_kind, conversation_id,
                persona, session_id, created_at, updated_at
             ) VALUES ('onebot', '10000', 'private', '20000', 'miyu',
                       'platform-session', 'now', 'now')",
            [],
        )
        .unwrap();

        // A session cannot be attached to a second external identity.
        assert!(conn
            .execute(
                "INSERT INTO platform_session_bindings (
                    platform, account_id, conversation_kind, conversation_id,
                    persona, session_id, created_at, updated_at
                 ) VALUES ('onebot', '10000', 'private', 'other', 'miyu',
                           'platform-session', 'now', 'now')",
                [],
            )
            .is_err());
        conn.execute(
            "INSERT INTO platform_plugin_kv (
                plugin_id, platform, account_id, conversation_kind,
                conversation_id, key, value_json, updated_at
             ) VALUES ('reply_processor', 'onebot', '10000', 'private',
                       '20000', 'recent_images', '[]', 'now')",
            [],
        )
        .unwrap();

        conn.execute(
            "DELETE FROM sessions WHERE session_id = 'platform-session'",
            [],
        )
        .unwrap();
        let binding_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM platform_session_bindings",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let plugin_count: i64 = conn
            .query_row("SELECT count(*) FROM platform_plugin_kv", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(binding_count, 0);
        // Plugin state is scoped to the external conversation, not a session.
        assert_eq!(plugin_count, 1);
    }

    #[test]
    fn v4_platform_meme_refs_enforce_identity_and_direction() {
        let conn = open_migrated();
        assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
        conn.execute(
            "INSERT INTO platform_meme_refs (
                platform, account_id, conversation_kind, conversation_id,
                message_id, library, meme_id, direction, created_at
             ) VALUES ('onebot', '10000', 'group', '20000', 'message-1',
                       'default', 'meme-1', 'inbound', 'now')",
            [],
        )
        .unwrap();

        assert!(conn
            .execute(
                "INSERT INTO platform_meme_refs (
                    platform, account_id, conversation_kind, conversation_id,
                    message_id, library, meme_id, direction, created_at
                 ) VALUES ('onebot', '10000', 'group', '20000', 'message-1',
                           'default', 'meme-1', 'inbound', 'later')",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO platform_meme_refs (
                    platform, account_id, conversation_kind, conversation_id,
                    message_id, library, meme_id, direction, created_at
                 ) VALUES ('onebot', '10000', 'group', '20000', 'message-2',
                           'default', 'meme-1', 'sideways', 'now')",
                [],
            )
            .is_err());
    }

    #[test]
    fn v4_migrates_an_existing_v3_database_without_losing_platform_state() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_v1_baseline(&conn).unwrap();
        apply_v2_sessions(&conn).unwrap();
        apply_v3_platform_sessions_and_plugin_state(&conn).unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn.execute(
            "INSERT INTO platform_plugin_kv (
                plugin_id, platform, account_id, conversation_kind,
                conversation_id, key, value_json, updated_at
             ) VALUES ('reply_processor', 'onebot', '10000', 'group',
                       '20000', 'recent_images', '[]', 'now')",
            [],
        )
        .unwrap();

        run_migrations(&mut conn).unwrap();

        assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
        let plugin_rows: i64 = conn
            .query_row("SELECT count(*) FROM platform_plugin_kv", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(plugin_rows, 1);
        let meme_table_exists: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'platform_meme_refs'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(meme_table_exists);
    }

    #[test]
    fn v9_creates_platform_access_and_audit_tables() {
        let conn = open_migrated();
        assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
        for table in ["platform_access_grants", "platform_access_audit"] {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM sqlite_master
                        WHERE type = 'table' AND name = ?1
                    )",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "missing access-control table: {table}");
        }
    }
}
