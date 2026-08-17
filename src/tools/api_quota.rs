use super::{ToolRegistry, ToolSpec};
use crate::config::{
    ApiQuotaAccountConfig, ApiQuotaPluginConfig, ApiQuotaProviderConfig, ProviderConfig,
};
use crate::i18n::{is_zh, text as t};
use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::Semaphore;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const DEEPSEEK_BALANCE_URL: &str = "https://api.deepseek.com/user/balance";
const OPENROUTER_KEY_URL: &str = "https://openrouter.ai/api/v1/key";
static HTTP_CONCURRENCY: Semaphore = Semaphore::const_new(4);

#[derive(Clone, Copy)]
enum QuotaProvider {
    DeepSeek,
    OpenRouter,
}

impl QuotaProvider {
    fn id(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
        }
    }

    fn default_endpoint(self) -> &'static str {
        match self {
            Self::DeepSeek => DEEPSEEK_BALANCE_URL,
            Self::OpenRouter => OPENROUTER_KEY_URL,
        }
    }
}

#[derive(Clone)]
struct QuotaAccount {
    name: String,
    api_key: String,
    endpoint: String,
}

pub fn register(
    registry: &mut ToolRegistry,
    config: ApiQuotaPluginConfig,
    providers: Vec<ProviderConfig>,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("API quota HTTP client configuration must be valid");
    registry.register(ToolSpec::new(
        "query_api_quota",
        "Query and summarize the configured DeepSeek or OpenRouter API quota.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "enum": ["all", "deepseek", "openrouter"],
                    "description": "Provider to query. Defaults to all configured providers."
                },
                "account": {
                    "type": "string",
                    "description": "Optional account name. Omit to query every configured account."
                }
            },
            "additionalProperties": false
        }),
        move |args| {
            let client = client.clone();
            let config = config.clone();
            let providers = providers.clone();
            async move { query_api_quota(&client, &config, &providers, args).await }
        },
    ));
}

async fn query_api_quota(
    client: &reqwest::Client,
    config: &ApiQuotaPluginConfig,
    providers: &[ProviderConfig],
    args: Value,
) -> Result<String> {
    let provider = args
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("all");
    let account = args.get("account").and_then(Value::as_str);
    match provider {
        "deepseek" => query_deepseek(client, &config.deepseek, providers, account).await,
        "openrouter" => query_openrouter(client, &config.openrouter, providers, account).await,
        "all" => query_all(client, config, providers, account).await,
        other => bail!("unsupported API quota provider: {other}"),
    }
}

async fn query_all(
    client: &reqwest::Client,
    config: &ApiQuotaPluginConfig,
    providers: &[ProviderConfig],
    account: Option<&str>,
) -> Result<String> {
    let deepseek_configured =
        has_configured_account(&config.deepseek, providers, QuotaProvider::DeepSeek);
    let openrouter_configured =
        has_configured_account(&config.openrouter, providers, QuotaProvider::OpenRouter);
    if !deepseek_configured && !openrouter_configured {
        bail!("no DeepSeek or OpenRouter API key is configured")
    }

    match (deepseek_configured, openrouter_configured) {
        (true, true) => {
            let (deepseek, openrouter) = tokio::join!(
                query_deepseek(client, &config.deepseek, providers, account),
                query_openrouter(client, &config.openrouter, providers, account)
            );
            match (deepseek, openrouter) {
                (Err(deepseek), Err(openrouter)) => bail!(
                    "both API quota queries failed; DeepSeek: {deepseek}; OpenRouter: {openrouter}"
                ),
                (deepseek, openrouter) => Ok(format_query_results([
                    ("DeepSeek", deepseek),
                    ("OpenRouter", openrouter),
                ])),
            }
        }
        (true, false) => query_deepseek(client, &config.deepseek, providers, account).await,
        (false, true) => query_openrouter(client, &config.openrouter, providers, account).await,
        (false, false) => unreachable!(),
    }
}

