//! 平台侧日志的截断与脱敏。
//!
//! 工具参数和回复都可能很长，也可能带敏感内容，而平台日志是长期留存的。所以
//! 每一路都成对提供 `*_for` 版本——上限作为参数传进来，才能对着固定长度写
//! 测试。

use crate::platforms::*;

pub(crate) const PLATFORM_TOOL_LOG_MAX_CHARS: usize = 2_400;

pub(crate) const PLATFORM_REPLY_LOG_MAX_CHARS: usize = 1_200;

pub(crate) fn format_platform_tool_payload(payload: &str) -> String {
    format_platform_tool_payload_for(payload, crate::i18n::locale())
}

pub(crate) fn format_platform_tool_payload_for(payload: &str, locale: Locale) -> String {
    let sanitized = sanitize_platform_log_text(payload.trim());
    let text = sanitized.as_str();
    if text.chars().count() > PLATFORM_TOOL_LOG_MAX_CHARS {
        return truncate_platform_tool_log(text, locale);
    }
    let formatted = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.to_string());
    truncate_platform_tool_log(&formatted, locale)
}

pub(crate) fn truncate_platform_tool_log(text: &str, locale: Locale) -> String {
    truncate_platform_log(text, PLATFORM_TOOL_LOG_MAX_CHARS, locale)
}

pub(crate) fn truncate_platform_reply_log(text: &str) -> String {
    truncate_platform_reply_log_for(text, crate::i18n::locale())
}

pub(crate) fn truncate_platform_reply_log_for(text: &str, locale: Locale) -> String {
    sanitize_platform_log_text(&truncate_platform_log(
        text,
        PLATFORM_REPLY_LOG_MAX_CHARS,
        locale,
    ))
}

pub(crate) fn sanitize_platform_log_text(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\n' | '\t' => sanitized.push(character),
            character if character.is_control() => sanitized.extend(character.escape_default()),
            character => sanitized.push(character),
        }
    }
    sanitized
}

pub(crate) fn truncate_platform_log(text: &str, max_chars: usize, locale: Locale) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let omitted = total - max_chars;
    format!(
        "{}\n{}",
        text.chars().take(max_chars).collect::<String>(),
        if locale == Locale::Zh {
            format!("... 已截断 {omitted} 字符 ...")
        } else {
            format!("... truncated {omitted} characters ...")
        }
    )
}

pub(crate) fn format_platform_final_reply_log(
    outcome: &TurnOutcome,
    context: &PlatformTurnContext,
    reply_text: &str,
    image_count: usize,
) -> String {
    format_platform_final_reply_log_for(
        outcome,
        context,
        reply_text,
        image_count,
        crate::i18n::locale(),
    )
}

pub(crate) fn format_platform_final_reply_log_for(
    outcome: &TurnOutcome,
    context: &PlatformTurnContext,
    reply_text: &str,
    image_count: usize,
    locale: Locale,
) -> String {
    let endpoint = match (
        outcome
            .provider_id
            .as_deref()
            .filter(|value| !value.is_empty()),
        outcome.model.as_deref().filter(|value| !value.is_empty()),
    ) {
        (Some(provider), Some(model)) => format!("{provider} / {model}"),
        (Some(provider), None) => provider.to_string(),
        (None, Some(model)) => model.to_string(),
        (None, None) => text_for(locale, "unknown", "未知").to_string(),
    };
    let endpoint = sanitize_platform_log_text(&endpoint);
    let body = if reply_text.trim().is_empty() {
        if outcome.final_reply_already_sent {
            text_for(
                locale,
                "[reply was sent directly by a tool]",
                "[回复已由工具直接发送]",
            )
            .to_string()
        } else if image_count > 0 {
            if locale == Locale::Zh {
                format!("[无文本，发送 {image_count} 张图片]")
            } else {
                format!("[no text; sent {image_count} images]")
            }
        } else {
            text_for(locale, "[empty reply]", "[空回复]").to_string()
        }
    } else {
        truncate_platform_reply_log_for(reply_text.trim(), locale)
    };
    let conversation_kind = match (locale, context.conversation.kind) {
        (Locale::Zh, ConversationKind::Group) => "群聊",
        (Locale::Zh, ConversationKind::Private) => "私聊",
        (_, kind) => kind.as_str(),
    };
    if locale == Locale::Zh {
        format!(
            "【AI 最终回复】\n运行：{}\n会话：{} {}（机器人账号 {}）\n模型：{}\n内容：\n{}",
            outcome.run_id,
            conversation_kind,
            context.conversation.conversation_id,
            context.conversation.account_id,
            endpoint,
            body
        )
    } else {
        format!(
            "[AI final reply]\nRun: {}\nConversation: {} {} (bot account {})\nModel: {}\nContent:\n{}",
            outcome.run_id,
            conversation_kind,
            context.conversation.conversation_id,
            context.conversation.account_id,
            endpoint,
            body
        )
    }
}

