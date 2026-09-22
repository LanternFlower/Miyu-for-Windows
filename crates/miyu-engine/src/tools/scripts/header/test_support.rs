//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/scripts/header.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub fn extract_description(raw: &str) -> Option<String> {
    select_script_description(&extract_metadata(raw).descriptions)
}

pub fn description_from_script(path: &Path) -> Option<String> {
    select_script_description(&metadata_from_script(path).descriptions)
}