fn format_query_results<const N: usize>(results: [(&str, Result<String>); N]) -> String {
    results
        .into_iter()
        .map(|(provider, result)| match result {
            Ok(value) => value,
            Err(error) => format!(
                "## {provider}\n- {}: {error}",
                t("Query failed", "查询失败")
            ),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

async fn query_deepseek(
    client: &reqwest::Client,
    config: &ApiQuotaProviderConfig,
    providers: &[ProviderConfig],
    account: Option<&str>,
) -> Result<String> {
    let accounts = selected_accounts(config, providers, QuotaProvider::DeepSeek, account)?;
    let results = futures_util::stream::iter(
        accounts
            .iter()
            .cloned()
            .map(|account| async move { query_deepseek_account(client, &account).await }),
    )
    .buffered(4)
    .collect::<Vec<_>>()
    .await;
    if results.iter().all(Result::is_err) {
        let errors = accounts
            .iter()
            .zip(results)
            .map(|(account, result)| format!("{}: {}", account.name, result.unwrap_err()))
            .collect::<Vec<_>>()
            .join("; ");
        bail!("all DeepSeek account queries failed: {errors}")
    }
    let mut lines = vec![
        "## DeepSeek".to_string(),
        format!(
            "- {}",
            t(
                "DeepSeek API balance is split into independent CNY and USD pools; totals are shown separately.",
                "DeepSeek API 余额按 CNY 与 USD 分为两个独立余额池，以下分别显示各币种总余额。"
            )
        ),
    ];
    for (account, result) in accounts.iter().zip(results) {
        lines.push(format!("### {}", account.name));
        lines.push(
            result.unwrap_or_else(|error| format!("- {}: {error}", t("Query failed", "查询失败"))),
        );
    }
    Ok(lines.join("\n"))
}

async fn query_deepseek_account(
    client: &reqwest::Client,
    account: &QuotaAccount,
) -> Result<String> {
    let response: DeepSeekBalance =
        get_json(client, &account.endpoint, &account.api_key, "DeepSeek").await?;
    format_deepseek_balance(response)
}

fn format_deepseek_balance(response: DeepSeekBalance) -> Result<String> {
    if response.balance_infos.is_empty() {
        bail!("DeepSeek returned no balance information")
    }
    let lines = response
        .balance_infos
        .into_iter()
        .map(|balance| {
            if is_zh() {
                format!("- {} 总余额：{}", balance.currency, balance.total_balance)
            } else {
                format!(
                    "- {} total balance: {}",
                    balance.currency, balance.total_balance
                )
            }
        })
        .collect::<Vec<_>>();
    Ok(lines.join("\n"))
}

async fn query_openrouter(
    client: &reqwest::Client,
    config: &ApiQuotaProviderConfig,
    providers: &[ProviderConfig],
    account: Option<&str>,
) -> Result<String> {
    let accounts = selected_accounts(config, providers, QuotaProvider::OpenRouter, account)?;
    let results = futures_util::stream::iter(
        accounts
            .iter()
            .cloned()
            .map(|account| async move { query_openrouter_account(client, &account).await }),
    )
    .buffered(4)
    .collect::<Vec<_>>()
    .await;
    if results.iter().all(Result::is_err) {
        let errors = accounts
            .iter()
            .zip(results)
            .map(|(account, result)| format!("{}: {}", account.name, result.unwrap_err()))
            .collect::<Vec<_>>()
            .join("; ");
        bail!("all OpenRouter account queries failed: {errors}")
    }
    let mut lines = vec!["## OpenRouter".to_string()];
    for (account, result) in accounts.iter().zip(results) {
        lines.push(format!("### {}", account.name));
        lines.push(
            result.unwrap_or_else(|error| format!("- {}: {error}", t("Query failed", "查询失败"))),
        );
    }
    Ok(lines.join("\n"))
}

async fn query_openrouter_account(
    client: &reqwest::Client,
    account: &QuotaAccount,
) -> Result<String> {
    let response: OpenRouterKeyResponse =
        get_json(client, &account.endpoint, &account.api_key, "OpenRouter").await?;
    let data = response.data;
    let mut lines = Vec::new();
    if let Some(label) = data.label.filter(|label| !label.trim().is_empty()) {
        lines.push(format!("- API Key: {label}"));
    }
    lines.push(format!(
        "- {}: {}",
        t("Credit limit", "额度上限"),
        data.limit
            .map(format_usd)
            .unwrap_or_else(|| t("unlimited", "无限制").to_string())
    ));
    lines.push(format!(
        "- {}: {}",
        t("Remaining credit", "剩余额度"),
        data.limit_remaining
            .map(format_usd)
            .unwrap_or_else(|| t("unlimited", "无限制").to_string())
    ));
    lines.push(format!(
        "- {}: {}",
        t("Total usage", "累计使用"),
        format_usd(data.usage)
    ));
    lines.push(format!(
        "- {}: {} / {} / {}",
        t("Daily / weekly / monthly usage", "本日 / 本周 / 本月使用"),
        format_usd(data.usage_daily),
        format_usd(data.usage_weekly),
        format_usd(data.usage_monthly)
    ));
    Ok(lines.join("\n"))
}

fn has_configured_account(
    config: &ApiQuotaProviderConfig,
    providers: &[ProviderConfig],
    provider: QuotaProvider,
) -> bool {
    effective_accounts(config, providers, provider)
        .iter()
        .any(|account| !account.api_key.trim().is_empty())
}

fn effective_accounts(
    config: &ApiQuotaProviderConfig,
    providers: &[ProviderConfig],
    provider: QuotaProvider,
) -> Vec<QuotaAccount> {
    let endpoint = configured_quota_endpoint(providers, provider);
    let configured_accounts = if config.accounts.is_empty() {
        vec![ApiQuotaAccountConfig {
            id: "account-1".to_string(),
            name: "默认账号".to_string(),
            api_key: config.api_key.clone(),
        }]
    } else {
        config.accounts.clone()
    };
    let mut accounts = configured_accounts
        .into_iter()
        .enumerate()
        .filter(|(_, account)| !account.api_key.trim().is_empty())
        .map(|(index, account)| QuotaAccount {
            name: if account.name.trim().is_empty() {
                format!("账号 {}", index + 1)
            } else {
                account.name.trim().to_string()
            },
            api_key: account.api_key,
            endpoint: endpoint.clone(),
        })
        .collect::<Vec<_>>();

    for configured_provider in matching_providers(providers, provider) {
        let Some(raw_keys) = configured_provider.api_key.as_deref() else {
            continue;
        };
        for raw_key in split_provider_api_keys(raw_keys) {
            if accounts
                .iter()
                .any(|account| account.api_key.trim() == raw_key)
            {
                continue;
            }
            let base_name = if configured_provider.display_name.trim().is_empty() {
                configured_provider.id.trim()
            } else {
                configured_provider.display_name.trim()
            };
            let name = unique_account_name(
                &accounts,
                if base_name.is_empty() {
                    provider.id()
                } else {
                    base_name
                },
            );
            accounts.push(QuotaAccount {
                name,
                api_key: raw_key.to_string(),
                endpoint: quota_endpoint(provider, &configured_provider.base_url),
            });
        }
    }
    accounts
}

fn matching_providers(
    providers: &[ProviderConfig],
    provider: QuotaProvider,
) -> impl Iterator<Item = &ProviderConfig> {
    providers.iter().filter(move |configured| {
        configured.id.trim().eq_ignore_ascii_case(provider.id())
            || provider_base_url_matches(provider, &configured.base_url)
    })
}

fn provider_base_url_matches(provider: QuotaProvider, base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url.trim()) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    match provider {
        QuotaProvider::DeepSeek => host.eq_ignore_ascii_case("api.deepseek.com"),
        QuotaProvider::OpenRouter => host.eq_ignore_ascii_case("openrouter.ai"),
    }
}

fn configured_quota_endpoint(providers: &[ProviderConfig], provider: QuotaProvider) -> String {
    matching_providers(providers, provider)
        .find(|configured| {
            configured.id.trim().eq_ignore_ascii_case(provider.id())
                && !configured.base_url.trim().is_empty()
        })
        .or_else(|| {
            matching_providers(providers, provider)
                .find(|configured| !configured.base_url.trim().is_empty())
        })
        .map(|configured| quota_endpoint(provider, &configured.base_url))
        .unwrap_or_else(|| provider.default_endpoint().to_string())
}

fn quota_endpoint(provider: QuotaProvider, base_url: &str) -> String {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return provider.default_endpoint().to_string();
    }
    match provider {
        QuotaProvider::DeepSeek => {
            if base_url.ends_with("/user/balance") {
                base_url.to_string()
            } else {
                let base_url = base_url.strip_suffix("/v1").unwrap_or(base_url);
                format!("{base_url}/user/balance")
            }
        }
        QuotaProvider::OpenRouter => {
            if base_url.ends_with("/key") {
                base_url.to_string()
            } else {
                format!("{base_url}/key")
            }
        }
    }
}

fn split_provider_api_keys(raw: &str) -> impl Iterator<Item = &str> {
    raw.lines()
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn unique_account_name(accounts: &[QuotaAccount], base: &str) -> String {
    if accounts.iter().all(|account| account.name != base) {
        return base.to_string();
    }
    for number in 2usize.. {
        let candidate = format!("{base} {number}");
        if accounts.iter().all(|account| account.name != candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn selected_accounts(
    config: &ApiQuotaProviderConfig,
    providers: &[ProviderConfig],
    provider: QuotaProvider,
    selected: Option<&str>,
) -> Result<Vec<QuotaAccount>> {
    let mut accounts = effective_accounts(config, providers, provider);
    if let Some(selected) = selected.map(str::trim).filter(|value| !value.is_empty()) {
        accounts.retain(|account| account.name == selected);
        if accounts.is_empty() {
            bail!("API quota account is not configured: {selected}")
        }
    } else if accounts.is_empty() {
        bail!("no API quota account is configured")
    }
    Ok(accounts)
}

async fn get_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    endpoint: &str,
    api_key: &str,
    provider: &str,
) -> Result<T> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        bail!("{provider} quota endpoint is empty")
    }
    let api_key = resolve_api_key(api_key)
        .with_context(|| format!("failed to resolve {provider} API key"))?;
    let _permit = HTTP_CONCURRENCY
        .acquire()
        .await
        .context("API quota request limiter is closed")?;
    let response = client
        .get(endpoint)
        .bearer_auth(api_key)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .with_context(|| format!("failed to query {provider} quota"))?;
    let status = response.status();
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("failed to read {provider} quota response"))?;
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            bail!("{provider} quota response exceeds {MAX_RESPONSE_BYTES} bytes")
        }
        body.extend_from_slice(&chunk);
    }
    let body = String::from_utf8(body)
        .with_context(|| format!("{provider} quota response is not UTF-8"))?;
    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/message")
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| body.chars().take(300).collect());
        bail!("{provider} returned HTTP {status}: {detail}")
    }
    serde_json::from_str(&body).with_context(|| format!("invalid {provider} quota response"))
}

