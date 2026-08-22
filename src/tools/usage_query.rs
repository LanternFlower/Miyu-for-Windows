//! token 用量查询工具:读 usage-history.jsonl 聚合出中文摘要。
//! 智能体主工具集(终端/WebUI/shell hook)与 QQ 平台工具集共用这套
//! 实现,两边只是 usage-history 路径的来源不同。

use super::{ToolRegistry, ToolSpec};
use crate::state::usage;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn register(registry: &mut ToolRegistry, history_file: PathBuf, config: crate::config::AppConfig) {
    registry.register(
        ToolSpec::new(
            "query_token_usage",
            "Query Miyu's token usage statistics: totals, request count, cache hit rate, and the per-source (agent / messaging platforms) model breakdown. range: 1d (rolling 24h, default) / 7d / 30d / all.",
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
            }),
            move |arguments| {
                let history_file = history_file.clone();
                let config = config.clone();
                async move { query(arguments, history_file, config).await }
            },
        )
        .with_display_name("Token usage"),
    );
}

async fn query(arguments: Value, history_file: PathBuf, config: crate::config::AppConfig) -> Result<String> {
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

pub(crate) fn format_usage_summary(stats: &crate::state::UsageStats, range_key: &str) -> String {
    let label = match range_key {
        "1d" | "24h" | "today" => "近一天",
        "7d" => "近 7 天",
        "30d" => "近 30 天",
        _ => "至今",
    };
    if stats.totals.requests == 0 {
        return format!("{label}没有任何 LLM 调用记录。");
    }
    let fmt = format_tokens;
    let hit = if stats.totals.prompt > 0 {
        (stats.totals.cache_read as f64 / stats.totals.prompt as f64 * 100.0).round()
    } else {
        0.0
    };
    let mut lines = vec![
        format!("📊 Token 消耗 · {label}"),
        format!(
            "总消耗 {}(输入 {} · 输出 {})",
            fmt(stats.totals.total),
            fmt(stats.totals.prompt),
            fmt(stats.totals.completion)
        ),
        format!("请求 {} 次 · 缓存命中率 {hit:.0}%", stats.totals.requests),
    ];
    // 金额估算不进工具输出(用户 08-20 裁定:models.dev 价目对不齐实际计费,
    // 数字不准还容易被模型当真话复述)。WebUI 控制台的统计图表照旧。
    for source in &stats.sources {
        let name = match source.src.as_str() {
            "agent" => "智能体".to_string(),
            "qq" | "onebot" => "QQ".to_string(),
            other => other.to_string(),
        };
        let source_hit = if source.aggregate.prompt > 0 {
            format!(
                " · 命中 {:.0}%",
                source.aggregate.cache_read as f64 / source.aggregate.prompt as f64 * 100.0
            )
        } else {
            String::new()
        };
        lines.push(format!(
            "▸ {name} · {} 次 · {}{source_hit}",
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
            let display = if model.model.is_empty() { "(未标模型)" } else { model.model.as_str() };
            parts.push(format!("{display} {share:.0}%"));
        }
        if !parts.is_empty() {
            lines.push(format!("   {}", parts.join(" · ")));
        }
    }
    lines.join("\n")
}

fn format_tokens(value: u64) -> String {
    if value >= 1_000_000 {
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
            usage::UsageMeta { source: "agent", provider: Some("prov"), model: Some("m-x") },
            false,
        )
        .unwrap();
        let mut registry = ToolRegistry::new();
        register(&mut registry, history, crate::config::AppConfig::default());
        let output = registry
            .call("query_token_usage", r#"{"range":"1d"}"#)
            .await
            .unwrap();
        assert!(output.contains("Token 消耗"), "{output}");
        assert!(output.contains("智能体"), "{output}");
        assert!(output.contains("m-x"), "{output}");
        assert!(output.contains("命中率 45%"), "{output}");
    }
}
