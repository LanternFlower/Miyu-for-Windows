//! `miyu-core`:Miyu 的第 存储与协议/子系统/传输 层(09-16 拆 crate)。模块归属见 `test_scripts/arch_dep_check.py` 的 `TIERS`。
// 09-16 接口治理收口:死代码在生产构建里是警告,不再全局豁免;测试构建里 cfg(test) 的辅助函数照旧豁免。
#![cfg_attr(test, allow(dead_code))]

pub mod alarm;
pub mod args;
pub mod ipc;
pub mod ledger;
pub mod llm;
pub mod memory;
pub mod persona_hint;
pub mod skills;
pub mod slash_commands;
pub mod state;

// Compatibility aliases for modules that moved into `miyu-base` during the
// crate split. Keep one implementation and let the mechanically moved core
// modules continue to use their former `crate::...` paths.
pub use miyu_base::platform_dirs;
pub use miyu_base::process as process_command;
pub use miyu_base::sys;
