//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/conversation_db/attachments.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ConversationDb {
    /// 旧式 BLOB 存法:内容进 `data` 列。保留给测试与小附件;WebUI 上传
    /// 一律走 `insert_user_attachment_file`。
    pub fn insert_user_attachment(
        &self,
        session_id: &str,
        attachment: &UserAttachment,
        data: &[u8],
    ) -> Result<()> {
        self.insert_user_attachment_row(session_id, attachment, Some(data))
    }
}
