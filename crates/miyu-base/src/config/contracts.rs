//! 扩展契约版本(09-16 接口治理 Phase 7)。
//!
//! 每种外装扩展按哪一版契约写,这里是唯一真相源:包管理器 `requires-contracts`
//! 预检、`miyu host host.info` 都从这里报。v1 = 2026-09 的 as-built(`docs/interfaces/`);
//! 契约只加不删,改缺省值或删字段才升 major。

/// 宿主支持的扩展契约版本。包声明的版本 ≤ 这里的版本才能装。
pub const SUPPORTED_CONTRACTS: &[(&str, u32)] =
    &[("scripts", 1), ("skills", 1), ("persona-manifest", 1)];
