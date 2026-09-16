use crate::config::ProviderConfig;
use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelInfo>,
}

#[derive(Deserialize)]
struct ModelInfo {
    id: String,
}

pub(super) fn fetch_http_models(provider: &ProviderConfig) -> Result<Vec<String>> {
    let api_key = provider.api_key.as_deref().unwrap_or_default();
    let api_key = api_key
        .strip_prefix("$env:")
        .map(|name| std::env::var(name).unwrap_or_default())
        .unwrap_or_else(|| api_key.to_string());
    let api_key = if api_key.is_empty() && provider.is_opencode_zen() {
        "public".to_string()
    } else {
        api_key
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(provider.timeout_seconds))
        .build()?;
    let mut request = client
        .get(crate::provider_url::models_url(&provider.base_url))
        .header("Accept", "application/json")
        .header("User-Agent", "miyu-config");
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    let response = request.send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        anyhow::bail!("{status}: {body}");
    }
    let parsed: ModelsResponse = serde_json::from_str(&body)?;
    Ok(parsed
        .data
        .into_iter()
        .map(|model| model.id)
        .filter(|id| !id.is_empty())
        .collect())
}
