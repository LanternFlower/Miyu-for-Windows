//! 从模型输出的松散文本里提取 JSON 对象的公共辅助。
//!
//! 模型输出是不可信输入：`find('{')..=rfind('}')` 的朴素切片在花括号反序出现
//! 时（如 `}abc{`）start > end 直接 panic。所有需要"从散文里捞 JSON 对象"的
//! 调用点必须走这里的深度计数提取器。

/// 返回 `content` 中第一个配平的 `{...}` 对象切片。跟踪字符串字面量与转义，
/// 字符串内部的花括号不参与配平计数。没有完整对象时返回 `None`，绝不 panic。
pub fn extract_json_object(content: &str) -> Option<&str> {
    let start = content.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in content[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && in_string {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return Some(&content[start..end]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_plain_object() {
        assert_eq!(extract_json_object(r#"{"a":1}"#), Some(r#"{"a":1}"#));
    }

    #[test]
    fn extracts_object_from_surrounding_prose() {
        assert_eq!(
            extract_json_object(r#"结论如下 {"a":1} 完毕"#),
            Some(r#"{"a":1}"#)
        );
    }

    #[test]
    fn reversed_braces_do_not_panic() {
        assert_eq!(extract_json_object("}abc{"), None);
        assert_eq!(extract_json_object("}{"), None);
    }

    #[test]
    fn braces_inside_strings_are_ignored() {
        assert_eq!(
            extract_json_object(r#"{"text":"a } b { c"}"#),
            Some(r#"{"text":"a } b { c"}"#)
        );
    }

    #[test]
    fn nested_objects_balance() {
        assert_eq!(
            extract_json_object(r#"x {"a":{"b":2}} y"#),
            Some(r#"{"a":{"b":2}}"#)
        );
    }

    #[test]
    fn incomplete_object_returns_none() {
        assert_eq!(extract_json_object(r#"{"a":1"#), None);
        assert_eq!(extract_json_object("no json here"), None);
    }
}
