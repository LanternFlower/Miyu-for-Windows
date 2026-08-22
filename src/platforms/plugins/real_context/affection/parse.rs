//! 模型输出与档案字段的解析、限长。
//!
//! 名字、备注、标签都会进提示词，所以都有字符上限（`MAX_*_CHARS`）。
//! `finite` / `finite_nonnegative` 挡的是 NaN 和无穷——模型给出的数字直接参与
//! 算分，一个 NaN 会把整条档案污染成不可比较。

use crate::platforms::plugins::real_context::affection::*;

pub(crate) const MAX_NAME_CHARS: usize = 128;

pub(crate) const MAX_NOTE_CHARS: usize = 2_000;

pub(crate) const MAX_TAG_CHARS: usize = 24;

pub(crate) const MAX_REASON_CHARS: usize = 1_000;

pub(crate) const MAX_UPDATE_TEXT_CHARS: usize = 16_000;

pub(crate) fn clean_tags(tags: Vec<String>, maximum: usize) -> Vec<String> {
    let rejected = [
        "正常闲聊",
        "普通闲聊",
        "日常互动",
        "正常互动",
        "有效互动",
        "技术求助",
        "知识问答",
        "找bot帮忙",
        "找你帮忙",
        "询问问题",
        "提出问题",
        "感谢",
        "夸奖",
    ];
    let mut seen = HashSet::new();
    tags.into_iter()
        .map(|tag| bounded_single_line(&tag, MAX_TAG_CHARS))
        .filter(|tag| !tag.is_empty() && !rejected.contains(&tag.as_str()))
        .filter(|tag| seen.insert(tag.clone()))
        .take(if maximum == 0 { usize::MAX } else { maximum })
        .collect()
}

pub(crate) fn tag_changes(previous: &[String], current: &[String]) -> (Vec<String>, Vec<String>) {
    let previous_set = previous.iter().cloned().collect::<HashSet<_>>();
    let current_set = current.iter().cloned().collect::<HashSet<_>>();
    let added = current
        .iter()
        .filter(|tag| !previous_set.contains(*tag))
        .cloned()
        .collect();
    let removed = previous
        .iter()
        .filter(|tag| !current_set.contains(*tag))
        .cloned()
        .collect();
    (added, removed)
}

pub(crate) fn tags_from_value(value: Option<&Value>, maximum: usize) -> Vec<String> {
    let tags = match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                item.as_str().or_else(|| {
                    item.get("name")
                        .or_else(|| item.get("tag"))
                        .and_then(Value::as_str)
                })
            })
            .map(str::to_string)
            .collect(),
        Some(Value::String(value)) => value
            .split([',', '，', '、', '\n'])
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    clean_tags(tags, maximum)
}

pub(crate) fn normalized_user_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let digits = value
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    Some(if digits.is_empty() {
        bounded_single_line(value, 256)
    } else {
        digits
    })
}

pub(crate) fn required_user_id(arguments: &Value) -> Result<String> {
    let requested = arguments
        .get("user_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("query_qq_relationship requires a non-empty user_id"))?;
    normalized_user_id(requested)
        .ok_or_else(|| anyhow::anyhow!("query_qq_relationship requires a valid user_id"))
}

pub(crate) fn parse_json_object(text: &str) -> Result<Value> {
    let trimmed = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if value.is_object() {
            return Ok(value);
        }
    }
    let json_text = crate::json_extract::extract_json_object(trimmed)
        .context("affection output contains no complete JSON object")?;
    let value: Value = serde_json::from_str(json_text)?;
    if !value.is_object() {
        bail!("affection response is not a JSON object");
    }
    Ok(value)
}

pub(crate) fn number(value: &Value, key: &str, default: f64) -> f64 {
    let number = value
        .get(key)
        .and_then(|value| match value {
            Value::Number(value) => value.as_f64(),
            Value::String(value) => value.trim().parse().ok(),
            _ => None,
        })
        .unwrap_or(default);
    finite(number, default)
}

pub(crate) fn bounded_text(value: &str, maximum: usize) -> String {
    value
        .chars()
        .filter(|character| *character != '\0')
        .take(maximum)
        .collect()
}

pub(crate) fn bounded_single_line(value: &str, maximum: usize) -> String {
    bounded_text(value, maximum)
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn finite(value: f64, default: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        default
    }
}

pub(crate) fn finite_nonnegative(value: f64) -> f64 {
    finite(value, 0.0).max(0.0)
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
