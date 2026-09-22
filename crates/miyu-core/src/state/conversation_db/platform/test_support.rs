//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/conversation_db/platform.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ConversationDb {
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

    /// Records the model identity and token usage a subagent session actually
    /// used (audit columns on `sessions`).
    /// Writes a subagent row the way builds before v19 did: usage present,
    /// `cache_read_tokens` left NULL.
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
}