fn resolve_api_key(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("API key is not configured")
    }
    if let Some(name) = value.strip_prefix("$env:") {
        let name = name.trim();
        if name.is_empty() {
            bail!("environment variable name is empty")
        }
        let resolved = std::env::var(name)
            .with_context(|| format!("environment variable {name} is not set"))?;
        if resolved.trim().is_empty() {
            bail!("environment variable {name} is empty")
        }
        return Ok(resolved);
    }
    Ok(value.to_string())
}

fn format_usd(value: f64) -> String {
    let formatted = format!("{value:.6}");
    format!("${}", formatted.trim_end_matches('0').trim_end_matches('.'))
}

#[derive(Deserialize)]
struct DeepSeekBalance {
    #[serde(default)]
    balance_infos: Vec<DeepSeekBalanceInfo>,
}

#[derive(Deserialize)]
struct DeepSeekBalanceInfo {
    currency: String,
    total_balance: String,
}

#[derive(Deserialize)]
struct OpenRouterKeyResponse {
    data: OpenRouterKeyData,
}

#[derive(Deserialize)]
struct OpenRouterKeyData {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    limit: Option<f64>,
    #[serde(default)]
    limit_remaining: Option<f64>,
    usage: f64,
    usage_daily: f64,
    usage_weekly: f64,
    usage_monthly: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_openrouter_currency_without_noise() {
        assert_eq!(format_usd(12.340000), "$12.34");
        assert_eq!(format_usd(0.0), "$0");
        assert_eq!(format_usd(0.000001), "$0.000001");
    }

