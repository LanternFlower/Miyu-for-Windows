//! 发往 opencode Zen 的工具名别名。
//!
//! 09-20 起 Zen 免费档按请求体里的工具名认客户端:`stream:true` 且 tools 里
//! 同时有 `shell` 和 `read` 才放行(见 `zen_tools` 的模块注释,判据是对真端点
//! 六十多次对照实验切出来的)。这里守三件事:去程改对了、缺的补上了、回程换
//! 得回来;以及非 Zen 端点一个字节都不动。

use super::shared::*;
use crate::llm::openai_compatible::zen_tools;
use crate::llm::{
    ChatMessage, ChatStreamChunk, ChatStreamKind, FunctionDefinition, ToolCall, ToolCallFunction,
    ToolDefinition,
};
use miyu_base::default_models::{OPENCODE_ZEN_BASE_URL, OPENCODE_ZEN_GO_BASE_URL};

fn tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        kind: "function",
        function: FunctionDefinition {
            name: name.to_string(),
            description: format!("{name} 的描述"),
            parameters: serde_json::json!({"type":"object","properties":{}}),
        },
    }
}

fn names(tools: &[ToolDefinition]) -> Vec<&str> {
    tools
        .iter()
        .map(|tool| tool.function.name.as_str())
        .collect()
}

fn call(name: &str) -> ToolCall {
    ToolCall {
        id: "call_1".to_string(),
        kind: "function".to_string(),
        function: ToolCallFunction {
            name: name.to_string(),
            arguments: "{}".to_string(),
        },
    }
}

/// 去程:两件工具都在时就地改名,别的工具一个不动,也不会多补占位。
#[test]
fn zen_renames_the_two_recognised_tools_in_place() {
    let provider = test_provider("myopencode", OPENCODE_ZEN_BASE_URL);
    let mut tools = vec![
        tool("run_command"),
        tool("web_search"),
        tool("read"),
        tool("use_meme"),
    ];
    zen_tools::lower_tools(&provider, &mut tools);
    assert_eq!(names(&tools), ["shell", "web_search", "read", "use_meme"]);
    // 描述与参数不参与判定,不该被占位声明覆盖掉。
    assert_eq!(tools[0].function.description, "run_command 的描述");
}

/// 工具面里没有这两件时(受限场所底座、空壳档、judge 这类不带工具的辅助轮)
/// 补占位,否则整条请求进不去。补出来的顺序固定,免得每轮抖动掰断缓存前缀。
#[test]
fn zen_appends_placeholders_when_the_tool_face_lacks_them() {
    let provider = test_provider("opencode", OPENCODE_ZEN_BASE_URL);

    let mut none = Vec::new();
    zen_tools::lower_tools(&provider, &mut none);
    assert_eq!(names(&none), ["shell", "read"]);

    let mut half = vec![tool("read"), tool("get_current_time")];
    zen_tools::lower_tools(&provider, &mut half);
    assert_eq!(names(&half), ["read", "get_current_time", "shell"]);
}

/// Console Go(`/zen/go/v1`)是同一个网关的另一个端点,同样要处理。
#[test]
fn console_go_is_treated_as_a_zen_endpoint() {
    let provider = test_provider("opencodego", OPENCODE_ZEN_GO_BASE_URL);
    let mut tools = vec![tool("run_command")];
    zen_tools::lower_tools(&provider, &mut tools);
    assert_eq!(names(&tools), ["shell", "read"]);
}

/// 非 Zen 端点原样放过:别名只为过那道闸,别的供应商见到 `shell` 只会困惑。
#[test]
fn non_zen_providers_are_untouched() {
    let provider = test_provider("deepseek", "https://api.deepseek.com/v1");
    let mut tools = vec![tool("run_command"), tool("read")];
    zen_tools::lower_tools(&provider, &mut tools);
    assert_eq!(names(&tools), ["run_command", "read"]);

    let mut messages = vec![ChatMessage::assistant("", Some(vec![call("run_command")]))];
    zen_tools::lower_messages(&provider, &mut messages);
    assert_eq!(
        messages[0].tool_calls.as_ref().unwrap()[0].function.name,
        "run_command"
    );

    let restored = zen_tools::restore_calls(&provider, vec![call("shell")]);
    assert_eq!(restored[0].function.name, "shell");
}

