//! 端点调度：轮换、冷却与故障转移。
//!
//! 一个供应商可以配多个端点。`LlmScheduler` 决定这一次用哪个，并在失败后按
//! 失败类型给冷却时间——429 冷却久一点，连接失败短一点，参数错误根本不换端点
//! （换了还是错，只是白烧三次配额）。
//!
//! `endpoint_failover_allowed` 有个硬约束：**已经吐给用户的内容不能回退**。流
//! 式输出一旦提交就不能换端点重来，否则用户会看到同一段话说两遍。

use crate::llm::openai_compatible::*;

/// Responses 续传健康位(任务#16 自愈)。跨 clone 共享:压缩器等辅助克隆
/// 与主客户端看到同一份;置位即进程内立即生效,并持久化到
/// provider-capabilities.json 供后续会话读取。多供应商混池时按主
/// provider 记录(续传本就钉在单端点上,混池仅有过度抑制的轻微风险)。
#[derive(Clone)]
pub(in crate::llm::openai_compatible) struct ResponsesContinuationHealth {
    pub(in crate::llm::openai_compatible) unsupported: Arc<std::sync::atomic::AtomicBool>,
    pub(in crate::llm::openai_compatible) store: std::path::PathBuf,
    pub(in crate::llm::openai_compatible) base_url: String,
    pub(in crate::llm::openai_compatible) provider_id: String,
}

impl ResponsesContinuationHealth {
    pub(in crate::llm::openai_compatible) fn for_provider(paths: &MiyuPaths, provider: &ProviderConfig) -> Self {
        let store = crate::llm::provider_capabilities::store_path(&paths.cache_dir);
        let unsupported = crate::llm::provider_capabilities::continuation_unsupported(
            &store,
            &provider.base_url,
        );
        Self {
            unsupported: Arc::new(std::sync::atomic::AtomicBool::new(unsupported)),
            store,
            base_url: provider.base_url.clone(),
            provider_id: provider.id.clone(),
        }
    }

    /// 测试用:无持久化、乐观放行。
    pub(in crate::llm::openai_compatible) fn detached() -> Self {
        Self {
            unsupported: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            store: std::path::PathBuf::new(),
            base_url: String::new(),
            provider_id: String::new(),
        }
    }
}

#[derive(Clone)]
pub(in crate::llm::openai_compatible) struct LlmEndpoint {
    pub(in crate::llm::openai_compatible) client: Client,
    pub(in crate::llm::openai_compatible) provider: ProviderConfig,
    pub(in crate::llm::openai_compatible) api_key: String,
    pub(in crate::llm::openai_compatible) key_index: usize,
}

impl LlmEndpoint {
    pub(in crate::llm::openai_compatible) fn id(&self) -> String {
        endpoint_id(
            &self.provider.id,
            &self.provider.default_model,
            self.key_index,
        )
    }
}

#[derive(Default)]
pub(in crate::llm::openai_compatible) struct LlmScheduler {
    pub(in crate::llm::openai_compatible) cursor: usize,
    pub(in crate::llm::openai_compatible) cooldowns: HashMap<String, Instant>,
}

impl LlmScheduler {
    pub(in crate::llm::openai_compatible) fn ordered_indices(&mut self, endpoints: &[LlmEndpoint]) -> Vec<usize> {
        let available = endpoints
            .iter()
            .enumerate()
            .filter_map(|(index, endpoint)| self.is_ready(&endpoint.id()).then_some(index))
            .collect::<Vec<_>>();
        if available.is_empty() {
            return Vec::new();
        }
        let start = self.cursor % available.len();
        self.cursor = self.cursor.wrapping_add(1);
        rotate_from(available, start)
    }

    pub(in crate::llm::openai_compatible) fn is_ready(&mut self, id: &str) -> bool {
        match self.cooldowns.get(id).copied() {
            Some(until) if until > Instant::now() => false,
            Some(_) => {
                self.cooldowns.remove(id);
                true
            }
            None => true,
        }
    }

