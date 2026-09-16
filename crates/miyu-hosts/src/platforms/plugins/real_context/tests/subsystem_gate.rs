//! `PersonaManifest.subsystems.emotion` 的三态:开着 / 人格关掉 / 没有清单文件。
//!
//! 09-16 之前这个开关只在 persona.toml 里存在,运行时没有任何读者(计划 §9.1
//! 的「悬空配置」)。这里先把三态钉住,再把开关接进插件:persona 的意愿位 ×
//! 机器配置(`affection_enable` / `emotion_enable`)共同决定好感度与情绪是否构造。

use super::shared::{availability_context, inbound_event};
use crate::platforms::plugins::real_context::*;
use crate::platforms::plugins::PlatformPlugin;
use miyu_base::config::{PersonaManifest, PlatformPluginInstanceConfig, REAL_CONTEXT_PLUGIN_ID};

/// 机器级开关全开的配置。
fn machine_on() -> miyu_base::config::AppConfig {
    let mut config = miyu_base::config::AppConfig::default();
    let mut settings = serde_json::Map::new();
    settings.insert("affection_enable".to_string(), Value::Bool(true));
    settings.insert("emotion_enable".to_string(), Value::Bool(true));
    config.platforms.qq.plugins.insert(
        REAL_CONTEXT_PLUGIN_ID.to_string(),
        PlatformPluginInstanceConfig {
            enabled: None,
            settings,
        },
    );
    config
}

fn context_with(
    config: miyu_base::config::AppConfig,
) -> (tempfile::TempDir, Arc<PlatformTurnContext>) {
    let (temp, base) = availability_context(BotSendAvailability::Available);
    let rebuilt = PlatformTurnContext::new(
        base.conversation.clone(),
        base.sender_id.clone(),
        base.sender_display_name.clone(),
        false,
        config,
        base.paths.clone(),
        base.state_store.clone(),
        base.adapter.clone(),
        base.plugins.clone(),
    )
    .with_inbound_event(inbound_event());
    (temp, Arc::new(rebuilt))
}

fn write_manifest(context: &PlatformTurnContext, body: &str) {
    let path = PersonaManifest::manifest_path(
        &context.config,
        &context.paths,
        &context.config.active_persona_scope(),
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn relationship_tool_registered(
    plugin: &RealContextPlugin,
    context: &Arc<PlatformTurnContext>,
) -> bool {
    let mut registry = ToolRegistry::new();
    plugin
        .register_tools(&mut registry, context.clone())
        .unwrap();
    registry.contains("query_qq_relationship")
}

/// 开着(默认人格、没有清单文件 = 内置全开):机器开关说了算。
#[test]
fn emotion_runs_when_the_persona_leaves_it_on() {
    let (_temp, context) = context_with(machine_on());
    let plugin = RealContextPlugin::new();
    let settings = plugin.settings(&context).unwrap();
    assert!(settings.affection_enable && settings.emotion_enable);
    assert!(relationship_tool_registered(&plugin, &context));
    assert!(emotion::snapshot(&context, &settings).unwrap().is_some());
}

/// 人格关掉:好感度工具不注册、情绪快照为空、好感度快照为空——机器开关开着也没用。
#[test]
fn emotion_is_not_built_when_the_persona_turns_it_off() {
    let (_temp, context) = context_with(machine_on());
    write_manifest(&context, "[subsystems]\nemotion = false\n");
    let plugin = RealContextPlugin::new();
    let settings = plugin.settings(&context).unwrap();
    assert!(
        !settings.affection_enable && !settings.emotion_enable,
        "persona.toml 里 emotion = false 必须压掉机器开关"
    );
    assert!(!relationship_tool_registered(&plugin, &context));
    assert!(emotion::snapshot(&context, &settings).unwrap().is_none());
    assert!(affection::snapshot(&context, &settings, true)
        .unwrap()
        .is_none());
}

/// 没有清单文件:按内置默认——默认人格全开,dev 人格(core_only)关。
#[test]
fn missing_manifest_falls_back_to_the_builtin_default() {
    let mut config = machine_on();
    config.prompt.active_persona = miyu_core::state::DEV_PERSONA.to_string();
    let (_temp, context) = context_with(config);
    let plugin = RealContextPlugin::new();
    let settings = plugin.settings(&context).unwrap();
    assert!(
        !settings.affection_enable && !settings.emotion_enable,
        "dev 人格是 core_only,情绪与好感度不构造"
    );
    assert!(!relationship_tool_registered(&plugin, &context));
}

/// 人格开着但机器关着:人格不能把机器没开的东西打开。
#[test]
fn persona_cannot_enable_what_the_machine_disabled() {
    let (_temp, context) = context_with(miyu_base::config::AppConfig::default());
    write_manifest(&context, "[subsystems]\nemotion = true\n");
    let plugin = RealContextPlugin::new();
    let settings = plugin.settings(&context).unwrap();
    assert!(!settings.affection_enable && !settings.emotion_enable);
    assert!(!relationship_tool_registered(&plugin, &context));
}
