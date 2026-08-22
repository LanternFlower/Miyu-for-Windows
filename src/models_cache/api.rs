//! models.dev 接口的响应结构与解析。
//!
//! 纯 serde + 一层解析。上游字段会变、会缺，所以几乎全是 `Option`，解析失败的
//! 单个模型跳过而不是整批作废——一个模型的元数据坏了不该让所有价格都查不到。

use crate::models_cache::*;

pub(crate) const API_URL: &str = "https://models.dev/api.json";

#[derive(Debug, Deserialize)]
pub(crate) struct ApiResponse(HashMap<String, ApiProvider>);

#[derive(Debug, Deserialize)]
pub(crate) struct ApiProvider {
    #[serde(default)]
    pub(crate) models: HashMap<String, ApiModel>,
    #[serde(default)]
    pub(crate) npm: Option<String>,
    /// 该供应商的 API base URL,用来把 Miyu 配置里的自定义供应商
    /// (id 不一定与 models.dev 键一致,如 opencodego vs opencode-go)
    /// 对到目录条目上,计费估算靠它。
    #[serde(default)]
    pub(crate) api: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiModel {
    #[serde(default)]
    pub(crate) modalities: Option<ApiModalities>,
    #[serde(default)]
    pub(crate) limit: Option<ApiLimit>,
    #[serde(default)]
    pub(crate) reasoning_options: Vec<ApiReasoningOption>,
    #[serde(default)]
    pub(crate) provider: Option<ApiModelProvider>,
    #[serde(default)]
    pub(crate) cost: Option<ApiCost>,
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
pub(crate) struct ApiModalities {
    #[serde(default)]
    pub(crate) input: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiLimit {
    #[serde(default)]
    pub(crate) context: Option<u64>,
    #[serde(default)]
    pub(crate) input: Option<u64>,
    #[serde(default)]
    pub(crate) output: Option<u64>,
}

impl ApiLimit {
    /// The window Miyu may actually fill. Some catalogue entries advertise a
    /// total `context` larger than the `input` the provider will accept —
    /// opencode's big-pickle reports 200k context against a 160k input cap —
    /// and budgeting against the larger number puts compaction 20k of tokens
    /// too late, so the request overflows before it is ever compacted.
    pub(crate) fn usable_context(&self) -> Option<u64> {
        match (self.context, self.input.filter(|input| *input > 0)) {
            (Some(context), Some(input)) => Some(context.min(input)),
            (context, input) => context.or(input),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ApiReasoningOption {
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
pub(crate) struct ApiModelProvider {
    #[serde(default)]
    pub(crate) npm: Option<String>,
}

pub(in crate::models_cache) fn parse_api_response(text: &str) -> Result<Cache> {
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

pub(crate) fn reasoning_variants(
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

pub(crate) fn push_variant(variants: &mut Vec<ReasoningVariant>, id: String, setting: ReasoningSetting) {
    if variants.iter().any(|variant| variant.id == id) {
        return;
    }
    variants.push(ReasoningVariant { id, setting });
}
