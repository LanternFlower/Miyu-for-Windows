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

#[test]
fn all_builtin_tools_fit_the_qwen_parameters_contract() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let registry = crate::tools::builtin_registry(&crate::config::AppConfig::default(), &paths);
    let definitions = registry.definitions();
    assert!(definitions.iter().any(|tool| tool.function.name == "alarm"));

    let provider = test_provider("Bailian", "https://example.com/v1");
    for tool in lower_chat_tools_for_provider(&provider, definitions) {
        assert_qwen_parameters_schema(&tool.function.parameters);
        let properties = tool.function.parameters["properties"].as_object().unwrap();
        for required in tool.function.parameters["required"].as_array().unwrap() {
            assert!(properties.contains_key(required.as_str().unwrap()));
        }
    }
}
