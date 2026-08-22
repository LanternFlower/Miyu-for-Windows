use super::{ToolRegistry, ToolSpec};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const BASE_DESCRIPTION: &str = "Load the full description and parameter schema of tools, scripts, or tool groups on demand. Pick <name> values from <available_load_targets> and load them with {\"names\":[\"name\"]}. type=tool/script loads a single tool; type=group loads every not-yet-loaded tool in that group. <script_summary> summarizes the state of all scripts: always_loaded scripts are directly callable and need no loading; lazy scripts must be picked from available_load_targets and loaded; unregistered_scripts only counts user script files that lack a description and are not registered yet — it says nothing about how many scripts are registered. Files in <unregistered_scripts> cannot be loaded or called directly; read the listed path first and register them with register_script.";

pub fn register(registry: &mut ToolRegistry) {
    registry.register(
        ToolSpec::new(
            "load_tools",
            BASE_DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "names": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Names of the tools, scripts, or tool groups to load. Only names from available_load_targets are allowed, e.g. web_search or group:gaming."
                    }
                },
                "required": ["names"],
                "additionalProperties": false
            }),
            |_args| async {
                bail!("load_tools must be executed through the active tool registry")
            },
        )
        .with_display_name("加载"),
    );
}

pub(super) fn dynamic_description(registry: &ToolRegistry, loaded: &BTreeSet<String>) -> String {
    let mut description = format!(
        "{BASE_DESCRIPTION}\n\n{}\n{}",
        registry.script_summary_xml(),
        registry.load_targets_xml(loaded),
    );
    if let Some(xml) = unregistered_scripts_xml(registry) {
        description.push('\n');
        description.push_str(&xml);
    }
    description
}

/// Stub loading mode: the catalog is already visible as permanently
/// registered stubs, so the loader description only explains the contract
/// fetch flow (plus unregistered script files, which are not in the tools
/// array at all).
pub(super) fn stub_mode_description(registry: &ToolRegistry) -> String {
    // 没有未注册脚本时一个字都不追加。此前恒定输出一份空的
    // <unregistered_scripts></unregistered_scripts>:既白占 53 字符,又是
    // 一个静默的缓存杀手——往 scripts 目录扔一个未注册文件,tools 数组的
    // 字节就变,所有会话的前缀缓存从 token 0 掰断(08-17 实测)。
    let base = "Fetch the full parameter contract of tools or scripts. Every tool of this session is already pinned in the tools list as a stub entry (stub-marked tools carry only a one-line summary and a permissive parameter shell). Request with {\"names\":[\"tool_name\"]}; the result's contracts field returns each tool's full description and parameter JSON Schema. Then call the tool with the actual arguments placed directly in its top-level parameter object as the contract specifies. group:name fetches a whole group's contracts at once.";
    match unregistered_scripts_xml(registry) {
        Some(xml) => format!("{base} Files in <unregistered_scripts> are not registered yet; read the listed path first and register them with register_script.\n\n{xml}"),
        None => base.to_string(),
    }
}

pub(super) fn execute(args: Value, registry: &ToolRegistry) -> Result<String> {
    let names = args
        .get("names")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("names array is required"))?;
    if names.is_empty() {
        bail!("names must not be empty");
    }

    let requested = names
        .iter()
        .filter_map(|value| value.as_str().map(str::trim).map(str::to_string))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let (loaded_targets, loaded_tools, skipped) =
        registry.expand_load_targets(&requested, &BTreeSet::new());

    // 未知名一票否决:哪怕混着可用工具也整调报错并逐名点出——否则
    // "7 个不存在+1 个本就常驻"会渲染成一次成功加载,模型(和用户)都
    // 会以为那批工具真的存在(验收:dev 记忆里的旧工具清单诱发误加载)。
    let unknown = skipped
        .iter()
        .filter(|note| note.ends_with(": unknown tool or script"))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        bail!(
            "these names do not exist in this session: {}. Pick names from <available_load_targets>; other requested names were not loaded.",
            unknown.join("; ")
        );
    }
    if loaded_tools.is_empty() {
        let already_available = skipped
            .iter()
            .any(|note| note.contains("already available") || note.contains("already loaded"));
        if !already_available {
            bail!(
                "no loadable tool, script, or group in names. {}",
                skipped.join("; ")
            );
        }
    }

    // Full contracts ride the tool result (conversation tail) instead of the
    // cached tools prefix. Also answer for directly requested names that were
    // "already available": in stub mode the model may legitimately ask for an
    // always-loaded tool's schema.
    let mut contract_names = loaded_tools.clone();
    for name in &requested {
        if !name.starts_with("group:") {
            contract_names.push(name.clone());
        }
    }
    let contracts = registry.tool_contracts(&contract_names);

    let note = if loaded_tools.is_empty() {
        "nothing to load; every requested tool is already available"
    } else {
        "loaded"
    };
    Ok(serde_json::to_string_pretty(&json!({
        "loaded_targets": loaded_targets,
        "loaded_tools": loaded_tools,
        "skipped": skipped,
        "contracts": contracts,
        "note": note,
    }))?)
}

