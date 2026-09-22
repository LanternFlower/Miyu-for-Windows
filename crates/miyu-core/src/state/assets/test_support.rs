//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/assets.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl StateStore {
    pub fn save_user_attachment(&self, attachment: &UserAttachment, data: &[u8]) -> Result<()> {
        self.conv_db
            .insert_user_attachment(&self.session(), attachment, data)
    }

    /// 落盘附件的目标文件路径。
    pub fn user_attachment_path(&self, attachment: &UserAttachment) -> std::path::PathBuf {
        self.conv_db.attachment_path(attachment)
    }
}
