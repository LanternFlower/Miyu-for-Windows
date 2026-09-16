//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/memory/write.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl MemoryStore {
    pub fn remember_pending_event(
        &self,
        user_message: &str,
        assistant_message: &str,
    ) -> Result<()> {
        if !self.config.enabled || !self.writes_enabled || !self.config.auto_diary_enabled {
            return Ok(());
        }
        self.init()?;
        self.data_conn()?.execute(
            "INSERT INTO pending_events (
                user_message, assistant_message, created_at, origin_session_id
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                user_message.trim(),
                assistant_message.trim(),
                now(),
                self.write_session_id(),
            ],
        )?;
        Ok(())
    }

    pub fn flush_pending_events(&self) -> Result<()> {
        if !self.config.enabled || !self.config.auto_diary_enabled {
            return Ok(());
        }
        self.init()?;
        let conn = self.data_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, user_message, assistant_message, created_at, origin_session_id
               FROM pending_events WHERE processed_at IS NULL ORDER BY id LIMIT 20",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (id, user, assistant, created_at, origin_session_id) = row?;
            let content = diary_content(&created_at, &user, &assistant);
            let expires_at = (Utc::now()
                + ChronoDuration::days(self.config.short_diary_retention_days as i64))
            .to_rfc3339();
            // 会话标记跟着待处理事件走,不取当前环境:消化是后台批处理,
            // 此刻的"当前会话"跟这条事件的来源没有关系。
            conn.execute(
                "INSERT INTO episodes (
                    content, source, status, recall_count, created_at, updated_at,
                    retention, user_message, assistant_message, expires_at, origin_session_id
                 ) VALUES (?1, 'episode', 'active', 0, ?2, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    content,
                    created_at,
                    SHORT_TERM,
                    user,
                    assistant,
                    expires_at,
                    origin_session_id,
                ],
            )?;
            conn.execute(
                "UPDATE pending_events SET processed_at=?1 WHERE id=?2",
                params![now(), id],
            )?;
        }
        Ok(())
    }
}
