//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/conversation_db/turns.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ConversationDb {
    /// 追加一条工具报告（v25 起走 `turn_tool_reports` 子表）。
    ///
    /// 一次 INSERT，**没有读**。原来是「读整列 → 解析 → push → 整个序列化 →
    /// 写回」，第 k 次追加要写回当前全部 k 条，总写入 O(N²)。
    ///
    /// 顺序由 `report_id` 自增保证，所以连 `MAX(seq)` 都不用查。
    pub fn append_tool_report(&self, turn_id: &str, report: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO turn_tool_reports (turn_id, report) VALUES (?1, ?2)",
            params![turn_id, report],
        )?;
        Ok(())
    }
}
