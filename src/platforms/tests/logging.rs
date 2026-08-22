//! 日志截断与脱敏。

use crate::platforms::*;
use super::shared::*;

#[test]
fn platform_tool_payload_pretty_prints_small_json() {
    assert_eq!(
        format_platform_tool_payload_for(r#"{"query":"Miyu","limit":2}"#, Locale::Zh),
        "{\n  \"limit\": 2,\n  \"query\": \"Miyu\"\n}"
    );
}

#[test]
fn platform_tool_payload_truncates_on_unicode_boundaries() {
    let payload = "喵".repeat(PLATFORM_TOOL_LOG_MAX_CHARS + 1);
    let formatted = format_platform_tool_payload_for(&payload, Locale::Zh);
    let (kept, notice) = formatted.split_once('\n').unwrap();

    assert_eq!(kept.chars().count(), PLATFORM_TOOL_LOG_MAX_CHARS);
    assert!(kept.chars().all(|character| character == '喵'));
    assert_eq!(notice, "... 已截断 1 字符 ...");
}

#[test]
fn platform_reply_log_truncates_on_unicode_boundaries() {
    let payload = "喵".repeat(PLATFORM_REPLY_LOG_MAX_CHARS + 7);
    let formatted = truncate_platform_reply_log_for(&payload, Locale::Zh);
    let kept = formatted.lines().next().unwrap();

    assert_eq!(kept.chars().count(), PLATFORM_REPLY_LOG_MAX_CHARS);
    assert!(formatted.ends_with("... 已截断 7 字符 ..."));
    assert_eq!(
        truncate_platform_reply_log_for("safe\u{1b}[31m", Locale::Zh),
        "safe\\u{1b}[31m"
    );
}

#[test]
fn platform_tool_logs_include_correlation_and_result_details() {
    let started = format_platform_tool_started_log_for(
        "run_123",
        &serde_json::json!({
            "tool_id": "run_123_tool_2",
            "name": "web_search",
            "display_name": "网页搜索",
            "arguments": "{\"query\":\"Miyu\"}"
        }),
        Locale::Zh,
    );
    assert!(started.starts_with("【工具：web_search】\n运行：run_123"));
    assert!(started.contains("调用 ID：run_123_tool_2"));
    assert!(started.contains("显示名称：网页搜索"));
    assert!(started.contains("\"query\": \"Miyu\""));

    let finished = format_platform_tool_finished_log_for(
        "run_123",
        &serde_json::json!({
            "tool_id": "run_123_tool_2",
            "name": "web_search",
            "display_name": "网页搜索",
            "ok": false,
            "output": "request timed out"
        }),
        Locale::Zh,
    );
    assert!(finished.starts_with("【工具结果：web_search】\n运行：run_123"));
    assert!(finished.contains("调用 ID：run_123_tool_2"));
    assert!(finished.contains("显示名称：网页搜索"));
    assert!(finished.contains("状态：失败"));
    assert!(finished.ends_with("结果：\nrequest timed out"));

    let english = format_platform_tool_finished_log_for(
        "run_123",
        &serde_json::json!({
            "tool_id": "run_123_tool_2",
            "name": "web_search",
            "ok": true,
            "output": "done"
        }),
        Locale::En,
    );
    assert!(english.starts_with("[Tool result: web_search]\nRun: run_123"));
    assert!(english.contains("Status: success"));

    let sanitized = format_platform_tool_finished_log_for(
        "run_123",
        &serde_json::json!({
            "tool_id": "run_123_tool_2",
            "name": "web_search\nforged",
            "ok": true,
            "output": "safe\u{1b}[31m"
        }),
        Locale::En,
    );
    assert!(sanitized.starts_with("[Tool result: web_search forged]"));
    assert!(sanitized.ends_with("Result:\nsafe\\u{1b}[31m"));
}

#[test]
fn platform_final_reply_log_is_bilingual() {
    let (_temp, context, _adapter) = test_turn_context(false);
    let outcome = TurnOutcome {
        run_id: "run_123".to_string(),
        text: "hello".to_string(),
        provider_id: Some("provider".to_string()),
        model: Some("model".to_string()),
        image_assets: Vec::new(),
        suppressed_reply_ranges: Vec::new(),
        final_reply_already_sent: false,
    };

    let chinese =
        format_platform_final_reply_log_for(&outcome, &context, "你好", 0, Locale::Zh);
    assert!(chinese.starts_with("【AI 最终回复】\n运行：run_123"));
    assert!(chinese.contains("模型：provider / model"));

    let english =
        format_platform_final_reply_log_for(&outcome, &context, "hello", 0, Locale::En);
    assert!(english.starts_with("[AI final reply]\nRun: run_123"));
    assert!(english.contains("Model: provider / model"));
}
