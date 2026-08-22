//! 字段显示：脱敏、变体、语言、菜单窗口。

use crate::config_tui::{
    choice_display_label, field_display_value, language_choice_label, language_choice_value,
    menu_window, parse_extra_body, t, textarea_temp_file, thinking_variant_field, Field,
};
use crate::llm::ThinkingVariantOptions;

#[test]
fn textarea_temp_file_is_markdown_and_reopenable_for_writing() {
    let path = textarea_temp_file("before").unwrap();
    assert_eq!(path.extension().and_then(|value| value.to_str()), Some("md"));
    std::fs::write(&path, "after").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "after");
}

#[test]
fn sensitive_field_is_masked_until_actively_edited() {
    let field = Field::new("API Key", "secret-key".to_string()).sensitive();

    assert_eq!(field_display_value(&field, false), "********");
    assert_eq!(field_display_value(&field, true), "secret-key");
}

#[test]
fn empty_sensitive_field_remains_empty() {
    let field = Field::new("API Key", String::new()).sensitive();

    assert_eq!(field_display_value(&field, false), "");
}

#[test]
fn thinking_variant_field_uses_raw_model_options_and_default_choice() {
    let field = thinking_variant_field(
        &ThinkingVariantOptions {
            provider_id: "provider".to_string(),
            model: "model".to_string(),
            variants: vec!["default".to_string(), "high".to_string()],
            selected: Some("default".to_string()),
        },
        Some("default"),
    );

    assert_eq!(field.label, t("Thinking variant", "思考程度"));
    assert_eq!(field.value, "default");
    assert_eq!(field.choices, vec!["", "default", "high"]);
    assert!(field.raw_choice_labels);
    assert_eq!(choice_display_label("high", "", true), "high");
    assert_eq!(field.empty_choice_label, "default");

    let unsupported = thinking_variant_field(
        &ThinkingVariantOptions {
            provider_id: "provider".to_string(),
            model: "plain-model".to_string(),
            variants: Vec::new(),
            selected: None,
        },
        None,
    );
    assert_eq!(unsupported.choices, vec![""]);
    assert_eq!(field_display_value(&unsupported, false), "default");

    let stale = thinking_variant_field(
        &ThinkingVariantOptions {
            provider_id: "provider".to_string(),
            model: "changed-model".to_string(),
            variants: Vec::new(),
            selected: None,
        },
        Some("high"),
    );
    assert_eq!(stale.value, "high");
    assert_eq!(stale.choices, vec!["", "high"]);
    assert_eq!(field_display_value(&stale, false), "high");
}

#[test]
fn sensitive_textarea_displays_configured_item_count() {
    let field = Field::textarea("API Keys", "first\n\nsecond, third".to_string()).sensitive();

    assert_eq!(
        field_display_value(&field, false),
        t("[3 configured]", "[已配置 3 项]")
    );
}

#[test]
fn empty_sensitive_textarea_renders_empty() {
    let field = Field::textarea("API Keys", String::new()).sensitive();

    assert_eq!(field_display_value(&field, false), "");
}

#[test]
fn language_choices_have_locale_specific_labels() {
    assert_eq!(language_choice_label("auto", false), Some("Auto"));
    assert_eq!(language_choice_label("en", false), Some("English"));
    assert_eq!(
        language_choice_label("zh", false),
        Some("Simplified Chinese")
    );
    assert_eq!(language_choice_label("auto", true), Some("自动"));
    assert_eq!(language_choice_label("en", true), Some("英语"));
    assert_eq!(language_choice_label("zh", true), Some("简体中文"));
}

#[test]
fn language_choice_labels_map_to_stable_values() {
    for value in ["auto", "Auto", "自动"] {
        assert_eq!(language_choice_value(value), Some("auto"));
    }
    for value in ["en", "English", "英语"] {
        assert_eq!(language_choice_value(value), Some("en"));
    }
    for value in ["zh", "Simplified Chinese", "简体中文"] {
        assert_eq!(language_choice_value(value), Some("zh"));
    }
    assert_eq!(language_choice_value("unsupported"), None);
}

#[test]
fn menu_window_keeps_selection_visible_for_long_lists() {
    assert_eq!(menu_window(100, 0, 5), 0..5);
    assert_eq!(menu_window(100, 50, 5), 48..53);
    assert_eq!(menu_window(100, 99, 5), 95..100);
    assert_eq!(menu_window(3, 2, 10), 0..3);
    assert_eq!(menu_window(0, 0, 5), 0..0);
}

#[test]
fn extra_body_parser_accepts_only_json_objects() {
    for input in ["true", "\"hello\"", "[1, 2, 3]", "{invalid"] {
        assert!(parse_extra_body(input).is_err());
    }

    let parsed = parse_extra_body(r#"{"enable_thinking":false}"#)
        .unwrap()
        .unwrap();
    assert_eq!(parsed["enable_thinking"], false);
    assert!(parse_extra_body("  ").unwrap().is_none());
}