/// 历史里的 `tool_calls` 要跟着清单一起改名,不然清单报 `shell`、回放叫
/// `run_command`,两边对不上。
#[test]
fn zen_renames_tool_calls_replayed_from_history() {
    let provider = test_provider("opencode", OPENCODE_ZEN_BASE_URL);
    let mut messages = vec![
        ChatMessage::system("跑一下 ls"),
        ChatMessage::assistant(
            "",
            Some(vec![call("run_command"), call("read"), call("use_meme")]),
        ),
    ];
    zen_tools::lower_messages(&provider, &mut messages);
    let replayed = messages[1].tool_calls.as_ref().unwrap();
    assert_eq!(
        replayed
            .iter()
            .map(|call| call.function.name.as_str())
            .collect::<Vec<_>>(),
        ["shell", "read", "use_meme"]
    );
}

/// 回程:收口那批调用换回 Miyu 自己的名字,回合层认的是这套名字。
#[test]
fn zen_restores_tool_call_names_on_the_way_back() {
    let provider = test_provider("opencode", OPENCODE_ZEN_BASE_URL);
    let restored = zen_tools::restore_calls(
        &provider,
        vec![call("shell"), call("read"), call("web_search")],
    );
    assert_eq!(
        restored
            .iter()
            .map(|call| call.function.name.as_str())
            .collect::<Vec<_>>(),
        ["run_command", "read", "web_search"]
    );
}

/// 回程:流式里那条「工具名已解码」的 chunk 也要换,否则界面上先闪一下
/// `shell` 再变成 `run_command`。别的 chunk 不碰。
#[test]
fn zen_restores_the_streamed_tool_name_chunk() {
    let provider = test_provider("opencode", OPENCODE_ZEN_BASE_URL);
    let restored = zen_tools::restore_chunk(
        &provider,
        ChatStreamChunk {
            kind: ChatStreamKind::ToolCall,
            text: "read".to_string(),
        },
    );
    assert_eq!(restored.text, "read");

    // 正文里出现 "read" 只是正文。
    let content = zen_tools::restore_chunk(
        &provider,
        ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: "read".to_string(),
        },
    );
    assert_eq!(content.text, "read");
}

/// 用户 09-21 在 normal 模式 + opencodego/deepseek-v4.1-flash 上撞到：连着三次
/// `tool error: unknown tool: read_file (did you mean: read?)`。
///
/// Miyu 的工具**就叫 `read`**，仓库里没有任何一件叫 `read_file`（`read_file`
/// 是 08-21 三域合并里被改掉的旧名）。别名表左列本该是「Miyu 自己的工具名」，
/// 这一条却填了那个死名，于是回程把模型正确调用的 `read` 改写成不存在的
/// `read_file`——模型每次都是对的，每次都被判错，死循环。
#[test]
fn a_read_call_from_zen_comes_back_as_read_not_a_retired_name() {
    let provider = test_provider("opencodego", OPENCODE_ZEN_GO_BASE_URL);
    let restored = zen_tools::restore_calls(&provider, vec![call("read")]);
    assert_eq!(
        restored[0].function.name, "read",
        "模型调的 read 被改成了别的名字，回合层会当成 unknown tool"
    );

    let chunk = zen_tools::restore_chunk(
        &provider,
        ChatStreamChunk {
            kind: ChatStreamKind::ToolCall,
            text: "read".to_string(),
        },
    );
    assert_eq!(chunk.text, "read");
}

/// 去程:真实工具面里那件 `read` 原样留下,不该被改名,也不该再补一个占位。
#[test]
fn the_real_read_tool_survives_the_outbound_rename() {
    let provider = test_provider("opencodego", OPENCODE_ZEN_GO_BASE_URL);
    let mut tools = vec![tool("run_command"), tool("read"), tool("web_search")];
    zen_tools::lower_tools(&provider, &mut tools);
    assert_eq!(names(&tools), ["shell", "read", "web_search"]);
    assert_eq!(
        tools[1].function.description, "read 的描述",
        "真工具的描述被占位声明盖掉了"
    );
}
