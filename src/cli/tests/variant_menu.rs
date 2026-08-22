//! 思考变体（variant）菜单。

// 被测的东西散在 cli::mod 与 repl 的兄弟模块里，这里全都要够到。
use crate::cli::repl::width::*;
use crate::cli::*;
#[test]
fn variant_is_a_repl_command_with_arguments() {
    assert!(repl_commands().contains(&"/variant"));
    assert_eq!(split_repl_command("/variant high"), ("/variant", "high"));
    assert_eq!(split_repl_command("/reset all"), ("/reset", "all"));
    assert_eq!(complete_repl_command("/var"), Some("/variant"));
}

#[test]
fn variant_menu_checks_pending_selection_before_confirming() {
    let options = ThinkingVariantOptions {
        provider_id: "ririxin".to_string(),
        model: "deepseek-v4-flash".to_string(),
        variants: vec!["high".to_string(), "max".to_string()],
        selected: Some("high".to_string()),
    };
    let mut item = VariantMenuItem::from_options(&options);
    assert_eq!(
        item.options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        vec!["default", "high", "max"]
    );
    assert_eq!(item.selection().2.as_deref(), Some("high"));

    item.cursor = 2;
    assert_eq!(item.selection().2.as_deref(), Some("high"));
    item.check_cursor();
    assert_eq!(item.selection().2.as_deref(), Some("max"));
}

#[test]
fn single_variant_menu_uses_content_width() {
    let item = VariantMenuItem::from_options(&ThinkingVariantOptions {
        provider_id: "ririxin".to_string(),
        model: "deepseek-v4-flash".to_string(),
        variants: vec!["high".to_string(), "max".to_string()],
        selected: None,
    });

    assert!(single_variant_content_width(&item) < 30);
}

#[test]
fn mixed_variant_columns_do_not_fill_wide_terminal() {
    let items = ["myopencode", "myopencode6"]
        .into_iter()
        .map(|provider_id| {
            VariantMenuItem::from_options(&ThinkingVariantOptions {
                provider_id: provider_id.to_string(),
                model: "deepseek-v4-flash-free".to_string(),
                variants: vec!["high".to_string(), "max".to_string()],
                selected: None,
            })
        })
        .collect::<Vec<_>>();

    let (left, right) = variant_menu_column_widths(&items, 120);
    assert!(left + right < 80);
    assert!(left >= visible_width("myopencode6 / deepseek-v4-flash-free") + 2);
    assert!(right >= visible_width("[*] default") + 2);
}

#[test]
fn mixed_endpoint_label_only_omits_unset_variant() {
    assert_eq!(
        mixed_model_endpoint_label("provider", "model", None),
        "provider / model"
    );
    assert_eq!(
        mixed_model_endpoint_label("provider", "model", Some("default")),
        "provider / model · default"
    );
    assert_eq!(
        mixed_model_endpoint_label("provider", "model", Some("high")),
        "provider / model · high"
    );
}

#[test]
fn variant_menu_distinguishes_unset_from_default_effort() {
    let options = ThinkingVariantOptions {
        provider_id: "groq".to_string(),
        model: "qwen/qwen3-32b".to_string(),
        variants: vec!["none".to_string(), "default".to_string()],
        selected: Some("default".to_string()),
    };
    let item = VariantMenuItem::from_options(&options);

    assert_eq!(item.options[0].label, "default");
    assert_eq!(item.options[0].value, None);
    assert_eq!(item.options[2].label, "default (variant)");
    assert_eq!(item.options[2].value.as_deref(), Some("default"));
    assert_eq!(item.selected, 2);
    assert_eq!(item.selection().2.as_deref(), Some("default"));
}

#[test]
fn explicit_variant_prefix_can_select_default_effort() {
    let argument = "variant:default";
    assert_eq!(argument.strip_prefix("variant:"), Some("default"));
    assert_ne!(argument, "default");
}

#[test]
fn variant_name_resolution_handles_default_and_case_insensitive_names() {
    let available = vec!["low".to_string(), "high".to_string(), "default".to_string()];

    assert_eq!(
        resolve_variant_name("HIGH", &available).unwrap(),
        Some("high".into())
    );
    assert_eq!(resolve_variant_name("default", &available).unwrap(), None);
    assert_eq!(
        resolve_variant_name("variant:default", &available).unwrap(),
        Some("default".into())
    );
    assert!(resolve_variant_name("unknown", &available).is_err());
    assert!(resolve_variant_name("Variant:default", &available).is_err());
}
