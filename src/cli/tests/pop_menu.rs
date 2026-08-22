//! pop 命令与它的选择菜单。

// 被测的东西散在 cli::mod 与 repl 的兄弟模块里，这里全都要够到。
use crate::cli::repl::width::*;
use crate::cli::*;
use super::shared::*;


#[test]
fn repl_pop_accepts_zero_or_one_positive_integer() {
    assert_eq!(parse_repl_pop_count("").unwrap(), None);
    assert_eq!(parse_repl_pop_count(" 3 ").unwrap(), Some(3));
    assert!(parse_repl_pop_count("0").is_err());
    assert!(parse_repl_pop_count("nope").is_err());
    assert!(parse_repl_pop_count("1 2").is_err());
}

#[test]
fn counted_pop_removes_oldest_turns_and_caps_at_available_count() {
    let temp = tempfile::tempdir().unwrap();
    let paths = pop_test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    for id in ["t1", "t2", "t3"] {
        state.start_turn(id, id, 999999).unwrap();
        state.complete_turn(id, "reply", None).unwrap();
    }

    let first = execute_pop(&paths, &config, &state, Some(2))
        .unwrap()
        .unwrap();
    assert_eq!(first.turns, 2);
    assert_eq!(
        state
            .load_visible_turns()
            .unwrap()
            .into_iter()
            .map(|turn| turn.turn_id)
            .collect::<Vec<_>>(),
        vec!["t3"]
    );

    let second = execute_pop(&paths, &config, &state, Some(99))
        .unwrap()
        .unwrap();
    assert_eq!(second.turns, 1);
    assert!(state.load_visible_turns().unwrap().is_empty());
}

#[test]
fn pop_menu_uses_three_lines_without_context_metadata() {
    let turn = sample_pop_turn(TurnStatus::Completed);
    let lines = pop_menu_turn_lines(&turn, true, false, 80)
        .map(|line| strip_terminal_control_sequences(&line));

    assert_eq!(lines[0], "› [ ] 2026-07-19 10:42");
    assert!(lines[1].contains("first prompt line"));
    assert!(!lines[1].contains("second prompt line"));
    assert!(lines[2].contains("first answer line"));
    assert!(!lines[2].contains("second answer line"));
    let joined = lines.join(" ");
    assert!(!joined.contains("hidden tool report"));
    assert!(!joined.contains("private reasoning"));
    assert!(lines.iter().all(|line| visible_width(line) <= 80));
}

#[test]
fn pop_menu_labels_an_interrupted_reply_without_showing_the_reminder() {
    let mut turn = sample_pop_turn(TurnStatus::Interrupted);
    turn.assistant_content = crate::state::interrupted_text().to_string();
    let lines = pop_menu_turn_lines(&turn, false, true, 80)
        .map(|line| strip_terminal_control_sequences(&line));

    assert!(lines[2].contains("中断") || lines[2].contains("interrupted"));
    assert!(!lines[2].contains("system-reminder"));
}

#[test]
fn filtered_pop_turns_keep_oldest_first_order() {
    let matcher = SkimMatcherV2::default();
    let items = vec![
        "old matching prompt".to_string(),
        "middle unrelated".to_string(),
        "new matching prompt".to_string(),
    ];

    assert_eq!(pop_matches(&matcher, &items, "matching"), vec![0, 2]);
}

#[test]
fn pop_is_a_repl_command_with_an_optional_count() {
    assert!(repl_commands().contains(&"/pop"));
    assert_eq!(split_repl_command("/pop 3"), ("/pop", "3"));
    // "/p" became ambiguous once /persona joined the table. 前缀只在 Tab 展开。
    assert_eq!(complete_repl_command("/po"), Some("/pop"));
    assert_eq!(complete_repl_command("/pe"), Some("/persona"));
}
