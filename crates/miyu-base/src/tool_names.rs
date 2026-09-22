//! 工具名的纯判定。
//!
//! 「哪些名字是命令类工具」「带后缀的事件名归到哪个工具 id」都只认字符串,
//! 不碰注册表、不碰配置。渲染层要它做命令块判定,中转线桥要它判 tool 输出形态,
//! 工具层自己也要它——住在工具层就逼着中转线反过来 use 工具层。放在基础层(09-16)。
//!
//! `tools` 那两处留了 `pub(crate) use crate::tool_names::…` 的再导出,
//! `miyu_engine::tools::is_command_tool` / `tool_event_base_name` 老路径一字未改。

/// 命令类工具:输出按命令块渲染,中转线也按它判 tool 输出形态。工具名是 core
/// 的事实,渲染层与中转线都只引用这里(09-16 起 agent 不再反向 use render)。
pub fn is_command_tool(name: &str) -> bool {
    matches!(name, "run_command" | "Bash")
}

/// 事件名 → 工具底名:`divine:塔罗` 这类带后缀的事件名归到注册的工具 id 上
/// (09-16 从 render 搬来:这是工具元数据,渲染层只是消费者)。
pub fn tool_event_base_name(name: &str) -> &str {
    if name.starts_with("divine:") {
        "divine"
    } else if name.starts_with("use_meme:") {
        "use_meme"
    } else if name.starts_with("load_skill:") {
        "load_skill"
    } else if name.starts_with("load_tools:") {
        "load_tools"
    } else if name.starts_with("subagent:") {
        "subagent"
    // 改名前的事件名(task:<描述>)还留在历史记录里。
    } else if name.starts_with("task:") {
        "task"
    } else {
        name
    }
}
