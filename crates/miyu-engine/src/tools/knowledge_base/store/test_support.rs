//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/knowledge_base/store.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]

#[derive(Debug)]
pub struct EditResult {
    pub path: String,
    pub old_line_count: usize,
    pub new_line_count: usize,
    pub semantic_refreshed: bool,
}
