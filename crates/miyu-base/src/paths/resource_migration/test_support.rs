//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/paths/resource_migration.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub fn migrate_resource_layout(layout: &Layout) -> Result<()> {
    if !try_migrate_resource_layout(layout, false)? {
        bail!("Miyu resource migration is deferred while another daemon or starter is active");
    }
    Ok(())
}

pub fn write_resource_journal(layout: &Layout, journal: &ResourceMigrationJournal) -> Result<()> {
    write_journal_at(&layout.resource_journal(), journal)
}
