//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/conversation_db/sessions.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ConversationDb {
    pub fn find_session_by_name(&self, persona: &str, name: &str) -> Result<Option<SessionRecord>> {
        self.find_session_by_name_filtered(persona, name, false)
    }
}