/// `None` = 没有未注册脚本(常态)。调用方据此整段跳过,不发空壳。
fn unregistered_scripts_xml(registry: &ToolRegistry) -> Option<String> {
    let scripts = registry.unregistered_scripts();
    if scripts.is_empty() {
        return None;
    }
    let items = scripts
        .iter()
        .map(|script| {
            format!(
                "  <script>\n    <name>{}</name>\n    <path>{}</path>\n  </script>",
                xml_escape(&script.name),
                xml_escape(&script.path),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "<unregistered_scripts>\n{items}\n</unregistered_scripts>"
    ))
}

pub(super) fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::super::tool_descriptions::LoadPolicy;
    use super::*;

    #[test]
    fn description_separates_builtin_scripts_and_unregistered_files() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);
        registry.register(
            ToolSpec::new(
                "custom_builtin",
                "Built in",
                json!({"type":"object","properties":{}}),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(false),
        );
        registry
            .replace_script_tools(
                vec![ToolSpec::new(
                    "lazy_script",
                    "Lazy script",
                    json!({"type":"object","properties":{"query":{"type":"string"}}}),
                    |_| async { Ok(String::new()) },
                )
                .script()
                .with_always_loaded(false)],
                vec![super::super::registry::UnregisteredScript {
                    name: "unknown_script".to_string(),
                    path: "unknown-script".to_string(),
                }],
            )
            .unwrap();

        let description = dynamic_description(&registry, &BTreeSet::new());
        assert!(description.contains("<total>1</total>"));
        assert!(description.contains("<lazy>1</lazy>"));
        assert!(description.contains("<unregistered>1</unregistered>"));
        assert!(description.contains("<registered_names>lazy_script</registered_names>"));
        assert!(description.contains("<available_load_targets>"));
        assert!(description.contains("custom_builtin"));
        assert!(description.contains("lazy_script"));
        assert!(description.contains("<unregistered_scripts>"));
        assert!(description.contains("unknown-script"));
    }

    #[tokio::test]
    async fn registry_loads_dynamic_script_definition() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);
        registry
            .replace_script_tools(
                vec![ToolSpec::new(
                    "lazy_script",
                    "Lazy script",
                    json!({"type":"object","properties":{}}),
                    |_| async { Ok(String::new()) },
                )
                .script()
                .with_always_loaded(false)],
                Vec::new(),
            )
            .unwrap();

        let output = registry
            .call("load_tools", r#"{"names":["lazy_script"]}"#)
            .await
            .unwrap();
        assert!(output.contains("lazy_script"));
        assert!(output.contains("\"loaded_tools\""));
    }

    #[tokio::test]
    async fn always_loaded_names_do_not_fail_the_whole_request() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);
        registry.register(
            ToolSpec::new(
                "web_search",
                "Always-on web search",
                json!({"type":"object","properties":{}}),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(true),
        );
        registry.register(
            ToolSpec::new(
                "protondb_query",
                "Query ProtonDB compatibility",
                json!({"type":"object","properties":{}}),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(false)
            .with_load_policy(LoadPolicy::Group)
            .with_groups(vec!["gaming".to_string()]),
        );

        // Mixing an always-loaded tool with a valid group must still load
        // the group and merely note the redundant name.
        let output = registry
            .call("load_tools", r#"{"names":["group:gaming","web_search"]}"#)
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["loaded_tools"][0], "protondb_query");
        assert!(value["skipped"][0]
            .as_str()
            .unwrap()
            .contains("already available"));

        // Only-already-available requests succeed with an explanatory note.
        let output = registry
            .call("load_tools", r#"{"names":["web_search"]}"#)
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            value["note"],
            "nothing to load; every requested tool is already available"
        );

        // Mixed known+unknown must ALSO error, naming the unknowns: an
        // "already available" note beside 7 ghosts must not render as success.
        let mixed = registry
            .call(
                "load_tools",
                r#"{"names":["web_search","read_file","edit_string"]}"#,
            )
            .await;
        let message = format!("{:#}", mixed.unwrap_err());
        assert!(message.contains("read_file"), "{message}");
        assert!(message.contains("edit_string"), "{message}");

        // Entirely unknown names still error so the model can correct itself.
        assert!(registry
            .call("load_tools", r#"{"names":["no_such_tool"]}"#)
            .await
            .is_err());
    }

    #[test]
    fn stub_definitions_are_byte_stable_and_shaped() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);
        registry.register(
            ToolSpec::new(
                "always_tool",
                "Always-on tool",
                json!({"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(true),
        );
        registry.register(
            ToolSpec::new(
                "lazy_tool",
                "多行描述的第一行摘要。\n第二行细节不进 stub。",
                json!({"type":"object","properties":{"x":{"type":"integer"}},"required":["x"]}),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(false),
        );

        let first = serde_json::to_string(&registry.stub_definitions()).unwrap();
        let second = serde_json::to_string(&registry.stub_definitions()).unwrap();
        // The provider-visible tools array must be byte-identical across
        // renders: this is the whole point of stub mode.
        assert_eq!(first, second);

        let definitions = registry.stub_definitions();
        let lazy = definitions
            .iter()
            .find(|def| def.function.name == "lazy_tool")
            .unwrap();
        assert!(lazy.function.description.contains("多行描述的第一行摘要"));
        assert!(lazy.function.description.contains("stub entry"));
        assert!(!lazy.function.description.contains("第二行细节"));
        // 裸 {"type":"object"}:JSON Schema 里等价于显式写 properties:{} +
        // additionalProperties:true,少 48 字符/条。
        assert_eq!(lazy.function.parameters, serde_json::json!({"type":"object"}));
        let always = definitions
            .iter()
            .find(|def| def.function.name == "always_tool")
            .unwrap();
        assert_eq!(
            always.function.parameters["properties"]["q"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn load_result_carries_full_contracts() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);
        registry.register(
            ToolSpec::new(
                "lazy_tool",
                "Lazy tool",
                json!({"type":"object","properties":{"x":{"type":"integer"}},"required":["x"]}),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(false),
        );
        registry.register(
            ToolSpec::new(
                "always_tool",
                "Always-on tool",
                json!({"type":"object","properties":{"q":{"type":"string"}}}),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(true),
        );

        let output = registry
            .call("load_tools", r#"{"names":["lazy_tool"]}"#)
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        let contracts = value["contracts"].as_array().unwrap();
        assert_eq!(contracts[0]["name"], "lazy_tool");
        assert_eq!(
            contracts[0]["parameters"]["properties"]["x"]["type"],
            "integer"
        );

        // Asking for an already-available tool still returns its contract:
        // in stub mode that is a legitimate schema fetch.
        let output = registry
            .call("load_tools", r#"{"names":["always_tool"]}"#)
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        let contracts = value["contracts"].as_array().unwrap();
        assert_eq!(contracts[0]["name"], "always_tool");
    }

    #[tokio::test]
    async fn registry_loads_group_target_definition() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);
        registry.register(
            ToolSpec::new(
                "protondb_query",
                "Query ProtonDB compatibility",
                json!({"type":"object","properties":{"game":{"type":"string"}}}),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(false)
            .with_load_policy(LoadPolicy::Group)
            .with_groups(vec!["gaming".to_string()]),
        );

        let description = dynamic_description(&registry, &BTreeSet::new());
        assert!(description.contains("group:gaming"));
        assert!(description.contains("protondb_query"));

        let output = registry
            .call("load_tools", r#"{"names":["group:gaming"]}"#)
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            value["loaded_targets"].as_array().unwrap()[0]
                .as_str()
                .unwrap(),
            "group:gaming"
        );
        assert_eq!(
            value["loaded_tools"].as_array().unwrap()[0]
                .as_str()
                .unwrap(),
            "protondb_query"
        );
    }
}
