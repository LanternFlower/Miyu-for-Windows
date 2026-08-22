//! 搜索供应商与 API key 的轮换。
//!
//! 两级冷却：供应商级和 key 级。一个 key 被限流不代表这家不能用，把整家拉黑是
//! 浪费；反过来这家整体挂了，再换 key 也没用。
//!
//! 冷却时长按失败类型给（`cooldown_for_status` / `cooldown_for_error`）——429
//! 要等久一点，连接失败很快就能重试。

use crate::tools::web::*;

#[derive(Clone, Copy)]
pub(in crate::tools::web) enum SearchProvider {
    Tavily,
    Firecrawl,
    AnySearch,
    Exa,
    SearXng,
    DuckDuckGo,
}

impl SearchProvider {
    pub(in crate::tools::web) fn id(self) -> &'static str {
        match self {
            Self::Tavily => "tavily",
            Self::Firecrawl => "firecrawl",
            Self::AnySearch => "anysearch",
            Self::Exa => "exa",
            Self::SearXng => "searxng",
            Self::DuckDuckGo => "duckduckgo",
        }
    }
}

#[derive(Default)]
pub(in crate::tools::web) struct SearchScheduler {
    pub(in crate::tools::web) provider_cursor: usize,
    pub(in crate::tools::web) key_cursors: HashMap<&'static str, usize>,
    pub(in crate::tools::web) cooldowns: HashMap<String, Instant>,
}

impl SearchScheduler {
    pub(in crate::tools::web) fn ordered_providers(&mut self, providers: &[SearchProvider]) -> Vec<SearchProvider> {
        let available = providers
            .iter()
            .copied()
            .filter(|provider| self.is_ready(&provider_cooldown_id(provider.id())))
            .collect::<Vec<_>>();
        if available.is_empty() {
            return Vec::new();
        }
        let start = self.provider_cursor % available.len();
        self.provider_cursor = self.provider_cursor.wrapping_add(1);
        rotate_from(available, start)
    }

    pub(in crate::tools::web) fn ordered_key_positions(&mut self, provider: &'static str, key_count: usize) -> Vec<usize> {
        let available = (0..key_count)
            .filter(|&index| self.is_ready(&key_cooldown_id(provider, index)))
            .collect::<Vec<_>>();
        if available.is_empty() {
            return Vec::new();
        }
        let cursor = self.key_cursors.entry(provider).or_insert(0);
        let start = *cursor % available.len();
        *cursor = cursor.wrapping_add(1);
        rotate_from(available, start)
    }

    pub(in crate::tools::web) fn is_ready(&mut self, id: &str) -> bool {
        match self.cooldowns.get(id).copied() {
            Some(until) if until > Instant::now() => false,
            Some(_) => {
                self.cooldowns.remove(id);
                true
            }
            None => true,
        }
    }

    pub(in crate::tools::web) fn mark_success(&mut self, id: &str) {
        self.cooldowns.remove(id);
    }

    pub(in crate::tools::web) fn mark_failure(&mut self, id: String, duration: Duration) {
        self.cooldowns.insert(id, Instant::now() + duration);
    }
}

pub(in crate::tools::web) fn rotate_from<T>(mut items: Vec<T>, start: usize) -> Vec<T> {
    items.rotate_left(start);
    items
}

pub(in crate::tools::web) fn provider_cooldown_id(provider: &str) -> String {
    format!("provider:{provider}")
}

pub(in crate::tools::web) fn key_cooldown_id(provider: &str, index: usize) -> String {
    format!("key:{provider}:{index}")
}

pub(in crate::tools::web) fn has_non_empty_key(keys: &[String]) -> bool {
    keys.iter().any(|key| !key.trim().is_empty())
}

pub(in crate::tools::web) fn configured_primary_providers(config: &WebPluginConfig) -> Vec<SearchProvider> {
    let mut providers = Vec::new();
    if has_non_empty_key(&config.tavily_api_keys) {
        providers.push(SearchProvider::Tavily);
    }
    if has_non_empty_key(&config.firecrawl_api_keys) {
        providers.push(SearchProvider::Firecrawl);
    }
    if has_non_empty_key(&config.anysearch_api_keys) {
        providers.push(SearchProvider::AnySearch);
    }
    if has_non_empty_key(&config.exa_api_keys) {
        providers.push(SearchProvider::Exa);
    }
    if !config.searxng_base_url.trim().is_empty() {
        providers.push(SearchProvider::SearXng);
    }
    providers
}

pub(in crate::tools::web) fn ordered_providers(providers: &[SearchProvider]) -> Vec<SearchProvider> {
    SEARCH_SCHEDULER
        .lock()
        .map(|mut scheduler| scheduler.ordered_providers(providers))
        .unwrap_or_else(|_| providers.to_vec())
}

