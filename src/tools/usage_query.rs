//! token 用量查询工具:读 usage-history.jsonl 聚合出中文摘要。
//! 智能体主工具集(终端/WebUI/shell hook)与 QQ 平台工具集共用这套
//! 实现,两边只是 usage-history 路径的来源不同。

use super::{ToolRegistry, ToolSpec};
use crate::state::usage;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

/// 描述与 schema 在智能体侧与平台侧(src/platforms/tool.rs)两处注册共用,
/// 收敛成一份防止漂移。
pub(crate) const DESCRIPTION: &str = "Query Miyu's token usage statistics: totals, request count, cache hit rate, and the per-source (agent / messaging platforms) model breakdown. range: 1d (rolling 24h, default) / 7d / 30d / all.";

pub(crate) fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "range": {
                "type": "string",
                "enum": ["1d", "7d", "30d", "all"],
                "description": "Time range, defaults to 1d (rolling 24h)."
            }
        },
        "additionalProperties": false
    })
}

pub fn register(
    registry: &mut ToolRegistry,
    history_file: PathBuf,
    config: crate::config::AppConfig,
) {
    registry.register(
        ToolSpec::new(
            "query_token_usage",
            DESCRIPTION,
            parameters(),
            move |arguments| {
                let history_file = history_file.clone();
                let config = config.clone();
                async move { query(arguments, history_file, config).await }
            },
        )
        .with_display_name("Token usage"),
    );
}

async fn query(
    arguments: Value,
    history_file: PathBuf,
    config: crate::config::AppConfig,
) -> Result<String> {
    let range_key = arguments
        .get("range")
        .and_then(Value::as_str)
        .unwrap_or("1d")
        .to_string();
    let range = crate::state::UsageRange::parse(&range_key);
    let stats = tokio::task::spawn_blocking(move || {
        let price = crate::models_cache::pricing_resolver(&config);
        usage::usage_stats(&history_file, range, &price)
    })
    .await
    .context("usage stats task panicked")??;
    Ok(format_usage_summary(&stats, &range_key))
}

/// Markdown 输出(08-26):平台长文会转图渲染,QQ 与 WebUI 都按 markdown
/// 显示——原来的 `▸` 自造符号既不是 markdown 也不好读。细项(主动回复
/// 判断等)挂在所属来源下面,零记录不出现。
pub(crate) fn format_usage_summary(stats: &crate::state::UsageStats, range_key: &str) -> String {
    let label = match range_key {
        "1d" | "24h" | "today" => "近一天",
        "7d" => "近 7 天",
        "30d" => "近 30 天",
        _ => "至今",
    };
    if stats.totals.requests == 0 {
        return format!("**Token 消耗 · {label}**\n\n{label}没有任何 LLM 调用记录。");
    }
    let fmt = format_tokens;
    let hit = |cache_read: u64, prompt: u64| {
        (prompt > 0).then(|| (cache_read as f64 / prompt as f64 * 100.0).round())
    };
    let total_hit = hit(stats.totals.cache_read, stats.totals.prompt).unwrap_or(0.0);
    let mut lines = vec![
        format!("**Token 消耗 · {label}**"),
        String::new(),
        format!(
            "- 总消耗 **{}**(输入 {} · 输出 {})",
            fmt(stats.totals.total),
            fmt(stats.totals.prompt),
            fmt(stats.totals.completion)
        ),
        format!(
            "- 请求 **{}** 次 · 缓存命中率 **{total_hit:.0}%**",
            stats.totals.requests
        ),
    ];
    // 金额估算不进工具输出(用户 08-20 裁定:models.dev 价目对不齐实际计费,
    // 数字不准还容易被模型当真话复述)。WebUI 控制台的统计图表照旧。
    for source in &stats.sources {
        let name = usage_source_name(&source.src);
        let source_hit = hit(source.aggregate.cache_read, source.aggregate.prompt)
            .map(|value| format!(" · 命中 {value:.0}%"))
            .unwrap_or_default();
        lines.push(String::new());
        lines.push(format!(
            "**{name}** · {} 次 · {}{source_hit}",
            source.aggregate.requests,
            fmt(source.aggregate.total)
        ));
        let mut parts = Vec::new();
        for model in source.models.iter().take(3) {
            let share = if source.aggregate.total > 0 {
                (model.aggregate.total as f64 / source.aggregate.total as f64 * 100.0).round()
            } else {
                0.0
            };
            let display = if model.model.is_empty() {
                "(未标模型)"
            } else {
                model.model.as_str()
            };
            parts.push(format!("{display} {share:.0}%"));
        }
        if !parts.is_empty() {
            lines.push(format!("- 模型构成:{}", parts.join(" · ")));
        }
        for kind in &source.kinds {
            let share = if source.aggregate.total > 0 {
                (kind.aggregate.total as f64 / source.aggregate.total as f64 * 100.0).round()
            } else {
                0.0
            };
            lines.push(format!(
                "- 其中 {} {} 次 · {} · 占本来源 {share:.0}%",
                usage_kind_name(&kind.kind),
                kind.aggregate.requests,
                fmt(kind.aggregate.total)
            ));
        }
    }
    lines.join("\n")
}

