//! 思考者与审阅者的提示词，以及统计。
//!
//! 两个角色轮流跑：思考者产出，审阅者挑毛病并决定要不要再来一轮。
//! `ResearchStats` 汇总多轮的 token 用量——深度研究是最贵的工具，用量要看得见。

use crate::tools::deep_research::*;

pub(in crate::tools::deep_research) const THINKER_SYSTEM_PROMPT: &str = r#"你是深度研究系统中的“沉思者”。
你的任务是理解用户命题，主动调用可用工具查证，形成可发送给用户的 Markdown 草稿。

工作原则：
1. 优先基于题面和本地资料；需要时使用 web_search 和 web_fetch 联网查证。
2. 关键事实、技术判断、推荐理由和核心观点应有来源或依据。
3. 需要引用资料时，先调用 register_deep_research_reference 注册参考资料，再在正文中使用返回的 [R数字]/[K数字]/[W数字]。
4. 第一轮必须调用 register_deep_research_topic_title 注册 4-40 字短标题。
5. 不编造来源；资料冲突时说明冲突和取舍；无法查证的点写入“不确定点”。
6. 输出可直接发送给用户的 Markdown 正文，不输出内部 JSON，不输出“参考资料”章节。
7. 不使用 emoji 或装饰性图标。
"#;

pub(in crate::tools::deep_research) const REVIEWER_SYSTEM_PROMPT: &str = r#"你是深度研究系统中的“审视者”。
你只审查沉思者草稿，不替用户回答。请严格输出 JSON。

审查重点：
1. 是否覆盖用户问题的关键对象、维度、限制和输出要求。
2. 关键事实和观点是否有已注册 R/K/W 引用支撑。
3. 是否存在严重逻辑错误、前后矛盾、结论超出证据。
4. 是否存在影响结论的数据缺口，却没有说明查证失败或列入不确定点。

输出格式：
{
  "accepted": true/false,
  "challenge": "主要质疑或通过理由",
  "revision_instructions": ["需要修正的事项"]
}
"#;

#[derive(Default)]
pub(in crate::tools::deep_research) struct ResearchStats {
    pub(crate) tool_calls: usize,
    pub(crate) tool_ok: usize,
    pub(crate) tool_errors: usize,
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
    pub(crate) total_tokens: u64,
    pub(crate) cache_read_tokens: u64,
    pub(crate) token_estimate: u64,
    pub(crate) token_estimate_method: TokenEstimateMethod,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(in crate::tools::deep_research) enum TokenEstimateMethod {
    #[default]
    None,
    ProviderUsage,
    ProviderUsagePlusEstimate,
    RoughCharEstimate,
}

impl ResearchStats {
    pub(crate) fn add_usage_or_estimate(&mut self, usage: Option<&Usage>, texts: &[&str]) {
        if let Some(usage) = usage {
            let total_tokens = usage.effective_total_tokens();
            if total_tokens > 0 {
                self.prompt_tokens += usage.prompt_tokens;
                self.completion_tokens += usage.completion_tokens;
                self.cache_read_tokens += usage.cache_read_tokens;
                self.total_tokens += total_tokens;
                self.token_estimate += total_tokens;
                self.token_estimate_method = match self.token_estimate_method {
                    TokenEstimateMethod::None | TokenEstimateMethod::ProviderUsage => {
                        TokenEstimateMethod::ProviderUsage
                    }
                    _ => TokenEstimateMethod::ProviderUsagePlusEstimate,
                };
                return;
            }
        }
        let estimate = estimate_tokens(texts);
        self.token_estimate += estimate;
        self.token_estimate_method = match self.token_estimate_method {
            TokenEstimateMethod::None | TokenEstimateMethod::RoughCharEstimate => {
                TokenEstimateMethod::RoughCharEstimate
            }
            _ => TokenEstimateMethod::ProviderUsagePlusEstimate,
        };
    }
}

pub(in crate::tools::deep_research) fn merge_stats(
    state: &Arc<Mutex<ResearchState>>,
    sa_stats: &crate::tools::subagent_runner::SubagentStats,
) {
    use crate::tools::subagent_runner::TokenEstimateMethod as SaTEM;
    let mut state = state.lock().expect("deep research state lock");
    state.stats.tool_calls += sa_stats.tool_calls;
    state.stats.tool_ok += sa_stats.tool_ok;
    state.stats.tool_errors += sa_stats.tool_errors;
    state.stats.prompt_tokens += sa_stats.prompt_tokens;
    state.stats.completion_tokens += sa_stats.completion_tokens;
    state.stats.cache_read_tokens += sa_stats.cache_read_tokens;
    state.stats.total_tokens += sa_stats.total_tokens;
    state.stats.token_estimate += sa_stats.token_estimate;
    let has_provider = state.stats.token_estimate_method == TokenEstimateMethod::ProviderUsage
        || sa_stats.token_estimate_method == SaTEM::ProviderUsage;
    let has_estimate = state.stats.token_estimate_method == TokenEstimateMethod::RoughCharEstimate
        || state.stats.token_estimate_method == TokenEstimateMethod::None
        || sa_stats.token_estimate_method == SaTEM::RoughCharEstimate
        || sa_stats.token_estimate_method == SaTEM::None;
    state.stats.token_estimate_method = if has_provider && !has_estimate {
        TokenEstimateMethod::ProviderUsage
    } else if has_provider {
        TokenEstimateMethod::ProviderUsagePlusEstimate
    } else {
        TokenEstimateMethod::RoughCharEstimate
    };
}

pub(in crate::tools::deep_research) fn thinker_prompt(
    topic: &str,
    iteration: usize,
    draft: &str,
    review: &Value,
    state: &Arc<Mutex<ResearchState>>,
) -> Result<String> {
    Ok(format!(
        "请完成第 {iteration} 轮深度研究。\n\n用户命题：\n{topic}\n\n上一轮草稿：\n{}\n\n上一轮审视意见：\n{}\n\n当前参考资料注册表：\n{}\n\n要求：结论先行，必要时调用工具查证；需要引用时先注册参考资料，并在正文中使用 [R1]/[K1]/[W1] 标注。不要输出参考资料章节。",
        if draft.trim().is_empty() { "（无）" } else { draft },
        serde_json::to_string_pretty(review)?,
        reference_registry_json(state)?,
    ))
}

pub(in crate::tools::deep_research) fn reviewer_prompt(
    topic: &str,
    iteration: usize,
    draft: &str,
    state: &Arc<Mutex<ResearchState>>,
) -> Result<String> {
    Ok(format!(
        "请审查第 {iteration} 轮草案。\n\n用户命题：\n{topic}\n\n草案：\n{draft}\n\n参考资料注册表：\n{}\n\n若可以发送，accepted=true；否则列出具体 revision_instructions。",
        reference_registry_json(state)?,
    ))
}

pub(in crate::tools::deep_research) fn parse_review(content: &str) -> Value {
    parse_json_object(content).unwrap_or_else(|| {
        json!({"accepted": true, "challenge": "reviewer returned non-JSON feedback; accept current draft to avoid repeated research", "revision_instructions": [], "review_text": content.trim()})
    })
}

pub(in crate::tools::deep_research) fn parse_json_object(content: &str) -> Option<Value> {
    let trimmed = content.trim();
    serde_json::from_str(trimmed).ok().or_else(|| {
        crate::json_extract::extract_json_object(trimmed)
            .and_then(|json| serde_json::from_str(json).ok())
    })
}
