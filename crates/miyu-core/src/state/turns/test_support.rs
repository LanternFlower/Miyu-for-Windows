//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/turns.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl StateStore {
    pub fn append_persisted_context(&self, turn_id: &str, report: &str) -> Result<()> {
        self.conv_db.append_tool_report(turn_id, report.trim())
    }
}
