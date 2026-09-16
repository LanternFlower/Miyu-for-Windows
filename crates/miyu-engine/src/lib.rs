//! `miyu-engine`:Miyu 的第 工具与引擎 层(09-16 拆 crate)。模块归属见 `test_scripts/arch_dep_check.py` 的 `TIERS`。
// 09-16 接口治理收口:死代码在生产构建里是警告,不再全局豁免;测试构建里 cfg(test) 的辅助函数照旧豁免。
#![cfg_attr(test, allow(dead_code))]

pub mod agent;
pub mod default_kb;
pub mod tools;
pub mod transfer;
#[cfg(feature = "voice")]
pub mod voice;
