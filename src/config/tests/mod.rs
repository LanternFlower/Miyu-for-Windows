//! 配置的测试，按被测主题分文件。
//!
//! 原本是一个两千多行的 `mod tests`。这里几乎每条都是「某个版本迁移过来还能读」
//! 或「某个默认值不许变」，所以分组按配置的领域走。

mod shared;
mod provider;
mod platform;
mod plugins;
mod quota;
mod paths;
mod defaults;
