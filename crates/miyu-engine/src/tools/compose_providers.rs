use super::{mcp, scripts, skills, AppConfig, MiyuPaths, PersonaManifest, ToolRegistry};

pub fn register(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    paths: &MiyuPaths,
    manifest: &PersonaManifest,
    subsystems: &miyu_base::config::EnabledSubsystems,
    external: bool,
) {
    let plugin = |id: &str| manifest.plugin_enabled(id);
    if plugin("scripts") {
        if external {
            scripts::register_external(registry, config, paths);
        } else {
            scripts::register(registry, config, paths);
        }
    }
    if plugin("mcp") && config.mcp.enabled {
        mcp::register(registry, config.clone(), manifest.plugins.mcp.as_deref());
    }
    if subsystems.skills {
        if let Err(error) = skills::register_skills(registry, config, paths) {
            tracing::warn!(error = %error, "failed to register skills");
        }
        skills::register_authoring(registry, config.clone(), paths.clone());
    }
}
