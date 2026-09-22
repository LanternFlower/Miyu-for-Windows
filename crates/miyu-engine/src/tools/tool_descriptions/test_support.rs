//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/tool_descriptions.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

#[derive(Debug, Clone, Deserialize)]
pub struct ToolGroupDescription {
    pub summary: String,
}

pub static TOOL_GROUPS: OnceLock<HashMap<String, ToolGroupDescription>> = OnceLock::new();

pub const TOOL_GROUPS_RAW: &str = include_str!("../../../../../src/tools/descriptions/groups.json");

pub fn groups() -> &'static HashMap<String, ToolGroupDescription> {
    TOOL_GROUPS.get_or_init(|| {
        serde_json::from_str(TOOL_GROUPS_RAW).expect("tool group description JSON must be valid")
    })
}

pub fn group_names() -> Vec<&'static str> {
    groups().keys().map(String::as_str).collect()
}
