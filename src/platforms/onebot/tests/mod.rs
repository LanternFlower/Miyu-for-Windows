//! OneBot 平台的测试，按被测主题分文件。
//!
//! 原本是一个近四千行的 `mod tests`，里面混着连接、解析、准入、投递、
//! 文件、入群审核六件互不相干的事。

mod shared;
mod connection;
mod parsing;
mod identity;
mod admission;
mod requests;
mod notices;
mod files;
mod delivery;
mod turn;