pub(crate) fn format_platform_tool_name(name: &str, display_name: Option<&str>) -> String {
    display_name
        .filter(|display_name| *display_name != name)
        .map(sanitize_platform_tool_label)
        .unwrap_or_else(|| sanitize_platform_tool_label(name))
}

pub(crate) fn sanitize_platform_tool_label(value: &str) -> String {
    let compact = sanitize_platform_log_text(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if compact.is_empty() {
        "unknown".to_string()
    } else {
        compact.chars().take(128).collect()
    }
}

pub(crate) fn format_platform_tool_started_log(run_id: &str, data: &Value) -> String {
    format_platform_tool_started_log_for(run_id, data, crate::i18n::locale())
}

pub(crate) fn format_platform_tool_started_log_for(run_id: &str, data: &Value, locale: Locale) -> String {
    let tool_id = data
        .get("tool_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| text_for(locale, "unknown", "未知"));
    let name = data
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_else(|| text_for(locale, "unknown", "未知"));
    let display_name = data.get("display_name").and_then(Value::as_str);
    let arguments = data
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_name = sanitize_platform_tool_label(name);
    let display_name = format_platform_tool_name(name, display_name);
    let arguments = format_platform_tool_payload_for(arguments, locale);
    if locale == Locale::Zh {
        let mut lines = vec![
            format!("【工具：{tool_name}】"),
            format!("运行：{run_id}"),
            format!("调用 ID：{tool_id}"),
        ];
        if display_name != tool_name {
            lines.push(format!("显示名称：{display_name}"));
        }
        lines.push(format!("参数：\n{arguments}"));
        lines.join("\n")
    } else {
        let mut lines = vec![
            format!("[Tool: {tool_name}]"),
            format!("Run: {run_id}"),
            format!("Call ID: {tool_id}"),
        ];
        if display_name != tool_name {
            lines.push(format!("Display name: {display_name}"));
        }
        lines.push(format!("Arguments:\n{arguments}"));
        lines.join("\n")
    }
}

pub(crate) fn format_platform_tool_finished_log(run_id: &str, data: &Value) -> String {
    format_platform_tool_finished_log_for(run_id, data, crate::i18n::locale())
}

pub(crate) fn format_platform_tool_finished_log_for(run_id: &str, data: &Value, locale: Locale) -> String {
    let tool_id = data
        .get("tool_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| text_for(locale, "unknown", "未知"));
    let name = data
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_else(|| text_for(locale, "unknown", "未知"));
    let display_name = data.get("display_name").and_then(Value::as_str);
    let ok = data.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let output = data
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_name = sanitize_platform_tool_label(name);
    let display_name = format_platform_tool_name(name, display_name);
    let output = format_platform_tool_payload_for(output, locale);
    if locale == Locale::Zh {
        let mut lines = vec![
            format!("【工具结果：{tool_name}】"),
            format!("运行：{run_id}"),
            format!("调用 ID：{tool_id}"),
        ];
        if display_name != tool_name {
            lines.push(format!("显示名称：{display_name}"));
        }
        lines.push(format!("状态：{}", if ok { "成功" } else { "失败" }));
        lines.push(format!("结果：\n{output}"));
        lines.join("\n")
    } else {
        let mut lines = vec![
            format!("[Tool result: {tool_name}]"),
            format!("Run: {run_id}"),
            format!("Call ID: {tool_id}"),
        ];
        if display_name != tool_name {
            lines.push(format!("Display name: {display_name}"));
        }
        lines.push(format!("Status: {}", if ok { "success" } else { "failed" }));
        lines.push(format!("Result:\n{output}"));
        lines.join("\n")
    }
}