    /// The endpoint whose cooldown lifts first, for the single probe sent when
    /// every endpoint is cooling down. `None` sorts ahead of any deadline, so
    /// an endpoint with no cooldown recorded wins outright.
    pub(in crate::llm::openai_compatible) fn soonest_ready_index(&self, endpoints: &[LlmEndpoint]) -> Option<usize> {
        endpoints
            .iter()
            .enumerate()
            .min_by_key(|(_, endpoint)| self.cooldowns.get(&endpoint.id()).copied())
            .map(|(index, _)| index)
    }

    pub(in crate::llm::openai_compatible) fn mark_success(&mut self, id: &str) {
        self.cooldowns.remove(id);
    }

    pub(in crate::llm::openai_compatible) fn mark_failure(&mut self, id: String, duration: Duration) {
        self.cooldowns.insert(id, Instant::now() + duration);
    }
}

pub(in crate::llm::openai_compatible) fn rotate_from<T>(mut items: Vec<T>, start: usize) -> Vec<T> {
    items.rotate_left(start);
    items
}

pub(in crate::llm::openai_compatible) fn endpoint_id(provider_id: &str, model: &str, key_index: usize) -> String {
    format!("{provider_id}\t{model}\t{key_index}")
}

pub(in crate::llm::openai_compatible) fn ordered_endpoint_indices(endpoints: &[LlmEndpoint]) -> Vec<usize> {
    LLM_SCHEDULER
        .lock()
        .map(|mut scheduler| scheduler.ordered_indices(endpoints))
        .unwrap_or_else(|_| (0..endpoints.len()).collect())
}

pub(in crate::llm::openai_compatible) fn soonest_ready_endpoint_index(endpoints: &[LlmEndpoint]) -> Option<usize> {
    LLM_SCHEDULER
        .lock()
        .ok()
        .and_then(|scheduler| scheduler.soonest_ready_index(endpoints))
        .or_else(|| (!endpoints.is_empty()).then_some(0))
}

pub(in crate::llm::openai_compatible) fn mark_endpoint_success(endpoint: &LlmEndpoint) {
    if let Ok(mut scheduler) = LLM_SCHEDULER.lock() {
        scheduler.mark_success(&endpoint.id());
    }
}

pub(in crate::llm::openai_compatible) fn mark_endpoint_failure(endpoint: &LlmEndpoint, error: &anyhow::Error) -> Option<Duration> {
    let duration = cooldown_for_error(error)?;
    let mut scheduler = LLM_SCHEDULER.lock().ok()?;
    scheduler.mark_failure(endpoint.id(), duration);
    Some(duration)
}

pub(in crate::llm::openai_compatible) fn cooldown_for_status(status: u16) -> Option<Duration> {
    match status {
        401 | 403 | 429 => Some(Duration::from_secs(600)),
        408 | 500..=599 => Some(Duration::from_secs(120)),
        _ => None,
    }
}

pub(in crate::llm::openai_compatible) fn cooldown_for_error(error: &anyhow::Error) -> Option<Duration> {
    if let Some(failure) = error.downcast_ref::<HttpStatusFailure>() {
        return match failure.kind {
            HttpFailureKind::Authentication | HttpFailureKind::RateLimit => {
                Some(Duration::from_secs(600))
            }
            HttpFailureKind::EndpointUnavailable => Some(Duration::from_secs(120)),
            HttpFailureKind::EndpointIncompatible | HttpFailureKind::InvalidRequest => None,
            HttpFailureKind::Status => cooldown_for_status(failure.status),
        };
    }
    if error.downcast_ref::<TransportFailure>().is_some() {
        return Some(Duration::from_secs(120));
    }
    error
        .downcast_ref::<reqwest::Error>()
        .filter(|error| error.is_connect() || error.is_timeout())
        .map(|_| Duration::from_secs(120))
}

pub(in crate::llm::openai_compatible) fn endpoint_failover_allowed(error: &anyhow::Error) -> bool {
    !error
        .downcast_ref::<HttpStatusFailure>()
        .is_some_and(|failure| failure.kind == HttpFailureKind::InvalidRequest)
}

