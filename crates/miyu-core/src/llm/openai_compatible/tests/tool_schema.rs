use super::shared::*;
use crate::llm::openai_compatible::*;
use crate::llm::{FunctionDefinition, ToolDefinition};
use serde_json::{json, Value};

fn tool(parameters: Value) -> ToolDefinition {
    ToolDefinition {
        kind: "function",
        function: FunctionDefinition {
            name: "alarm".to_string(),
            description: "Create or inspect alarms".to_string(),
            parameters,
        },
    }
}

fn assert_qwen_parameters_schema(schema: &Value) {
    let object = schema.as_object().expect("parameters must be an object");
    assert_eq!(object.len(), 3);
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"].is_object());
    assert!(schema["required"].is_array());
}

#[test]
fn bailian_responses_tools_use_qwen_parameters_dialect() {
    let provider = test_provider(
        "custom-provider",
        "https://workspace.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
    );
    let tools = lower_responses_tools_for_provider(
        &provider,
        vec![tool(json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "action": {"type": "string"},
                "options": {
                    "type": "object",
                    "properties": {"repeat": {"type": "boolean"}},
                    "additionalProperties": false
                }
            },
            "required": ["action"],
            "additionalProperties": false
        }))],
    );

    let parameters = &tools[0]["parameters"];
    assert_qwen_parameters_schema(parameters);
    assert_eq!(parameters["required"], json!(["action"]));
    assert!(parameters.get("additionalProperties").is_none());
    assert!(parameters.get("$schema").is_none());
    assert_eq!(
        parameters["properties"]["options"]["additionalProperties"],
        false
    );
}

#[test]
fn bailian_chat_tools_add_empty_required_for_parameterless_tools() {
    let provider = test_provider(
        "custom-provider",
        "https://coding.dashscope.aliyuncs.com/v1",
    );
    let tools = lower_chat_tools_for_provider(
        &provider,
        vec![tool(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }))],
    );

    let parameters = &tools[0].function.parameters;
    assert_qwen_parameters_schema(parameters);
    assert_eq!(parameters["required"], json!([]));
}

#[test]
fn non_bailian_provider_keeps_standard_openai_schema() {
    let provider = test_provider("openai-compatible", "https://example.com/v1");
    let tools = lower_chat_tools_for_provider(
        &provider,
        vec![tool(json!({
            "type": "object",
            "properties": {"action": {"type": "string"}},
            "required": ["action"],
            "additionalProperties": false,
            "x-provider-extension": {"preserved": true}
        }))],
    );

    let parameters = &tools[0].function.parameters;
    assert_eq!(parameters["additionalProperties"], false);
    assert_eq!(parameters["required"], json!(["action"]));
    assert_eq!(parameters["x-provider-extension"]["preserved"], true);
}

#[test]
fn non_bailian_chat_tools_are_not_openai_normalized() {
    let provider = test_provider("openai-compatible", "https://example.com/v1");
    let original = json!({
        "type": "object",
        "properties": {
            "optional": {"anyOf": [{"type": "string"}, {"type": "null"}]}
        }
    });
    let tools = lower_chat_tools_for_provider(&provider, vec![tool(original.clone())]);

    assert_eq!(tools[0].function.parameters, original);
}

#[test]
fn bailian_required_entries_are_limited_to_declared_properties() {
    let provider = test_provider("bailian-proxy", "https://example.com/v1");
    let tools = lower_chat_tools_for_provider(
        &provider,
        vec![tool(json!({
            "type": "object",
            "properties": {"action": {"type": "string"}},
            "required": ["action", "stale_field"]
        }))],
    );

    assert_eq!(tools[0].function.parameters["required"], json!(["action"]));
}

// `all_builtin_tools_fit_the_qwen_parameters_contract` 搬到 miyu-engine 去了：
// 工具面住在那一层（`tools::test_support::builtin_registry`），而 core 反向依赖
// engine 会成环。那条测试现在住 `crates/miyu-engine/src/tools/tests.rs`，整形
// 函数经 testkit 从 `miyu_core::llm::{qwen_tool_provider,
// lower_chat_tools_for_provider}` 取。