pub(crate) fn usage_source_name(src: &str) -> String {
    match src {
        "agent" => "智能体".to_string(),
        "qq" | "onebot" => "QQ".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn usage_kind_name(kind: &str) -> String {
    match kind {
        crate::state::USAGE_KIND_JUDGE => "主动回复判断".to_string(),
        crate::state::USAGE_KIND_AFFECTION => "好感度更新".to_string(),
        crate::state::USAGE_KIND_GROUP_JOIN => "入群审批".to_string(),
        other => other.to_string(),
    }
}

fn format_tokens(value: u64) -> String {
    // 与 WebUI 的 usageFmt 同档:全量范围下总量早就过十亿,只有 M 会印出
    // "1234.56M"(08-26 用户点名)。
    if value >= 1_000_000_000 {
        format!("{:.2}B", value as f64 / 1_000_000_000.0)
    } else if value >= 1_000_000 {
        format!("{:.2}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Usage;

    #[tokio::test]
    async fn agent_registry_tool_reports_usage() {
        let temp = tempfile::tempdir().unwrap();
        let history = temp.path().join("usage-history.jsonl");
        usage::record_usage(
            &history,
            &Usage {
                prompt_tokens: 2000,
                completion_tokens: 300,
                total_tokens: 2300,
                cache_read_tokens: 900,
                ..Usage::default()
            },
            usage::UsageMeta {
                source: "agent",
                provider: Some("prov"),
                model: Some("m-x"),
                kind: None,
            },
            false,
        )
        .unwrap();
        let mut registry = ToolRegistry::new();
        register(&mut registry, history, crate::config::AppConfig::default());
        let output = registry
            .call("query_token_usage", r#"{"range":"1d"}"#)
            .await
            .unwrap();
        assert!(output.contains("**Token 消耗 · 近一天**"), "{output}");
        assert!(output.contains("**智能体**"), "{output}");
        assert!(output.contains("- 模型构成:m-x"), "{output}");
        assert!(output.contains("缓存命中率 **45%**"), "{output}");
        // 没有细项标签的来源不长出"其中"行。
        assert!(!output.contains("其中"), "{output}");
    }

    /// 主动回复判断作为来源下的细项(08-26):有记录才出现,数字不重复计进
    /// 来源合计之外。退回 kind 聚合前,这条断言拿不到"其中 主动回复判断"。
    #[tokio::test]
    async fn judge_usage_renders_as_a_platform_sub_item() {
        let temp = tempfile::tempdir().unwrap();
        let history = temp.path().join("usage-history.jsonl");
        let record = |kind: Option<&str>, prompt: u64| {
            usage::record_usage(
                &history,
                &Usage {
                    prompt_tokens: prompt,
                    completion_tokens: 100,
                    total_tokens: prompt + 100,
                    ..Usage::default()
                },
                usage::UsageMeta {
                    source: "onebot",
                    provider: Some("prov"),
                    model: Some("m-q"),
                    kind,
                },
                kind.is_some(),
            )
            .unwrap();
        };
        record(None, 9_000);
        record(Some(crate::state::USAGE_KIND_JUDGE), 1_000);
        let mut registry = ToolRegistry::new();
        register(&mut registry, history, crate::config::AppConfig::default());
        let output = registry
            .call("query_token_usage", r#"{"range":"1d"}"#)
            .await
            .unwrap();
        assert!(output.contains("**QQ** · 2 次"), "{output}");
        assert!(output.contains("- 其中 主动回复判断 1 次"), "{output}");
        // 合计仍是两条之和,细项不额外加总。
        assert!(output.contains("总消耗 **10.2k**"), "{output}");
    }
}
