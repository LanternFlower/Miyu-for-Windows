//! 建库与迁移。
//!
//! `SCHEMA_VERSION` 只增不减，`migrate` 从任意旧版本一路走到当前版本。

use crate::platforms::plugins::message_history::store::*;

pub(crate) const SCHEMA_VERSION: i64 = 4;

pub(crate) fn open_database(db_path: &Path) -> Result<Connection> {
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

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
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