pub(in crate::tools::web) fn ordered_key_positions(provider: &'static str, key_count: usize) -> Vec<usize> {
    SEARCH_SCHEDULER
        .lock()
        .map(|mut scheduler| scheduler.ordered_key_positions(provider, key_count))
        .unwrap_or_else(|_| (0..key_count).collect())
}

pub(in crate::tools::web) fn mark_provider_success(provider: &str) {
    if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
        scheduler.mark_success(&provider_cooldown_id(provider));
    }
}

pub(in crate::tools::web) fn mark_key_success(provider: &'static str, index: usize) {
    if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
        scheduler.mark_success(&key_cooldown_id(provider, index));
    }
}

pub(in crate::tools::web) fn mark_provider_failure(provider: &str, error: &str) {
    let Some(duration) = cooldown_for_error(error) else {
        return;
    };
    if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
        scheduler.mark_failure(provider_cooldown_id(provider), duration);
    }
}

pub(in crate::tools::web) fn mark_key_failure(provider: &'static str, index: usize, error: &str) {
    let Some(duration) = cooldown_for_error(error) else {
        return;
    };
    if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
        scheduler.mark_failure(key_cooldown_id(provider, index), duration);
    }
}

pub(in crate::tools::web) fn cooldown_for_status(status: u16) -> Option<Duration> {
    match status {
        401 | 403 | 429 => Some(Duration::from_secs(600)),
        408 | 500..=599 => Some(Duration::from_secs(120)),
        _ => None,
    }
}

pub(in crate::tools::web) fn cooldown_for_error(error: &str) -> Option<Duration> {
    let lower = error.to_ascii_lowercase();
    if lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("ratelimit")
        || lower.contains("quota")
    {
        return Some(Duration::from_secs(600));
    }
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("invalid api key")
    {
        return Some(Duration::from_secs(600));
    }
    if lower.contains("408")
        || lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("504")
        || lower.contains("request failed")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection")
    {
        return Some(Duration::from_secs(120));
    }
    if lower.contains("captcha") || lower.contains("challenge") {
        return Some(Duration::from_secs(300));
    }
    None
}

pub(in crate::tools::web) fn search_provider_order(provider: &str, config: &WebPluginConfig) -> Result<Vec<SearchProvider>> {
    if provider == "auto" {
        let mut providers = ordered_providers(&configured_primary_providers(config));
        // 未配置 key 时 Exa 走官方 MCP 免费公共额度：排在已配置服务之后、爬虫之前；
        // 报错/429 会通过 cooldown 自动让位给爬虫
        if !has_non_empty_key(&config.exa_api_keys)
            && SEARCH_SCHEDULER
                .lock()
                .map(|mut scheduler| scheduler.is_ready(&provider_cooldown_id("exa")))
                .unwrap_or(true)
        {
            providers.push(SearchProvider::Exa);
        }
        if SEARCH_SCHEDULER
            .lock()
            .map(|mut scheduler| scheduler.is_ready(&provider_cooldown_id("duckduckgo")))
            .unwrap_or(true)
        {
            providers.push(SearchProvider::DuckDuckGo);
        }
        if providers.is_empty() {
            return Ok(vec![SearchProvider::DuckDuckGo]);
        }
        return Ok(providers);
    }
    match provider {
        "tavily" => Ok(vec![SearchProvider::Tavily]),
        "firecrawl" => Ok(vec![SearchProvider::Firecrawl]),
        "anysearch" => Ok(vec![SearchProvider::AnySearch]),
        "exa" => Ok(vec![SearchProvider::Exa]),
        "searxng" => Ok(vec![SearchProvider::SearXng]),
        "duckduckgo" | "script" => Ok(vec![SearchProvider::DuckDuckGo]),
        _ => bail!("{provider}: unknown provider"),
    }
}

pub(in crate::tools::web) async fn search_with_provider(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
    config: &WebPluginConfig,
    provider: SearchProvider,
) -> Result<String> {
    match provider {
        SearchProvider::Tavily => {
            search_tavily(client, query, max_results, &config.tavily_api_keys).await
        }
        SearchProvider::Firecrawl => {
            search_firecrawl(client, query, max_results, &config.firecrawl_api_keys).await
        }
        SearchProvider::AnySearch => {
            search_anysearch(client, query, max_results, &config.anysearch_api_keys).await
        }
        SearchProvider::Exa => search_exa(client, query, max_results, &config.exa_api_keys).await,
        SearchProvider::SearXng => {
            search_searxng(client, query, max_results, &config.searxng_base_url).await
        }
        SearchProvider::DuckDuckGo => search_duckduckgo(client, query, max_results).await,
    }
}
