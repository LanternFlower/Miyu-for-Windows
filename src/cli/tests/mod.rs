//! CLI 层的测试，按被测主题分文件。
//!
//! 原本挤在一个叫 `repl_input_tests` 的两千多行模块里，而里面测的其实是
//! 参数解析、日志格式化、footer、活动区、菜单——名字名不副实。
mod daemon_log;
mod cli_args;
mod footer_tail;
mod pop_menu;
mod variant_menu;
mod slash_commands;
mod input_editing;
mod shared;
