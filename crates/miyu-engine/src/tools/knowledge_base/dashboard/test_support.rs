//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/knowledge_base/dashboard.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl KnowledgeBase {
    pub fn dashboard_root(&self) -> &Path {
        &self.root
    }
}
