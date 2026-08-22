//! DSML：从纯文本里认出工具调用。
//!
//! 有些模型不支持原生 function calling，只能让它在正文里写一段标记，再由这里
//! 解析出来。
//!
//! 难点在流式：标记可能被切在任意位置，所以要能判断「当前尾巴是不是某个标记的
//! 前缀」（`partial_hidden_suffix_len`）——判早了会把半个标记吐给用户，判晚了
//! 会让正文卡住不显示。
//!
//! `strip_orphaned_dsml_tags` 兜底：模型写了开标记没写闭标记时，别把残骸留在
//! 用户看到的文本里。

use crate::llm::openai_compatible::*;

pub(in crate::llm::openai_compatible) fn dsml_enabled_for(provider: &ProviderConfig) -> bool {
    let base_url = provider.base_url.to_ascii_lowercase();
    let model = provider.default_model.to_ascii_lowercase();
    base_url.contains("taotoken.net") && model.starts_with("glm")
}

pub(in crate::llm::openai_compatible) const DSML_ANY_PREFIX: &str = "<｜｜DSML";

pub(in crate::llm::openai_compatible) const DSML_PREFIX: &str = "<｜｜DSML｜｜tool_calls";

pub(in crate::llm::openai_compatible) const DSML_END: &str = "</｜｜DSML｜｜tool_calls>";

pub(in crate::llm::openai_compatible) const SYSTEM_REMINDER_PREFIX: &str = "<system-reminder";

pub(in crate::llm::openai_compatible) const SYSTEM_REMINDER_UNDERSCORE_PREFIX: &str = "<system_reminder";

pub(in crate::llm::openai_compatible) fn hidden_start_after(target: &str, offset: usize) -> Option<usize> {
    [
        target[offset..].find(DSML_ANY_PREFIX),
        target[offset..].find(SYSTEM_REMINDER_PREFIX),
        target[offset..].find(SYSTEM_REMINDER_UNDERSCORE_PREFIX),
    ]
    .into_iter()
    .flatten()
    .map(|index| offset + index)
    .min()
}

pub(in crate::llm::openai_compatible) fn starts_hidden_prefix(value: &str) -> bool {
    DSML_ANY_PREFIX.starts_with(value)
        || SYSTEM_REMINDER_PREFIX.starts_with(value)
        || SYSTEM_REMINDER_UNDERSCORE_PREFIX.starts_with(value)
        || value.starts_with(DSML_ANY_PREFIX)
        || value.starts_with(SYSTEM_REMINDER_PREFIX)
        || value.starts_with(SYSTEM_REMINDER_UNDERSCORE_PREFIX)
}

pub(in crate::llm::openai_compatible) fn partial_hidden_suffix_len(value: &str) -> usize {
    let max_len = value.len().min(
        DSML_ANY_PREFIX
            .len()
            .max(SYSTEM_REMINDER_PREFIX.len())
            .max(SYSTEM_REMINDER_UNDERSCORE_PREFIX.len()),
    );
    for len in (1..=max_len).rev() {
        if !value.is_char_boundary(value.len() - len) {
            continue;
        }
        let suffix = &value[value.len() - len..];
        if DSML_ANY_PREFIX.starts_with(suffix)
            || SYSTEM_REMINDER_PREFIX.starts_with(suffix)
            || SYSTEM_REMINDER_UNDERSCORE_PREFIX.starts_with(suffix)
        {
            return len;
        }
    }
    0
}

pub(in crate::llm::openai_compatible) fn hidden_end_after(target: &str, offset: usize) -> Option<usize> {
    let remaining = &target[offset..];
    if remaining.starts_with(DSML_ANY_PREFIX) {
        return remaining
            .find(DSML_END)
            .map(|index| offset + index + DSML_END.len());
    }
    for tag in ["system-reminder", "system_reminder"] {
        let open_prefix = format!("<{tag}");
        if remaining.starts_with(&open_prefix) {
            let close = format!("</{tag}>");
            return remaining
                .find(&close)
                .map(|index| offset + index + close.len());
        }
    }
    None
}

pub(in crate::llm::openai_compatible) fn extract_dsml_tool_calls(mut content: String) -> (String, Vec<ToolCall>) {
    let mut calls = Vec::new();
    let mut index = 0usize;
    while let Some(start) = content.find(DSML_PREFIX) {
        let tag_end = content[start..]
            .find('>')
            .map(|offset| start + offset + 1)
            .unwrap_or(start + DSML_PREFIX.len());
        let body_start = tag_end;
        let Some(relative_end) = content[body_start..].find(DSML_END) else {
            content.replace_range(start.., "");
            break;
        };
        let end = body_start + relative_end;
        let block = content[body_start..end].to_string();
        calls.extend(parse_dsml_block(&block, &mut index));
        content.replace_range(start..end + DSML_END.len(), "");
    }
    (content.trim().to_string(), calls)
}

