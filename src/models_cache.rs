use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const API_URL: &str = "https://models.dev/api.json";

/// 人民币手动价折算 USD 的估算汇率。计费本就是估算,固定汇率的误差
/// 远小于价格本身的不确定度;真要精确对账应直接看供应商账单。
const CNY_PER_USD: f64 = 7.25;

#[derive(Debug, Deserialize)]
struct ApiResponse(HashMap<String, ApiProvider>);

#[derive(Debug, Deserialize)]
struct ApiProvider {
    #[serde(default)]
    models: HashMap<String, ApiModel>,
    #[serde(default)]
    npm: Option<String>,
    /// 该供应商的 API base URL,用来把 Miyu 配置里的自定义供应商
    /// (id 不一定与 models.dev 键一致,如 opencodego vs opencode-go)
    /// 对到目录条目上,计费估算靠它。
    #[serde(default)]
    api: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiModel {
    #[serde(default)]
    modalities: Option<ApiModalities>,
    #[serde(default)]
    limit: Option<ApiLimit>,
    #[serde(default)]
    reasoning_options: Vec<ApiReasoningOption>,
    #[serde(default)]
    provider: Option<ApiModelProvider>,
    #[serde(default)]
    cost: Option<ApiCost>,
}

/// models.dev 的模型单价,USD / 1M tokens。
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct ApiCost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

impl ApiCost {
    /// 一次调用的估算费用(USD)。cache_read ⊆ prompt(Usage 归一化
    /// 保证的不变量),命中部分按缓存价、未命中按输入价;cache_write
    /// 有单独价目才计附加费。
    pub fn estimate(&self, prompt: u64, completion: u64, cache_read: u64, cache_write: u64) -> f64 {
        let uncached = prompt.saturating_sub(cache_read) as f64;
        let read_price = self.cache_read.unwrap_or(self.input);
        (uncached * self.input
            + cache_read as f64 * read_price
            + completion as f64 * self.output
            + cache_write as f64 * self.cache_write.unwrap_or(0.0))
            / 1_000_000.0
    }
}

#[derive(Debug, Deserialize)]
struct ApiModalities {
    #[serde(default)]
    input: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ApiLimit {
    #[serde(default)]
    context: Option<u64>,
    #[serde(default)]
    input: Option<u64>,
    #[serde(default)]
    output: Option<u64>,
}

impl ApiLimit {
    /// The window Miyu may actually fill. Some catalogue entries advertise a
    /// total `context` larger than the `input` the provider will accept —
    /// opencode's big-pickle reports 200k context against a 160k input cap —
    /// and budgeting against the larger number puts compaction 20k of tokens
    /// too late, so the request overflows before it is ever compacted.
    fn usable_context(&self) -> Option<u64> {
        match (self.context, self.input.filter(|input| *input > 0)) {
            (Some(context), Some(input)) => Some(context.min(input)),
            (context, input) => context.or(input),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ApiReasoningOption {
    #[serde(rename = "effort")]
    Effort {
        #[serde(default)]
        values: Vec<Option<String>>,
    },
    #[serde(rename = "toggle")]
    Toggle,
    #[serde(rename = "budget_tokens")]
    BudgetTokens {
        #[serde(default)]
        min: Option<i64>,
        #[serde(default)]
        max: Option<i64>,
    },
}

#[derive(Debug, Deserialize)]
struct ApiModelProvider {
    #[serde(default)]
    npm: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub input_modalities: Vec<String>,
    pub context_window: Option<u64>,
    reasoning: Option<ModelReasoningInfo>,
    pub cost: Option<ApiCost>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelReasoningInfo {
    pub provider_npm: Option<String>,
    pub variants: Vec<ReasoningVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningVariant {
    pub id: String,
    pub setting: ReasoningSetting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningSetting {
    Effort(String),
    Toggle(bool),
    BudgetTokens(u64),
    Disabled,
}

struct Cache {
    data: HashMap<String, HashMap<String, ModelInfo>>,
    /// models.dev 供应商键 → 其 API base URL(尾斜杠归一),配合配置里的
    /// base_url 做供应商对齐。
    provider_api: HashMap<String, String>,
}

static CACHE: OnceLock<Mutex<Option<Cache>>> = OnceLock::new();
static PROVIDER_API_CACHE: OnceLock<Mutex<HashMap<(String, String), u64>>> = OnceLock::new();
static REFRESH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static ACTIVE_METADATA_STARTED: AtomicBool = AtomicBool::new(false);

fn cache_lock() -> &'static Mutex<Option<Cache>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

fn refresh_lock() -> &'static Mutex<()> {
    REFRESH_LOCK.get_or_init(|| Mutex::new(()))
}

fn provider_api_cache_lock() -> &'static Mutex<HashMap<(String, String), u64>> {
    PROVIDER_API_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn is_loaded() -> bool {
    cache_lock().lock().unwrap().is_some()
}

fn cache_file(paths: &crate::paths::MiyuPaths) -> PathBuf {
    paths.cache_dir.join("models_cache.json")
}

fn load_from_disk(path: &PathBuf) -> Result<Cache> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read models cache: {}", path.display()))?;
    parse_api_response(&text)
}

fn parse_api_response(text: &str) -> Result<Cache> {
    let api: ApiResponse = serde_json::from_str(text).context("failed to parse models cache")?;
    let mut result = HashMap::new();
    let mut provider_api = HashMap::new();
    for (provider_id, provider) in api.0 {
        if let Some(api_url) = provider.api.as_deref() {
            let normalized = api_url.trim().trim_end_matches('/');
            if !normalized.is_empty() {
                provider_api.insert(provider_id.clone(), normalized.to_string());
            }
        }
        let mut models = HashMap::new();
        for (model_id, model) in provider.models {
            let input = model.modalities.map(|m| m.input).unwrap_or_default();
            let limit = model.limit.unwrap_or(ApiLimit {
                context: None,
                input: None,
                output: None,
            });
            let variants = reasoning_variants(&model.reasoning_options, limit.output);
            models.insert(
                model_id,
                ModelInfo {
                    input_modalities: input,
                    context_window: limit.usable_context(),
                    reasoning: (!variants.is_empty()).then_some(ModelReasoningInfo {
                        provider_npm: model
                            .provider
                            .and_then(|model_provider| model_provider.npm)
                            .or_else(|| provider.npm.clone()),
                        variants,
                    }),
                    cost: model.cost,
                },
            );
        }
        result.insert(provider_id, models);
    }
    Ok(Cache {
        data: result,
        provider_api,
    })
}

fn reasoning_variants(
    options: &[ApiReasoningOption],
    output_limit: Option<u64>,
) -> Vec<ReasoningVariant> {
    if let Some(ApiReasoningOption::Effort { values }) = options
        .iter()
        .find(|option| matches!(option, ApiReasoningOption::Effort { .. }))
    {
        return values
            .iter()
            .map(|value| match value.as_deref().map(str::trim) {
                Some(value) if !value.is_empty() => ReasoningVariant {
                    id: value.to_string(),
                    setting: ReasoningSetting::Effort(value.to_string()),
                },
                _ => ReasoningVariant {
                    id: "none".to_string(),
                    setting: ReasoningSetting::Disabled,
                },
            })
            .collect();
    }
    let mut variants = Vec::new();
    for option in options {
        match option {
            ApiReasoningOption::Effort { .. } => unreachable!(),
            ApiReasoningOption::Toggle => {
                push_variant(
                    &mut variants,
                    "on".to_string(),
                    ReasoningSetting::Toggle(true),
                );
                push_variant(
                    &mut variants,
                    "off".to_string(),
                    ReasoningSetting::Toggle(false),
                );
            }
            ApiReasoningOption::BudgetTokens { min, max } => {
                let maximum = max
                    .and_then(|value| u64::try_from(value).ok())
                    .or(output_limit)
                    .unwrap_or_default();
                if maximum == 0 {
                    continue;
                }
                let minimum = min
                    .and_then(|value| u64::try_from(value).ok())
                    .unwrap_or_default()
                    .min(maximum);
                let high = ((maximum.saturating_add(1)) / 2).max(minimum);
                push_variant(
                    &mut variants,
                    "high".to_string(),
                    ReasoningSetting::BudgetTokens(high),
                );
                if high != maximum {
                    push_variant(
                        &mut variants,
                        "max".to_string(),
                        ReasoningSetting::BudgetTokens(maximum),
                    );
                }
            }
        }
    }
    variants
}

fn push_variant(variants: &mut Vec<ReasoningVariant>, id: String, setting: ReasoningSetting) {
    if variants.iter().any(|variant| variant.id == id) {
        return;
    }
    variants.push(ReasoningVariant { id, setting });
}

fn fetch_and_cache(path: &PathBuf) -> Result<Cache> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?;
    let text = client
        .get(API_URL)
        .header("User-Agent", "Mozilla/5.0 Miyu/0.1")
        .send()?
        .error_for_status()?
        .text()?;
    if text.trim().is_empty() {
        anyhow::bail!("models.dev returned empty response");
    }
    let cache = parse_api_response(&text)?;
    let parent = path.parent().context("models cache path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    temp.write_all(text.as_bytes())?;
    temp.persist(path)
        .map_err(|error| error.error)
        .context("failed to replace models cache")?;
    Ok(cache)
}

pub fn try_load(paths: &crate::paths::MiyuPaths) {
    let path = cache_file(paths);
    let cache = load_from_disk(&path).ok();
    if let Some(cache) = cache {
        let mut lock = cache_lock().lock().unwrap();
        *lock = Some(cache);
    }
}

pub fn try_load_active(paths: &crate::paths::MiyuPaths, config: &crate::config::AppConfig) {
    let path = cache_file(paths);
    let cache = load_from_disk(&path).ok();
    if let Some(mut cache) = cache {
        retain_configured_models(&mut cache.data, config);
        let mut lock = cache_lock().lock().unwrap();
        *lock = Some(cache);
    }
}

pub fn spawn_background_refresh(paths: crate::paths::MiyuPaths) {
    let path = cache_file(&paths);
    std::thread::spawn(move || {
        let _refresh = refresh_lock().lock().unwrap();
        let fetched = fetch_and_cache(&path).ok();
        if let Some(cache) = fetched {
            let mut lock = cache_lock().lock().unwrap();
            *lock = Some(cache);
        }
    });
}

pub fn spawn_background_refresh_active(
    paths: crate::paths::MiyuPaths,
    config: crate::config::AppConfig,
) {
    spawn_provider_api_refresh(config.providers.clone());
    let path = cache_file(&paths);
    std::thread::spawn(move || {
        let _refresh = refresh_lock().lock().unwrap();
        let fetched = fetch_and_cache(&path).ok();
        if let Some(mut cache) = fetched {
            retain_configured_models(&mut cache.data, &config);
            let mut lock = cache_lock().lock().unwrap();
            *lock = Some(cache);
        }
    });
}

pub fn ensure_active_metadata(paths: &crate::paths::MiyuPaths, config: &crate::config::AppConfig) {
    if !is_loaded() {
        try_load_active(paths, config);
    }
    if ACTIVE_METADATA_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        spawn_background_refresh_active(paths.clone(), config.clone());
    }
}

fn retain_configured_models(
    data: &mut HashMap<String, HashMap<String, ModelInfo>>,
    config: &crate::config::AppConfig,
) {
    let mut selected = HashMap::<String, HashSet<String>>::new();
    let mut selected_model_ids = HashSet::new();
    for provider in &config.providers {
        selected
            .entry(provider.id.clone())
            .or_default()
            .insert(provider.default_model.clone());
        if !provider.default_model.trim().is_empty() {
            selected_model_ids.insert(provider.default_model.clone());
        }
    }
    let conversation_models = config.platforms.qq.conversations.iter().flat_map(|route| {
        route
            .text_models
            .iter()
            .flatten()
            .chain(route.multimodal_models.iter().flatten())
    });
    let real_context_models = config
        .platforms
        .qq
        .plugins
        .get(crate::config::REAL_CONTEXT_PLUGIN_ID)
        .and_then(|instance| crate::config::RealContextPluginSettings::from_instance(instance).ok())
        .and_then(|settings| settings.text_models)
        .unwrap_or_default();
    for choice in config
        .active_provider_models
        .iter()
        .flatten()
        .chain(config.active_multimodal_provider_models.iter().flatten())
        .chain(config.platforms.qq.text_models.iter().flatten())
        .chain(config.platforms.qq.multimodal_models.iter().flatten())
        .chain(
            config
                .platforms
                .qq
                .non_whitelist_text_models
                .iter()
                .flatten(),
        )
        .chain(conversation_models)
        .chain(real_context_models.iter())
    {
        selected
            .entry(choice.provider_id.clone())
            .or_default()
            .insert(choice.model.clone());
        selected_model_ids.insert(choice.model.clone());
    }
    data.retain(|provider_id, models| {
        let provider_models = selected.get(provider_id);
        models.retain(|model_id, _| {
            provider_models.is_some_and(|ids| ids.contains(model_id))
                || selected_model_ids.contains(model_id)
        });
        !models.is_empty()
    });
}

pub fn input_modalities(provider_id: &str, model_id: &str) -> Option<Vec<String>> {
    let lock = cache_lock().lock().unwrap();
    let cache = lock.as_ref()?;
    lookup_input_modalities(&cache.data, provider_id, model_id)
}

pub fn input_modalities_blocking(
    paths: &crate::paths::MiyuPaths,
    provider_id: &str,
    model_id: &str,
) -> Option<Vec<String>> {
    if let Some(modalities) = input_modalities(provider_id, model_id) {
        return Some(modalities);
    }
    refresh_blocking(paths).ok()?;
    input_modalities(provider_id, model_id)
}

fn lookup_input_modalities(
    data: &HashMap<String, HashMap<String, ModelInfo>>,
    provider_id: &str,
    model_id: &str,
) -> Option<Vec<String>> {
    if let Some(info) = data
        .get(provider_id)
        .and_then(|provider| provider.get(model_id))
        .filter(|info| !info.input_modalities.is_empty())
    {
        return Some(info.input_modalities.clone());
    }

    let mut matches = data
        .values()
        .filter_map(|provider| provider.get(model_id))
        .filter(|info| !info.input_modalities.is_empty())
        .map(|info| info.input_modalities.clone())
        .collect::<Vec<_>>();
    matches.sort();
    matches.dedup();
    if matches.len() == 1 {
        matches.pop()
    } else {
        None
    }
}

pub fn context_window(provider_id: &str, model_id: &str) -> Option<u64> {
    if let Some(window) = provider_api_cache_lock()
        .lock()
        .unwrap()
        .get(&(provider_id.to_string(), model_id.to_string()))
        .copied()
    {
        return Some(window);
    }
    let lock = cache_lock().lock().unwrap();
    let cache = lock.as_ref()?;
    lookup_context_window(&cache.data, provider_id, model_id)
}

#[derive(Debug, Serialize, Deserialize)]
struct ProviderApiCacheEntry {
    provider_id: String,
    model: String,
    context_window: u64,
}

pub fn spawn_provider_api_refresh(providers: Vec<crate::config::ProviderConfig>) {
    std::thread::spawn(move || {
        let mut discovered = Vec::new();
        for provider in providers {
            if let Ok(entries) = fetch_provider_context_windows(&provider) {
                discovered.extend(entries);
            }
        }
        if discovered.is_empty() {
            return;
        }
        let mut cache = provider_api_cache_lock().lock().unwrap();
        for entry in discovered {
            cache.insert((entry.provider_id, entry.model), entry.context_window);
        }
    });
}

fn fetch_provider_context_windows(
    provider: &crate::config::ProviderConfig,
) -> Result<Vec<ProviderApiCacheEntry>> {
    let mut api_key = provider.api_key.as_deref().unwrap_or_default();
    if api_key.is_empty() {
        return Ok(Vec::new());
    }
    let resolved_key;
    if let Some(env_name) = api_key.strip_prefix("$env:") {
        resolved_key = std::env::var(env_name).unwrap_or_default();
        api_key = &resolved_key;
    }
    if api_key.is_empty() {
        return Ok(Vec::new());
    }
    let url = provider_models_url(&provider.base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.min(5)))
        .build()?;
    let mut request = client
        .get(url)
        .header("Accept", "application/json")
        .header("User-Agent", "miyu-model-metadata");
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    let value = request.send()?.error_for_status()?.json::<Value>()?;
    let Some(models) = value.get("data").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    Ok(models
        .iter()
        .filter_map(|model| {
            let id = model.get("id")?.as_str()?.trim();
            let context_window = api_context_window(model)?;
            (!id.is_empty() && context_window > 0).then(|| ProviderApiCacheEntry {
                provider_id: provider.id.clone(),
                model: id.to_string(),
                context_window,
            })
        })
        .collect())
}

fn provider_models_url(base_url: &str) -> String {
    let mut url = base_url.trim().trim_end_matches('/').to_string();
    if url.ends_with("/chat/completions") {
        url.truncate(url.len() - "/chat/completions".len());
    }
    if url.ends_with("/v1") {
        format!("{url}/models")
    } else {
        format!("{url}/v1/models")
    }
}

fn api_context_window(model: &Value) -> Option<u64> {
    for key in [
        "context_window",
        "context_length",
        "max_context_length",
        "max_input_tokens",
        "input_token_limit",
    ] {
        if let Some(value) = model.get(key).and_then(Value::as_u64).filter(|v| *v > 0) {
            return Some(value);
        }
    }
    for parent in ["limit", "limits"] {
        if let Some(value) = model
            .get(parent)
            .and_then(|value| value.get("context"))
            .and_then(Value::as_u64)
            .filter(|v| *v > 0)
        {
            return Some(value);
        }
    }
    None
}

pub fn reasoning_info(provider_id: &str, model_id: &str) -> Option<ModelReasoningInfo> {
    let lock = cache_lock().lock().unwrap();
    let cache = lock.as_ref()?;
    lookup_reasoning_info(&cache.data, provider_id, model_id)
}

fn lookup_reasoning_info(
    data: &HashMap<String, HashMap<String, ModelInfo>>,
    provider_id: &str,
    model_id: &str,
) -> Option<ModelReasoningInfo> {
    if let Some(info) = data
        .get(provider_id)
        .and_then(|provider| provider.get(model_id))
    {
        return info.reasoning.clone();
    }

    for canonical_provider in canonical_provider_candidates(data, model_id) {
        if let Some(info) = data
            .get(&canonical_provider)
            .and_then(|provider| provider.get(model_id))
        {
            return info.reasoning.clone();
        }
    }

    let matches = data
        .values()
        .filter_map(|provider| provider.get(model_id))
        .map(|info| info.reasoning.clone())
        .collect::<Vec<_>>();
    let mut groups = Vec::<(Option<ModelReasoningInfo>, usize)>::new();
    for info in matches {
        if let Some((existing, count)) =
            groups
                .iter_mut()
                .find(|(existing, _)| match (existing.as_ref(), info.as_ref()) {
                    (Some(existing), Some(info)) => existing.variants == info.variants,
                    (None, None) => true,
                    _ => false,
                })
        {
            *count += 1;
            if let (Some(existing), Some(info)) = (existing.as_mut(), info.as_ref()) {
                if existing.provider_npm != info.provider_npm {
                    existing.provider_npm = None;
                }
            }
        } else {
            groups.push((info, 1));
        }
    }
    groups.sort_by(|left, right| right.1.cmp(&left.1));
    let (info, count) = groups.first()?;
    if groups
        .get(1)
        .is_some_and(|(_, next_count)| next_count == count)
    {
        return None;
    }
    info.clone()
}

fn canonical_provider_candidates(
    data: &HashMap<String, HashMap<String, ModelInfo>>,
    model_id: &str,
) -> Vec<String> {
    let lower = model_id.to_ascii_lowercase();
    let mut candidates = Vec::new();
    if let Some((namespace, _)) = lower.split_once('/') {
        candidates.push(namespace.to_string());
    }
    let alias = if lower.starts_with("gpt-")
        || lower.starts_with("o1")
        || lower.starts_with("o3")
        || lower.starts_with("o4")
    {
        Some("openai")
    } else if lower.starts_with("claude-") {
        Some("anthropic")
    } else if lower.starts_with("gemini-") {
        Some("google")
    } else if lower.starts_with("grok-") {
        Some("xai")
    } else if lower.starts_with("qwen") {
        Some("alibaba")
    } else {
        None
    };
    if let Some(alias) = alias {
        candidates.push(alias.to_string());
    }
    let mut prefixes = data
        .keys()
        .filter(|provider_id| lower.starts_with(&format!("{}-", provider_id.to_ascii_lowercase())))
        .cloned()
        .collect::<Vec<_>>();
    prefixes.sort_by_key(|provider_id| std::cmp::Reverse(provider_id.len()));
    candidates.extend(prefixes);
    candidates.dedup();
    candidates
}

fn lookup_context_window(
    data: &HashMap<String, HashMap<String, ModelInfo>>,
    provider_id: &str,
    model_id: &str,
) -> Option<u64> {
    if let Some(window) = data
        .get(provider_id)
        .and_then(|provider| provider.get(model_id))
        .and_then(|info| info.context_window)
    {
        return Some(window);
    }

    let mut matches = data
        .values()
        .filter_map(|provider| provider.get(model_id))
        .filter_map(|info| info.context_window)
        .collect::<Vec<_>>();
    matches.sort_unstable();
    matches.dedup();
    matches.into_iter().min()
}

/// 模型单价查询,供计费估算。供应商对齐两步走:① Miyu 供应商 id 恰好是
/// models.dev 键(deepseek、openrouter 等官方模板);② 按 base_url 对齐
/// (自定义 id,如 opencodego → opencode-go)。都对不上就不猜——同名
/// 模型在不同渠道价格不同,跨供应商模糊匹配会算错钱。
pub fn model_cost(provider_id: &str, base_url: &str, model_id: &str) -> Option<ApiCost> {
    let lock = cache_lock().lock().unwrap();
    let cache = lock.as_ref()?;
    if let Some(cost) = cache
        .data
        .get(provider_id)
        .and_then(|models| models.get(model_id))
        .and_then(|info| info.cost)
    {
        return Some(cost);
    }
    let normalized = base_url.trim().trim_end_matches('/');
    if normalized.is_empty() {
        return None;
    }
    cache
        .provider_api
        .iter()
        .filter(|(_, api)| api.as_str() == normalized)
        .find_map(|(key, _)| {
            cache
                .data
                .get(key)
                .and_then(|models| models.get(model_id))
                .and_then(|info| info.cost)
        })
}

/// 用量统计的计价器:usage 记录只存供应商 id,这里借 config 把 id 解析
/// 成 base_url 再查目录。查不到价的记录计 None(前端显示为无估算),
/// 绝不糊弄一个数字。
pub fn pricing_resolver(
    config: &crate::config::AppConfig,
) -> impl Fn(&str, &str) -> Option<ApiCost> + '_ {
    move |provider_id: &str, model_id: &str| {
        if provider_id.is_empty() || model_id.is_empty() {
            return None;
        }
        let provider = config
            .providers
            .iter()
            .find(|provider| provider.id == provider_id);
        // 手动价格优先:目录没收录的中转/赠送端点靠它。CNY 按估算汇率
        // 折成 USD 聚合(统计页统一以 $ 展示)。
        if let Some(manual) = provider.and_then(|p| p.model_costs.get(model_id)) {
            let rate = match manual.currency {
                crate::config::CostCurrency::Usd => 1.0,
                crate::config::CostCurrency::Cny => 1.0 / CNY_PER_USD,
            };
            return Some(ApiCost {
                input: manual.input * rate,
                output: manual.output * rate,
                cache_read: manual.cache_read.map(|price| price * rate),
                cache_write: None,
            });
        }
        let base_url = provider.map(|p| p.base_url.as_str()).unwrap_or("");
        model_cost(provider_id, base_url, model_id)
    }
}

pub fn refresh_blocking(paths: &crate::paths::MiyuPaths) -> Result<()> {
    let _refresh = refresh_lock().lock().unwrap();
    if is_loaded() {
        return Ok(());
    }
    let path = cache_file(paths);
    let cache = fetch_and_cache(&path)?;
    let mut lock = cache_lock().lock().unwrap();
    *lock = Some(cache);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(window: u64) -> ModelInfo {
        ModelInfo {
            input_modalities: Vec::new(),
            context_window: Some(window),
            reasoning: None,
            cost: None,
        }
    }

    /// 手动价格优先于目录价:目录未收录的中转端点靠 model_costs。
    #[test]
    fn manual_model_cost_overrides_catalogue() {
        let mut config = crate::config::AppConfig::default();
        config.providers.push(crate::config::ProviderConfig {
            id: "relay".to_string(),
            display_name: "Relay".to_string(),
            base_url: "https://relay.example/v1".to_string(),
            protocol: "openai-chat".to_string(),
            api_key: None,
            models: vec!["m".to_string()],
            model_context_window: HashMap::new(),
model_temperature: HashMap::new(),
            model_modalities: HashMap::new(),
            model_costs: HashMap::from([(
                "m".to_string(),
                crate::config::ModelCostConfig {
                    currency: crate::config::CostCurrency::Usd,
                    input: 1.5,
                    output: 3.0,
                    cache_read: Some(0.15),
                },
            )]),
            default_model: "m".to_string(),
            timeout_seconds: 60,
            temperature: 1.0,
            anthropic_max_tokens: 4096,
            extra_body: None,
        });
        {
            let price = pricing_resolver(&config);
            let cost = price("relay", "m").expect("manual price should resolve");
            assert_eq!(cost.input, 1.5);
            assert_eq!(cost.output, 3.0);
            assert_eq!(cost.cache_read, Some(0.15));
            assert_eq!(cost.cache_write, None);
        }
        // CNY 手动价按估算汇率折 USD
        config.providers.last_mut().unwrap().model_costs.insert(
            "m".to_string(),
            crate::config::ModelCostConfig {
                currency: crate::config::CostCurrency::Cny,
                input: 7.25,
                output: 14.5,
                cache_read: None,
            },
        );
        let price = pricing_resolver(&config);
        let cost = price("relay", "m").unwrap();
        assert!((cost.input - 1.0).abs() < 1e-9);
        assert!((cost.output - 2.0).abs() < 1e-9);
        assert_eq!(cost.cache_read, None);
    }

    /// 单价解析与估算:cache_read ⊆ prompt,命中按缓存价、未命中按输入价。
    #[test]
    fn cost_parses_and_estimates() {
        let parsed = parse_api_response(
            r#"{"opencode-go":{"api":"https://opencode.ai/zen/go/v1/","models":{
                "deepseek-v4-flash":{"cost":{"input":0.07,"output":0.14,"cache_read":0.0014}},
                "no-cost":{}
            }}}"#,
        )
        .unwrap();
        assert_eq!(
            parsed.provider_api["opencode-go"],
            "https://opencode.ai/zen/go/v1"
        );
        assert!(parsed.data["opencode-go"]["no-cost"].cost.is_none());
        let cost = parsed.data["opencode-go"]["deepseek-v4-flash"].cost.unwrap();
        // 200 万 prompt(其中 100 万命中)+ 100 万输出
        let est = cost.estimate(2_000_000, 1_000_000, 1_000_000, 0);
        assert!((est - (0.07 + 0.0014 + 0.14)).abs() < 1e-9, "{est}");
        // 无缓存价时命中按输入价计
        let flat = ApiCost { input: 1.0, output: 2.0, cache_read: None, cache_write: None };
        assert!((flat.estimate(1_000_000, 0, 400_000, 0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn catalogue_context_window_is_capped_by_the_input_limit() {
        // opencode's big-pickle advertises a 200k context against a 160k input
        // cap; budgeting against 200k puts compaction past the point the
        // provider still accepts the request.
        let parsed = parse_api_response(
            r#"{"opencode":{"models":{
                "big-pickle":{"limit":{"context":200000,"input":160000,"output":32000}},
                "context-only":{"limit":{"context":128000,"output":8000}},
                "input-only":{"limit":{"input":64000}},
                "input-zero":{"limit":{"context":32000,"input":0}},
                "no-limit":{}
            }}}"#,
        )
        .unwrap();
        let models = &parsed.data["opencode"];

        assert_eq!(models["big-pickle"].context_window, Some(160_000));
        assert_eq!(models["context-only"].context_window, Some(128_000));
        assert_eq!(models["input-only"].context_window, Some(64_000));
        assert_eq!(models["input-zero"].context_window, Some(32_000));
        assert_eq!(models["no-limit"].context_window, None);
    }

