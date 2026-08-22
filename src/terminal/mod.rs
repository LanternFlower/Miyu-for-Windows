//! 终端输出原语。
//!
//! 这里放的是"怎么把东西画到终端上"的底层能力，既不属于工具层也不属于渲染
//! 层——两边都要用：
//!
//! - `kitty` 的图形协议既服务于 `print_image` 这类工具，也服务于公式渲染；
//! - `CommandOutputStream` 由命令执行产出、由渲染层消费。
//!
//! 放在基础层，两边都往下依赖，方向一致。
pub(crate) mod kitty;

/// 命令输出来自哪条流。
///
/// 渲染层据此决定颜色与前缀（stderr 要显眼），工具层据此把两条流分开收集。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOutputStream {
    Stdout,
    Stderr,
}
