//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/scripts/index.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub fn auto_detect_script(path: &Path) -> Option<ScriptEntry> {
    let detected = inspect_script(path)?;
    let id = detected.id.clone()?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let entry = entry_from_detected(&detected, id, file_name);
    (!entry.description.is_empty()).then_some(entry)
}
