//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/paths/legacy_migration.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub fn daemon_is_running_at(runtime_dir: &Path, current_process_is_daemon: bool) -> bool {
    std::os::unix::net::UnixStream::connect(runtime_dir.join("core.sock")).is_ok()
        || runtime_lock_is_held(&runtime_dir.join("core.lock"))
        || (!current_process_is_daemon && runtime_lock_is_held(&runtime_dir.join("starter.lock")))
}

pub fn migrate_entry(source: &Path, destination: &Path) -> Result<()> {
    let mappings = existing_mappings(&[MigrationMapping::new(source, destination)])?;
    preflight_mappings(&mappings)?;
    let Some(mapping) = mappings.first() else {
        return Ok(());
    };
    migrate_entry_unchecked(&mapping.source, &mapping.destination)
}
