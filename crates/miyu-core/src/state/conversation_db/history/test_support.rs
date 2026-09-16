//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/conversation_db/history.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ConversationDb {
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
}
