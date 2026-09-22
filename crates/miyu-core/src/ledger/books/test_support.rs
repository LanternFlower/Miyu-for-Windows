//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/ledger/books.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl LedgerDb {
    pub fn archive_account(&self, account_id: &str, archived: bool) -> Result<()> {
        self.with_tx(|tx| {
            let changed = tx.execute(
                "UPDATE ledger_accounts SET archived = ?2, updated_at = ?3 WHERE account_id = ?1",
                params![account_id, i64::from(archived), now_rfc3339()],
            )?;
            if changed == 0 {
                bail!("account {account_id} not found");
            }
            Ok(())
        })
    }
}
