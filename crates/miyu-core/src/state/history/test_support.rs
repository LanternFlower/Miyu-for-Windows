//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/history.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl StateStore {
    pub fn load_session_loaded_tools_with_sources(&self) -> Result<Vec<(String, Option<String>)>> {
        self.conv_db
            .load_session_loaded_items_with_sources(&self.session(), "tool")
    }
}
