//! extra_body 的合并与保留键。

use crate::llm::openai_compatible::*;


#[test]
fn extra_body_reserved_keys_match_each_protocol() {
    for reserved in [
        CHAT_RESERVED_BODY_KEYS,
        RESPONSES_RESERVED_BODY_KEYS,
        ANTHROPIC_RESERVED_BODY_KEYS,
    ] {
        let mut extra = serde_json::Map::new();
        for key in reserved {
            extra.insert((*key).to_string(), serde_json::json!("override"));
        }
        extra.insert("custom".to_string(), serde_json::json!("keep"));

        let sanitized = sanitize_extra_body(Some(extra), reserved).unwrap();
        assert_eq!(sanitized.len(), 1);
        assert_eq!(sanitized["custom"], "keep");
    }
}