    #[test]
    fn context_window_prefers_exact_provider() {
        let data = HashMap::from([
            (
                "provider-a".to_string(),
                HashMap::from([("shared-model".to_string(), model(128_000))]),
            ),
            (
                "provider-b".to_string(),
                HashMap::from([("shared-model".to_string(), model(200_000))]),
            ),
        ]);

        assert_eq!(
            lookup_context_window(&data, "provider-a", "shared-model"),
            Some(128_000)
        );
    }

    #[test]
    fn compact_cache_retains_only_configured_models() {
        let config = crate::config::AppConfig::default();
        let provider = &config.providers[0];
        let mut data = HashMap::from([
            (
                provider.id.clone(),
                HashMap::from([
                    (provider.default_model.clone(), model(128_000)),
                    ("unused-model".to_string(), model(64_000)),
                ]),
            ),
            (
                "unused-provider".to_string(),
                HashMap::from([("unused-model".to_string(), model(32_000))]),
            ),
        ]);

        retain_configured_models(&mut data, &config);

        assert!(!data.contains_key("unused-provider"));
        assert!(data[&provider.id].contains_key(&provider.default_model));
        assert!(!data[&provider.id].contains_key("unused-model"));
    }

    #[test]
    fn compact_cache_retains_models_used_only_by_platform_routes() {
        let mut config = crate::config::AppConfig::default();
        let provider_id = config.providers[0].id.clone();
        config.providers[0].models.extend([
            "route-text".to_string(),
            "route-vision".to_string(),
            "platform-text".to_string(),
            "non-whitelist-text".to_string(),
            "context-text".to_string(),
        ]);
        config.platforms.qq.text_models = Some(vec![crate::config::ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: "platform-text".to_string(),
        }]);
        config.platforms.qq.non_whitelist_text_models =
            Some(vec![crate::config::ActiveProviderModelConfig {
                provider_id: provider_id.clone(),
                model: "non-whitelist-text".to_string(),
            }]);
        let mut real_context = crate::config::PlatformPluginInstanceConfig::default();
        crate::config::merge_real_context_settings(
            &mut real_context,
            &crate::config::RealContextPluginSettings {
                text_models: Some(vec![crate::config::ActiveProviderModelConfig {
                    provider_id: provider_id.clone(),
                    model: "context-text".to_string(),
                }]),
                ..Default::default()
            },
        );
        config.platforms.qq.plugins.insert(
            crate::config::REAL_CONTEXT_PLUGIN_ID.to_string(),
            real_context,
        );
        config
            .platforms
            .qq
            .conversations
            .push(crate::config::PlatformModelRoute {
                conversation: crate::config::PlatformConversationConfig {
                    kind: crate::config::PlatformConversationKind::Group,
                    id: "20000".to_string(),
                },
                persona: crate::config::PlatformPersonaOverride::Inherit,
                text_models_inheritance: crate::config::PlatformModelPoolInheritance::Platform,
                text_models: Some(vec![crate::config::ActiveProviderModelConfig {
                    provider_id: provider_id.clone(),
                    model: "route-text".to_string(),
                }]),
                multimodal_models_inheritance:
                    crate::config::PlatformModelPoolInheritance::Platform,
                multimodal_models: Some(vec![crate::config::ActiveProviderModelConfig {
                    provider_id: provider_id.clone(),
                    model: "route-vision".to_string(),
                }]),
                extra_prompt: String::new(),
                session_limits: None,
            });
        let mut data = HashMap::from([(
            provider_id.clone(),
            HashMap::from([
                (config.providers[0].default_model.clone(), model(128_000)),
                ("route-text".to_string(), model(64_000)),
                ("route-vision".to_string(), model(96_000)),
                ("platform-text".to_string(), model(64_000)),
                ("non-whitelist-text".to_string(), model(64_000)),
                ("context-text".to_string(), model(64_000)),
                ("unused-model".to_string(), model(32_000)),
            ]),
        )]);

