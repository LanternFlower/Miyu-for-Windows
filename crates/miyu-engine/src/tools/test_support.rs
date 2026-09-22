//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

/// 属主面、当前人格的全量工具目录(旧 `builtin_registry`)。
pub fn builtin_registry(config: &AppConfig, paths: &MiyuPaths) -> ToolRegistry {
    let manifest = PersonaManifest::load(config, paths, &config.active_persona_scope());
    compose_registry(config, paths, &manifest, Surface::owner(false))
}

/// dev persona 的工具目录:core 之上一件不挂(旧 `dev_registry`)。
pub fn dev_registry(config: &AppConfig, paths: &MiyuPaths) -> ToolRegistry {
    let manifest = PersonaManifest::load(config, paths, miyu_core::state::DEV_PERSONA);
    compose_registry(config, paths, &manifest, Surface::owner(false))
}
