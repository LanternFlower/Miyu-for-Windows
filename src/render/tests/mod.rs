//! 渲染层的测试，按被测部件分文件。
//!
//! 原本是一个两千多行的 `mod tests`。这里的断言大多是「终端里长什么样」，
//! 所以分组按部件走：命令块、Markdown、表格、工具摘要、推理计时。

mod shared;
mod command;
mod markdown;
mod table;
mod patch;
mod tool_summary;
mod reasoning;
mod usage;
mod math;
