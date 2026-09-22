//! Provider-specific tool schema compatibility.
//!
//! Miyu keeps the complete JSON Schema in its tool registry. Some OpenAI-compatible
//! gateways accept only a narrower wire dialect, so compatibility changes belong at
//! this request boundary instead of weakening every tool definition globally.

use crate::llm::openai_compatible::*;

pub(in crate::llm::openai_compatible) fn lower_chat_tools_for_provider(
    provider: &ProviderConfig,
    mut tools: Vec<ToolDefinition>,
) -> Vec<ToolDefinition> {
    if !provider_uses_qwen_tool_schema(provider) {
        return tools;
    }
    for tool in &mut tools {
        tool.function.parameters = qwen_tool_input_schema(openai_tool_input_schema(
            std::mem::take(&mut tool.function.parameters),
        ));
    }
    tools
}

pub(in crate::llm::openai_compatible) fn lower_responses_tools_for_provider(
    provider: &ProviderConfig,
    tools: Vec<ToolDefinition>,
) -> Vec<Value> {
    tools
        .into_iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.function.name,
                "description": tool.function.description,
                "parameters": responses_tool_input_schema_for_provider(
                    provider,
                    tool.function.parameters,
                ),
                "strict": false,
            })
        })
        .collect()
}

fn responses_tool_input_schema_for_provider(provider: &ProviderConfig, schema: Value) -> Value {
    let schema = openai_tool_input_schema(schema);
    if provider_uses_qwen_tool_schema(provider) {
        qwen_tool_input_schema(schema)
    } else {
        schema
    }
}

/// Qwen-Agent's validator (used behind Bailian/DashScope endpoints) accepts
/// exactly these three keys at the parameters root. Nested property schemas stay
/// intact, so enums, arrays, descriptions, and object shapes retain their meaning.
fn qwen_tool_input_schema(schema: Value) -> Value {
    let object = schema.as_object();
    let properties = object
        .and_then(|object| object.get("properties"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required = object
        .and_then(|object| object.get("required"))
        .and_then(Value::as_array)
        .map(|required| {
            required
                .iter()
                .filter_map(Value::as_str)
                .filter(|name| properties.contains_key(*name))
                .map(|name| Value::String(name.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

fn provider_uses_qwen_tool_schema(provider: &ProviderConfig) -> bool {
    let base_url = provider.base_url.to_ascii_lowercase();
    let id = provider.id.to_ascii_lowercase();
    let display_name = provider.display_name.to_ascii_lowercase();

    (((base_url.contains("dashscope") || base_url.contains("bailian"))
        && base_url.contains("aliyuncs.com"))
        || base_url.contains(".maas.aliyuncs.com"))
        || id.contains("bailian")
        || display_name.contains("bailian")
        || provider.display_name.contains("百炼")
}
