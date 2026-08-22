//! 状态层的测试，按被测领域分文件。
//!
//! 原本是一个三千行的 `mod tests`。这一层的断言几乎都是「崩在中间也不能坏」，
//! 所以分组按事务边界走：回合、会话、平台、队列、压缩、重做。

mod shared;
mod turns;
mod sessions;
mod platform;
mod assets;
mod queue;
mod compact;
mod redo;
mod goals;
