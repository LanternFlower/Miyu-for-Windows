//! 终端连 daemon 的回合驱动。
//!
//! 日常路径：回合跑在 daemon 里，这边通过 IPC 收事件流并渲染。
//! 单次调用与交互式 REPL 生命周期不同，分两个文件。
mod interactive;
mod one_shot;
pub(in crate::cli) use interactive::*;
pub(in crate::cli) use one_shot::*;
