//! `miyu-hosts`:Miyu 的第 场所与展示 层(09-16 拆 crate)。模块归属见 `test_scripts/arch_dep_check.py` 的 `TIERS`。
// 09-16 接口治理收口:死代码在生产构建里是警告,不再全局豁免;测试构建里 cfg(test) 的辅助函数照旧豁免。
#![cfg_attr(test, allow(dead_code))]

pub mod daemon;
pub mod platforms;
pub mod render;
pub mod runtime;
pub mod web;
// 场所层仍有少量旧根 crate 调用点；实现已下沉到 miyu-base，这里只留兼容别名。
pub use miyu_base::process as process_command;
pub use miyu_base::{platform_dirs, sys};