        retain_configured_models(&mut data, &config);

        let retained = &data[&provider_id];
        assert!(retained.contains_key("route-text"));
        assert!(retained.contains_key("route-vision"));
        assert!(retained.contains_key("platform-text"));
        assert!(retained.contains_key("non-whitelist-text"));
        assert!(retained.contains_key("context-text"));
        assert!(!retained.contains_key("unused-model"));
    }

    #[test]
    fn compact_cache_retains_same_model_metadata_from_other_providers() {
        let mut config = crate::config::AppConfig::default();
        let provider = &mut config.providers[0];
        provider.models = vec!["custom-model".to_string()];
        provider.default_model = "custom-model".to_string();
        let mut data = HashMap::from([
            (
                provider.id.clone(),
                HashMap::from([("custom-model".to_string(), model(64_000))]),
            ),
            (
                "catalog-provider".to_string(),
                HashMap::from([("custom-model".to_string(), model(128_000))]),
            ),
        ]);

        retain_configured_models(&mut data, &config);

        assert!(data.contains_key("catalog-provider"));
        assert_eq!(
            lookup_context_window(&data, "custom-provider", "custom-model"),
            Some(64_000)
        );
    }

    #[test]
    fn provider_api_context_window_accepts_common_metadata_shapes() {
        assert_eq!(
            api_context_window(&serde_json::json!({"context_window": 128000})),
            Some(128000)
        );
        assert_eq!(
            api_context_window(&serde_json::json!({"limit": {"context": 64000}})),
            Some(64000)
        );
        assert_eq!(
            api_context_window(&serde_json::json!({"id": "model"})),
            None
        );
    }