    #[test]
    fn parses_deepseek_balance_schema() {
        let parsed: DeepSeekBalance = serde_json::from_str(
            r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"110.00","granted_balance":"10.00","topped_up_balance":"100.00"},{"currency":"USD","total_balance":"5.25","granted_balance":"0","topped_up_balance":"5.25"}]}"#,
        )
        .unwrap();
        let output = format_deepseek_balance(parsed).unwrap();
        assert!(output.contains("CNY"));
        assert!(output.contains("110.00"));
        assert!(output.contains("USD"));
        assert!(output.contains("5.25"));
    }

    #[test]
    fn legacy_key_becomes_the_default_account() {
        let config = ApiQuotaProviderConfig {
            api_key: "legacy".to_string(),
            accounts: Vec::new(),
        };
        let accounts = effective_accounts(&config, &[], QuotaProvider::DeepSeek);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].name, "默认账号");
        assert_eq!(accounts[0].api_key, "legacy");
        assert_eq!(accounts[0].endpoint, DEEPSEEK_BALANCE_URL);
    }

    #[test]
    fn provider_config_supplies_quota_key_and_endpoint() {
        let mut providers = ProviderConfig::default_templates();
        let deepseek = providers
            .iter_mut()
            .find(|provider| provider.id == "deepseek")
            .unwrap();
        deepseek.base_url = "https://quota.example/v1/".to_string();
        deepseek.api_key = Some("provider-key".to_string());

        let accounts = effective_accounts(
            &ApiQuotaProviderConfig::default(),
            &providers,
            QuotaProvider::DeepSeek,
        );
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].name, "DeepSeek");
        assert_eq!(accounts[0].api_key, "provider-key");
        assert_eq!(accounts[0].endpoint, "https://quota.example/user/balance");
    }

    #[test]
    fn plugin_and_provider_keys_are_deduplicated() {
        let mut providers = ProviderConfig::default_templates();
        let deepseek = providers
            .iter_mut()
            .find(|provider| provider.id == "deepseek")
            .unwrap();
        deepseek.base_url = "https://api.deepseek.com/v1".to_string();
        deepseek.api_key = Some("same-key".to_string());
        let mut quota = ApiQuotaProviderConfig::default();
        quota.accounts[0].api_key = "same-key".to_string();

        let accounts = effective_accounts(&quota, &providers, QuotaProvider::DeepSeek);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].name, "默认账号");
        assert_eq!(accounts[0].endpoint, DEEPSEEK_BALANCE_URL);
    }

    #[test]
    fn provider_config_supports_multiple_keys() {
        let mut providers = ProviderConfig::default_templates();
        let openrouter = providers
            .iter_mut()
            .find(|provider| provider.id == "openrouter")
            .unwrap();
        openrouter.api_key = Some("first-key, second-key\nthird-key".to_string());

        let accounts = effective_accounts(
            &ApiQuotaProviderConfig::default(),
            &providers,
            QuotaProvider::OpenRouter,
        );
        assert_eq!(accounts.len(), 3);
        assert_eq!(accounts[0].endpoint, OPENROUTER_KEY_URL);
        assert_eq!(accounts[1].name, "OpenRouter 2");
        assert_eq!(accounts[2].name, "OpenRouter 3");
    }

    #[test]
    fn rejects_openrouter_response_without_usage_fields() {
        assert!(serde_json::from_str::<OpenRouterKeyResponse>(r#"{"data":{}}"#).is_err());
    }
}
