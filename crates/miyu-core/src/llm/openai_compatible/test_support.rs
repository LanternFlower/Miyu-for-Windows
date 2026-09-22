//! 跨 crate 测试夹具（`testkit` 特性下编译，生产构建里零字节）。
//!
//! 供应商方言整形在生产构建里是 `openai_compatible` 私有的，但它要跟**工具面**
//! 一起验：`miyu-engine` 的注册表 × Qwen 方言契约。工具面住在 engine、整形函数
//! 住在 core，core 反向依赖 engine 会成环，所以这里开一扇 testkit 的窗。

use crate::llm::openai_compatible::*;
use crate::llm::ToolDefinition;

/// Qwen 方言的测试供应商。识别只看 id / display_name / base_url
/// （见 `tool_schema::provider_uses_qwen_tool_schema`），这里给最小的一份。
pub fn qwen_tool_provider() -> ProviderConfig {
    serde_json::from_value(serde_json::json!({
        "id": "Bailian",
        "display_name": "Bailian",
        "base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "protocol": "auto",
    }))
    .expect("provider fixture")
}

/// 发请求前的工具面整形（生产路径上由 `chat` 在组请求时调用）。
///
/// 名字带 `qwen` 是为了避开生产函数 `tool_schema::lower_chat_tools_for_provider`
/// ——两者会被同一个 glob 导入拉进同一作用域。
pub fn lower_chat_tools_for_qwen(
    provider: &ProviderConfig,
    tools: Vec<ToolDefinition>,
) -> Vec<ToolDefinition> {
    super::tool_schema::lower_chat_tools_for_provider(provider, tools)
}
