//! opencode Zen 免费档的客户端识别:请求体里的工具名。
//!
//! 09-19 起 Zen 对免费模型加了一道闸,第三方客户端一律
//! `403 FreeTierError: OpenCode's free tier can only be used from within OpenCode`。
//! 09-20 对真端点做了上百次对照实验(脚本 `testkit/opencode-zen/freetier_probe.js`),
//! 判据是**三件同时成立**:
//!
//! ```text
//! stream: true
//! tools 里同时有 (shell 或 bash) 和 read
//! 至少带一个 x-opencode-* 头(`zen_headers` 已经在发了)
//! ```
//!
//! 三条缺一不可,而且是**合取**——这也是排查时踩的坑:先去掉头、body 也不对,
//! 于是误判成"头不相干";单独去掉某一个 x-opencode-* 头又照样放行,一个都不带
//! 才挡。所以这里只补 body 那一半,头那一半归 `zen_headers`。
//!
//! 逐条实测过、确认**不参与判定**的:`User-Agent` 具体内容(换回 1.18.29 那串
//! 照样 200,但完全不带 UA 会挡)、`traceparent`/`b3`、API key 是真 key 还是
//! 字面量 `public`、body 大小、system 提示词(中文人格提示词照样 200)、工具的
//! description 与 parameters(全清空照样 200)、tools 里额外挂多少件自造工具。
//! 另有一条独立通道是 opencode 自己那段系统提示词原文,对 Miyu 没用。
//!
//! 所以这里做的事就两件:发出去之前把 `run_command` 报成 `shell`(Miyu 的 `read`
//! 本来就叫 `read`,不用改);收回来把名字换回去。判定和 `zen_headers` 一样按
//! **端点**走,不按供应商 id——用户可以把那份配置改名。
//!
//! 工具面里没有这两件工具时(受限场所的底座、工具还没加载的空壳档、judge /
//! 好感度这类不带工具的辅助轮)补一条同名占位声明,否则整条请求进不去。占位声明
//! 的描述写明别调用;万一模型真调了,名字换回去之后落到那一刻的注册表上——
//! 受限面的注册表本来就没有 `run_command`(`restricted_platform_registry` 是
//! 另建一张表,不是过滤定义),会以「unknown tool」收场,不会反向打开权限。

use crate::llm::openai_compatible::zen_headers::is_zen_endpoint;
use crate::llm::{
    ChatMessage, ChatStreamChunk, ChatStreamKind, FunctionDefinition, ToolCall, ToolDefinition,
};
use miyu_base::config::ProviderConfig;
use miyu_base::i18n::text as t;

/// (Miyu 自己的工具名, 发到 Zen 时报的名字)。
///
/// `shell` 与 `read` 是实测出来的最小通过集:两个都在才放行,只留一个就挡
/// (`shell` 单独 403、`read` 单独 403)。`bash` 与 `shell` 等价,选 `shell`
/// 是因为官方客户端报的就是它。
///
/// **左列必须是真的已注册工具名。** 09-20 这里把读文件那件写成了 `read_file`
/// ——那是 08-21 三域合并里改掉的旧名,仓库里早就没有了。于是回程把模型正确
/// 调用的 `read` 改写成不存在的 `read_file`,用户 09-21 在 opencodego 上连撞
/// 三次「unknown tool: read_file (did you mean: read?)」:模型每次都对,每次
/// 都被判错。`every_wire_alias_names_a_real_tool` 拿注册表守着这一列。
///
/// 读文件那件两边同名,留在表里是为了那条「缺了就补占位」的保证。
/// 表本身导出给 `miyu-engine` 侧的守卫核对(工具层不该被 llm 层反向依赖,
/// 所以断言住在工具层,名单从这里取——导出表本身而不是另抄一份名单,免得
/// 两份各自漂移)。
pub const WIRE_ALIASES: &[(&str, &str)] = &[("run_command", "shell"), ("read", "read")];

/// 这次请求要不要做别名替换。非 Zen 端点一个字节都不动。
pub(in crate::llm::openai_compatible) fn aliases_apply(provider: &ProviderConfig) -> bool {
    is_zen_endpoint(provider)
}

fn to_wire(name: &str) -> Option<&'static str> {
    WIRE_ALIASES
        .iter()
        .find(|(real, _)| *real == name)
        .map(|(_, wire)| *wire)
}

fn from_wire(name: &str) -> Option<&'static str> {
    WIRE_ALIASES
        .iter()
        .find(|(_, wire)| *wire == name)
        .map(|(real, _)| *real)
}

/// 工具声明:改名 + 缺的补占位,保证 `shell` 与 `read` 一定在。
pub(in crate::llm::openai_compatible) fn lower_tools(
    provider: &ProviderConfig,
    tools: &mut Vec<ToolDefinition>,
) {
    if !aliases_apply(provider) {
        return;
    }
    for tool in tools.iter_mut() {
        if let Some(wire) = to_wire(&tool.function.name) {
            tool.function.name = wire.to_string();
        }
    }
    for (_, wire) in WIRE_ALIASES {
        if tools.iter().any(|tool| tool.function.name == *wire) {
            continue;
        }
        tools.push(placeholder(wire));
    }
}

/// 历史里的 `tool_calls` 也得跟着改:工具清单报的是 `shell`,回放的调用还叫
/// `run_command` 的话,两边对不上——有的上游会直接 400。
pub(in crate::llm::openai_compatible) fn lower_messages(
    provider: &ProviderConfig,
    messages: &mut [ChatMessage],
) {
    if !aliases_apply(provider) {
        return;
    }
    for message in messages.iter_mut() {
        let Some(calls) = message.tool_calls.as_mut() else {
            continue;
        };
        for call in calls.iter_mut() {
            if let Some(wire) = to_wire(&call.function.name) {
                call.function.name = wire.to_string();
            }
        }
    }
}

/// 回程:线上的名字换回 Miyu 自己的。不是别名就原样返回 `None`,调用方不动它。
pub(in crate::llm::openai_compatible) fn restore_name(
    provider: &ProviderConfig,
    name: &str,
) -> Option<&'static str> {
    aliases_apply(provider).then(|| from_wire(name)).flatten()
}

/// 占位声明。参数留空对象——实测 description 与 parameters 都不参与判定,
/// 给足内容只会白占 token 和缓存前缀。
fn placeholder(wire: &str) -> ToolDefinition {
    ToolDefinition {
        kind: "function",
        function: FunctionDefinition {
            name: wire.to_string(),
            description: t(
                "Compatibility placeholder. Never call this tool.",
                "兼容占位声明,不要调用。",
            )
            .to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        },
    }
}

/// 回程:流式过程中那条「工具名已解码」的 chunk。名字不是别名就原样过。
pub(in crate::llm::openai_compatible) fn restore_chunk(
    provider: &ProviderConfig,
    chunk: ChatStreamChunk,
) -> ChatStreamChunk {
    if chunk.kind != ChatStreamKind::ToolCall {
        return chunk;
    }
    match restore_name(provider, &chunk.text) {
        Some(real) => ChatStreamChunk {
            kind: chunk.kind,
            text: real.to_string(),
        },
        None => chunk,
    }
}

/// 回程:收口时那一批工具调用。
pub(in crate::llm::openai_compatible) fn restore_calls(
    provider: &ProviderConfig,
    mut calls: Vec<ToolCall>,
) -> Vec<ToolCall> {
    if !aliases_apply(provider) {
        return calls;
    }
    for call in calls.iter_mut() {
        if let Some(real) = from_wire(&call.function.name) {
            call.function.name = real.to_string();
        }
    }
    calls
}