    #[test]
    fn context_window_fallback_uses_the_conservative_minimum() {
        let same = HashMap::from([
            (
                "provider-a".to_string(),
                HashMap::from([("shared-model".to_string(), model(200_000))]),
            ),
            (
                "provider-b".to_string(),
                HashMap::from([("shared-model".to_string(), model(200_000))]),
            ),
        ]);
        assert_eq!(
            lookup_context_window(&same, "custom", "shared-model"),
            Some(200_000)
        );

        let mut conflicting = same;
        conflicting
            .get_mut("provider-b")
            .unwrap()
            .insert("shared-model".to_string(), model(128_000));
        assert_eq!(
            lookup_context_window(&conflicting, "custom", "shared-model"),
            Some(128_000)
        );
    }

    #[test]
    fn parses_reasoning_options_with_provider_mapping() {
        let data = parse_api_response(
            r#"{
                "openrouter": {
                    "npm": "@openrouter/ai-sdk-provider",
                    "models": {
                        "example": {
                            "limit": { "context": 128000, "output": 32000 },
                            "reasoning_options": [
                                { "type": "effort", "values": ["low", "high", null] },
                                { "type": "budget_tokens", "min": -1, "max": 8000 }
                            ]
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let info = lookup_reasoning_info(&data.data, "openrouter", "example").unwrap();
        assert_eq!(
            info.provider_npm.as_deref(),
            Some("@openrouter/ai-sdk-provider")
        );
        assert_eq!(
            info.variants,
            vec![
                ReasoningVariant {
                    id: "low".to_string(),
                    setting: ReasoningSetting::Effort("low".to_string()),
                },
                ReasoningVariant {
                    id: "high".to_string(),
                    setting: ReasoningSetting::Effort("high".to_string()),
                },
                ReasoningVariant {
                    id: "none".to_string(),
                    setting: ReasoningSetting::Disabled,
                },
            ]
        );
    }

    #[test]
    fn negative_budget_min_uses_zero_floor() {
        let variants = reasoning_variants(
            &[ApiReasoningOption::BudgetTokens {
                min: Some(-1),
                max: Some(8000),
            }],
            Some(32_000),
        );
        assert_eq!(
            variants,
            vec![
                ReasoningVariant {
                    id: "high".to_string(),
                    setting: ReasoningSetting::BudgetTokens(4000),
                },
                ReasoningVariant {
                    id: "max".to_string(),
                    setting: ReasoningSetting::BudgetTokens(8000),
                },
            ]
        );
    }

    #[test]
    fn reasoning_fallback_keeps_shared_variants_without_provider_mapping() {
        let variants = vec![ReasoningVariant {
            id: "high".to_string(),
            setting: ReasoningSetting::Effort("high".to_string()),
        }];
        let data = HashMap::from([
            (
                "provider-a".to_string(),
                HashMap::from([(
                    "shared-model".to_string(),
                    ModelInfo {
                        input_modalities: Vec::new(),
                        context_window: None,
                        reasoning: Some(ModelReasoningInfo {
                            provider_npm: Some("@provider/a".to_string()),
                            variants: variants.clone(),
                        }),
                        cost: None,
                    },
                )]),
            ),
            (
                "provider-b".to_string(),
                HashMap::from([(
                    "shared-model".to_string(),
                    ModelInfo {
                        input_modalities: Vec::new(),
                        context_window: None,
                        reasoning: Some(ModelReasoningInfo {
                            provider_npm: Some("@provider/b".to_string()),
                            variants,
                        }),
                        cost: None,
                    },
                )]),
            ),
        ]);

        let info = lookup_reasoning_info(&data, "custom", "shared-model").unwrap();
        assert_eq!(info.provider_npm, None);
        assert_eq!(info.variants.len(), 1);
    }

    #[test]
    fn reasoning_fallback_prefers_canonical_model_provider() {
        let high_max = vec![
            ReasoningVariant {
                id: "high".to_string(),
                setting: ReasoningSetting::Effort("high".to_string()),
            },
            ReasoningVariant {
                id: "max".to_string(),
                setting: ReasoningSetting::Effort("max".to_string()),
            },
        ];
        let low = vec![ReasoningVariant {
            id: "low".to_string(),
            setting: ReasoningSetting::Effort("low".to_string()),
        }];
        let reasoning = |variants| ModelInfo {
            cost: None,
            input_modalities: Vec::new(),
            context_window: None,
            reasoning: Some(ModelReasoningInfo {
                provider_npm: Some("@ai-sdk/openai-compatible".to_string()),
                variants,
            }),
        };
        let data = HashMap::from([
            (
                "deepseek".to_string(),
                HashMap::from([("deepseek-v4-flash".to_string(), reasoning(high_max.clone()))]),
            ),
            (
                "gateway".to_string(),
                HashMap::from([("deepseek-v4-flash".to_string(), reasoning(low))]),
            ),
        ]);

        let info = lookup_reasoning_info(&data, "ririxin", "deepseek-v4-flash").unwrap();
        assert_eq!(info.variants, high_max);
    }

    #[test]
    fn reasoning_fallback_counts_models_without_variants() {
        let reasoning = ModelInfo {
            cost: None,
            input_modalities: Vec::new(),
            context_window: None,
            reasoning: Some(ModelReasoningInfo {
                provider_npm: None,
                variants: vec![ReasoningVariant {
                    id: "high".to_string(),
                    setting: ReasoningSetting::Effort("high".to_string()),
                }],
            }),
        };
        let without_reasoning = ModelInfo {
            cost: None,
            input_modalities: Vec::new(),
            context_window: None,
            reasoning: None,
        };
        let data = HashMap::from([
            (
                "gateway-a".to_string(),
                HashMap::from([("custom-model".to_string(), reasoning)]),
            ),
            (
                "gateway-b".to_string(),
                HashMap::from([("custom-model".to_string(), without_reasoning.clone())]),
            ),
            (
                "gateway-c".to_string(),
                HashMap::from([("custom-model".to_string(), without_reasoning)]),
            ),
        ]);

        assert_eq!(
            lookup_reasoning_info(&data, "private", "custom-model"),
            None
        );
    }
}