/// Whether the *same* endpoint may be tried again inside one request. A 429 or
/// a rejected key is a verdict on that provider/model/key, not a moment in
/// time: the retries `MIN_ENDPOINT_ATTEMPTS` pads in would fire back-to-back
/// with no backoff and spend more of a quota that already said no — which on a
/// shared free tier is what exhausted it. Failover to a *different* endpoint is
/// unaffected; that is `endpoint_failover_allowed`'s job.
pub(in crate::llm::openai_compatible) fn same_endpoint_retry_allowed(error: &anyhow::Error) -> bool {
    !error
        .downcast_ref::<HttpStatusFailure>()
        .is_some_and(|failure| {
            matches!(
                failure.kind,
                HttpFailureKind::Authentication | HttpFailureKind::RateLimit
            )
        })
}

pub(in crate::llm::openai_compatible) fn endpoint_client(provider: &ProviderConfig) -> Result<Client> {
    // Auxiliary callers (judge/affection/organizer) rebuild their client per
    // call; without this cache every judge run pays fresh TLS setup and loses
    // connection reuse. Keyed by every input the builder consumes, so a config
    // edit that changes the timeout naturally mints a new client; the map is
    // bounded by the number of distinct providers. `reqwest::Client` is an Arc
    // handle — clones share one pool.
    static CLIENTS: std::sync::OnceLock<std::sync::Mutex<HashMap<(String, u64), Client>>> =
        std::sync::OnceLock::new();
    let timeout = provider.timeout_seconds.clamp(5, 30);
    let key = (provider.id.clone(), timeout);
    let mut cache = CLIENTS
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    if let Some(client) = cache.get(&key) {
        return Ok(client.clone());
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(timeout))
        .build()
        .with_context(|| format!("building HTTP client for provider {}", provider.id))?;
    cache.insert(key, client.clone());
    Ok(client)
}

pub(in crate::llm::openai_compatible) fn llm_endpoints(config: &AppConfig, paths: &MiyuPaths) -> Result<Vec<LlmEndpoint>> {
    let mut endpoints = Vec::new();
    let mut errors = Vec::new();
    for choice in config.active_provider_model_choices() {
        let mut provider = config.provider(Some(&choice.provider_id))?.clone();
        if !provider.enabled {
            errors.push(format!(
                "{}: {}",
                provider.id,
                t(
                    "provider is disabled; enable it in the provider settings",
                    "供应商未启用;请在供应商设置里启用"
                )
            ));
            continue;
        }
        provider.default_model = choice.model;
        let client = endpoint_client(&provider)?;
        if provider_uses_claude_code(&provider) {
            // claude-code 走本机 CLI 的订阅登录态,没有 API key;单端点直进池。
            endpoints.push(LlmEndpoint {
                client: client.clone(),
                provider: provider.clone(),
                api_key: String::new(),
                key_index: 0,
            });
            continue;
        }
        match provider.resolved_api_keys(paths) {
            Ok(keys) => {
                for key in keys {
                    endpoints.push(LlmEndpoint {
                        client: client.clone(),
                        provider: provider.clone(),
                        api_key: key.value,
                        key_index: key.index,
                    });
                }
            }
            Err(err) => errors.push(format!(
                "{} / {}: {err}",
                provider.id, provider.default_model
            )),
        }
    }
    if endpoints.is_empty() {
        bail!(
            "no active provider/model endpoint is configured:\n- {}",
            errors.join("\n- ")
        )
    }
    Ok(endpoints)
}

pub(in crate::llm::openai_compatible) fn stream_chunk_commits_attempt(
    chunk: &ChatStreamChunk,
    reasoning_visibility: ReasoningVisibility,
) -> bool {
    (chunk.kind == ChatStreamKind::ReasoningPartEnd
        && reasoning_visibility != ReasoningVisibility::Hidden)
        || chunk.kind == ChatStreamKind::ToolCall
        || chunk.kind == ChatStreamKind::RemoteToolStarted
        || chunk.kind == ChatStreamKind::RemoteToolFinished
        || (chunk.kind == ChatStreamKind::Content && !chunk.text.is_empty())
        || (reasoning_visibility == ReasoningVisibility::Full
            && chunk.kind == ChatStreamKind::Reasoning
            && !chunk.text.is_empty())
}
