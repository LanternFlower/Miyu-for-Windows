//! 思考变体（variant）菜单。

// 被测的东西散在 cli::mod 与 repl 的兄弟模块里，这里全都要够到。
use crate::cli::repl::editor::*;
use crate::cli::repl::width::*;
use crate::cli::*;
/// 正名 `/effort`（codex / Claude Code 的叫法）；`/variant`（opencode 的叫法）是
/// 别名，打哪个都到同一条命令。
#[test]
fn effort_is_a_repl_command_and_variant_is_its_alias() {
    assert!(repl_commands().contains(&"/effort"));
    assert!(!repl_commands().contains(&"/variant"));
    assert!(is_repl_command("/variant"));
    assert!(is_repl_command("/EFFORT"));
    assert!(names_repl_command("/Variant", ReplSlashCommand::Effort));
    assert!(!names_repl_command("/variant", ReplSlashCommand::Models));
    assert!(matches!(
        parse_repl_input("/variant high"),
        ReplInput::Slash(ReplSlashCommand::Effort, "high")
    ));
    assert!(matches!(
        parse_repl_input("/effort high"),
        ReplInput::Slash(ReplSlashCommand::Effort, "high")
    ));
    assert_eq!(split_repl_command("/effort high"), ("/effort", "high"));
    assert_eq!(split_repl_command("/reset all"), ("/reset", "all"));
    // Tab 补的是用户正在打的那个词：/var → /variant，/eff → /effort。
    assert_eq!(complete_repl_command("/var"), Some("/variant"));
    assert_eq!(complete_repl_command("/eff"), Some("/effort"));
    // 打 `/` 列全表时只出正名，别名不另占一行。
    let all = repl_command_suggestions("/");
    assert!(all.contains(&"/effort"));
    assert!(!all.contains(&"/variant"));
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

/// 单模型菜单：行数 = 抬头 + visible + 帮助；j 移动、Tab 勾选、Enter 交出选择、Esc 取消。
#[test]
fn variant_menu_lines_and_keys() {
    let options = ThinkingVariantOptions {
        provider_id: "p".to_string(),
        model: "m".to_string(),
        variants: vec!["high".to_string(), "max".to_string()],
        selected: None,
    };
    let mut menu = VariantMenu::new(std::slice::from_ref(&options)).unwrap();
    assert!(menu.is_single());
    assert_eq!(menu.height(), 5);
    let lines = menu.lines(60, 3);
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(lines[0].contains("思考档位") || lines[0].contains("Thinking variant"));
    assert!(lines[1].contains("[*] default"), "{lines:?}");
    assert_eq!(menu.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(menu.handle_key(KeyCode::Tab, KeyModifiers::NONE), None);
    assert_eq!(
        menu.handle_key(KeyCode::Enter, KeyModifiers::NONE),
        Some(Some(vec![(
            "p".to_string(),
            "m".to_string(),
            Some("high".to_string())
        )]))
    );
    assert_eq!(
        menu.handle_key(KeyCode::Esc, KeyModifiers::NONE),
        Some(None)
    );
}

/// 两个模型：左栏 j 换模型，l 切到右栏后 j 动的是档位。
#[test]
fn variant_menu_with_two_models_switches_columns() {
    let make = |id: &str| ThinkingVariantOptions {
        provider_id: id.to_string(),
        model: "m".to_string(),
        variants: vec!["high".to_string()],
        selected: None,
    };
    let mut menu = VariantMenu::new(&[make("a"), make("b")]).unwrap();
    assert!(!menu.is_single());
    let lines = menu.lines(80, 2);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(
        lines[1].contains("a / m") && lines[1].contains("default"),
        "{lines:?}"
    );
    menu.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    menu.handle_key(KeyCode::Char('l'), KeyModifiers::NONE);
    menu.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    menu.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    let selections = menu
        .handle_key(KeyCode::Enter, KeyModifiers::NONE)
        .unwrap()
        .unwrap();
    assert_eq!(selections[0].2, None);
    assert_eq!(selections[1].2.as_deref(), Some("high"));
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

mod models_inherit {
    //! `/models` 的「继承」连带规矩（BUG-10）：取消继承时因继承而勾上的模型跟着取消。
    use crate::cli::model_cmds::{decide_model_menu, toggle_model_row, ModelMenuDecision};

    // 行 0 = 继承；行 1、2 是全局池里的（派生）；行 3 不是。
    const DERIVED: [bool; 4] = [false, true, true, false];

    #[test]
    fn unticking_inherit_cascades_to_the_derived_rows() {
        let mut active = vec![true, true, true, false];
        toggle_model_row(&mut active, &DERIVED, 0);
        assert_eq!(active, [false, false, false, false]);
        // 勾回继承：回到派生态。
        active[3] = true;
        toggle_model_row(&mut active, &DERIVED, 0);
        assert_eq!(active, [true, true, true, false]);
    }

    #[test]
    fn toggling_a_model_while_inheriting_leaves_inheritance_and_keeps_the_pool_as_a_start() {
        let mut active = vec![true, true, true, false];
        toggle_model_row(&mut active, &DERIVED, 3);
        assert_eq!(active, [false, true, true, true]);
        let mut active = vec![true, true, true, false];
        toggle_model_row(&mut active, &DERIVED, 1);
        assert_eq!(active, [false, false, true, false]);
    }

    #[test]
    fn decisions_cover_the_states_the_store_cannot_express() {
        let inheriting = [true, true, true, false];
        // 只取消继承、一个没勾：不能落盘，说清楚（原来被判成「未做修改」）。
        assert_eq!(
            decide_model_menu(&inheriting, &[false, false, false, false]),
            ModelMenuDecision::NeedOne
        );
        // 没动：未做修改。
        assert_eq!(
            decide_model_menu(&inheriting, &inheriting),
            ModelMenuDecision::NoChange
        );
        // 取消继承 + 钉住原班：这是一次真实改动（原来和「未做修改」等价）。
        assert_eq!(
            decide_model_menu(&inheriting, &[false, true, true, false]),
            ModelMenuDecision::Override(vec![0, 1])
        );
        // 本来是覆盖：勾回继承 = 回到继承；全清 = 回到继承（老规矩）；原样 = 未做修改。
        let overriding = [false, false, true, false];
        assert_eq!(
            decide_model_menu(&overriding, &[true, false, true, false]),
            ModelMenuDecision::Inherit
        );
        assert_eq!(
            decide_model_menu(&overriding, &[false, false, false, false]),
            ModelMenuDecision::Inherit
        );
        assert_eq!(
            decide_model_menu(&overriding, &overriding),
            ModelMenuDecision::NoChange
        );
        assert_eq!(
            decide_model_menu(&overriding, &[false, false, true, true]),
            ModelMenuDecision::Override(vec![1, 2])
        );
    }
}
