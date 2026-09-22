//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/web/attachments.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

/// 内存版探测,供测试与小附件;上传路径走 `inspect_user_attachment_file`。
pub(in crate::web) fn inspect_user_attachment(
    file_name: &str,
    bytes: &[u8],
) -> std::result::Result<(String, String, u32, u32), ApiError> {
    inspect_attachment_reader(file_name, std::io::Cursor::new(bytes), bytes.len() as u64)
}
