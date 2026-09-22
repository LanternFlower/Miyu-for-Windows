//! Provider and model discovery shared by the WebUI, TUI, and OOBE.
//!
//! This module owns domain discovery only. Presentation layers decide how to
//! authenticate the caller and render errors; they must not call each other.

mod cli;
mod http;

use crate::config::ProviderConfig;
use anyhow::Result;

pub use cli::builtin_cli_binary;

/// Return the models visible from a provider's configured discovery source.
///
/// Network discovery and CLI discovery intentionally share one entry point so
/// every surface gets the same ordering, deduplication, and failure semantics.
pub fn fetch_models(provider: &ProviderConfig, cli_binary: Option<&str>) -> Result<Vec<String>> {
    if provider.is_builtin_cli_provider() {
        cli::builtin_cli_catalog(provider, cli_binary)
    } else {
        http::fetch_http_models(provider)
    }
}

/// Fill only missing model metadata from the local catalog.
pub fn auto_configure_model_tags(
    paths: &crate::paths::MiyuPaths,
    provider: &mut ProviderConfig,
    model: &str,
) {
    let needs_modalities = !provider.model_modalities.contains_key(model);
    let needs_window = !provider.model_context_window.contains_key(model);
    if !needs_modalities && !needs_window {
        return;
    }
    let Some(entry) = crate::models_cache::describe_models(
        paths,
        &provider.id,
        &provider.base_url,
        &[model.to_string()],
    )
    .pop() else {
        return;
    };
    if needs_modalities {
        if let Some(modalities) = entry.modalities.filter(|value| !value.is_empty()) {
            provider
                .model_modalities
                .insert(model.to_string(), modalities);
        }
    }
    if needs_window {
        if let Some(window) = entry.context_window.filter(|value| *value > 0) {
            provider
                .model_context_window
                .insert(model.to_string(), window as usize);
        }
    }
}

pub fn catalog_entry(
    paths: &crate::paths::MiyuPaths,
    provider: &ProviderConfig,
    model: &str,
) -> Option<crate::models_cache::ModelCatalogEntry> {
    crate::models_cache::describe_models(
        paths,
        &provider.id,
        &provider.base_url,
        &[model.to_string()],
    )
    .pop()
}