pub(in crate::llm::openai_compatible) fn strip_orphaned_dsml_tags(mut content: String) -> String {
    content = content.replace(DSML_END, "");
    content = content.replace(DSML_PREFIX, "");
    content = content.replace("</｜｜DSML｜｜invoke>", "");
    content = content.replace("<｜｜DSML｜｜invoke", "");
    content = content.replace("</｜｜DSML｜｜parameter>", "");
    content = content.replace("<｜｜DSML｜｜parameter", "");
    content.trim().to_string()
}

pub(in crate::llm::openai_compatible) fn parse_dsml_block(block: &str, index: &mut usize) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut rest = block;
    while let Some(start) = rest.find("<｜｜DSML｜｜invoke") {
        rest = &rest[start..];
        let Some(tag_end) = rest.find('>') else {
            break;
        };
        let tag = &rest[..tag_end];
        let Some(name) = attr_value(tag, "name") else {
            rest = &rest[tag_end..];
            continue;
        };
        let body_start = tag_end + 1;
        let Some(relative_end) = rest[body_start..].find("</｜｜DSML｜｜invoke>") else {
            break;
        };
        let body = &rest[body_start..body_start + relative_end];
        let arguments = parse_dsml_arguments(body);
        *index += 1;
        calls.push(ToolCall {
            id: format!("dsml-tool-call-{index}"),
            kind: "function".to_string(),
            function: ToolCallFunction {
                name,
                arguments: arguments.to_string(),
            },
        });
        rest = &rest[body_start + relative_end + "</｜｜DSML｜｜invoke>".len()..];
    }
    calls
}

pub(in crate::llm::openai_compatible) fn parse_dsml_arguments(body: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    let mut rest = body;
    while let Some(start) = rest.find("<｜｜DSML｜｜parameter") {
        rest = &rest[start..];
        let Some(tag_end) = rest.find('>') else {
            break;
        };
        let tag = &rest[..tag_end];
        let Some(name) = attr_value(tag, "name") else {
            rest = &rest[tag_end..];
            continue;
        };
        let value_start = tag_end + 1;
        let Some(relative_end) = rest[value_start..].find("</｜｜DSML｜｜parameter>") else {
            break;
        };
        let raw_value = rest[value_start..value_start + relative_end].trim();
        map.insert(name, parse_dsml_value(raw_value));
        rest = &rest[value_start + relative_end + "</｜｜DSML｜｜parameter>".len()..];
    }
    serde_json::Value::Object(map)
}

pub(in crate::llm::openai_compatible) fn parse_dsml_value(value: &str) -> serde_json::Value {
    let trimmed = value.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return value;
    }
    if let Ok(value) = trimmed.parse::<i64>() {
        return serde_json::Value::Number(value.into());
    }
    serde_json::Value::String(trimmed.trim_matches('"').to_string())
}

pub(in crate::llm::openai_compatible) fn attr_value(tag: &str, name: &str) -> Option<String> {
    let pattern = format!("{name}=\"");
    let start = tag.find(&pattern)? + pattern.len();
    let end = tag[start..].find('"')?;
    Some(tag[start..start + end].to_string())
}

pub(in crate::llm::openai_compatible) fn clean_plain_text(mut text: String) -> String {
    for tag in ["system-reminder", "system_reminder"] {
        text = strip_tagged_sections(text, tag);
    }
    text = text.replace("<system-reminder>", "");
    text = text.replace("</system-reminder>", "");
    text = text.replace("<system_reminder>", "");
    text = text.replace("</system_reminder>", "");
    text
}

pub(in crate::llm::openai_compatible) fn strip_tagged_sections(mut text: String, tag: &str) -> String {
    let close = format!("</{tag}>");
    let open_prefix = format!("<{tag}");
    loop {
        let Some(start) = text.find(&open_prefix) else {
            break;
        };
        // start 之后没有任何 `>`（流在标签中间被截断）时，`</tag>` 也必然
        // 不存在：直接按未闭合标签截掉其后全部内容。用 `start + open.len()`
        // 猜内容起点会在文本恰以 `<tag` 结尾时越界 panic。
        let Some(offset) = text[start..].find('>') else {
            text.replace_range(start.., "");
            break;
        };
        let content_start = start + offset + 1;
        let Some(relative_end) = text[content_start..].find(&close) else {
            text.replace_range(start.., "");
            break;
        };
        let end = content_start + relative_end + close.len();
        text.replace_range(start..end, "");
    }
    text
}
