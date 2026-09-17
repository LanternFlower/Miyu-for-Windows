//! REPL 斜杠命令的解析与补全。

// 被测的东西散在 cli::mod 与 repl 的兄弟模块里，这里全都要够到。
use crate::cli::repl::editor::*;
use crate::cli::repl::width::*;
use crate::cli::*;
#[test]
fn reset_is_a_repl_command() {
    assert!(repl_commands().contains(&"/reset"));
}

#[test]
fn compact_is_a_repl_command() {
    assert!(repl_commands().contains(&"/compact"));
}

#[test]
fn usage_and_persona_are_repl_commands() {
    assert!(repl_commands().contains(&"/usage"));
    assert!(repl_commands().contains(&"/persona"));
    assert_eq!(complete_repl_command("/us"), Some("/usage"));
    assert_eq!(
        split_repl_command("/persona Alice.md"),
        ("/persona", "Alice.md")
    );
}

/// `--allow-read` 前后都摘得掉,且不按空白切词——路径里可以有空格。
#[test]
fn allow_read_flag_is_taken_from_either_end() {
    assert_eq!(
        take_repl_flag("/home/a b/c --allow-read", "--allow-read"),
        ("/home/a b/c", true)
    );
    assert_eq!(
        take_repl_flag("--allow-read /home/a b/c", "--allow-read"),
        ("/home/a b/c", true)
    );
    assert_eq!(take_repl_flag("--allow-read", "--allow-read"), ("", true));
    assert_eq!(
        take_repl_flag("  /home/proj  ", "--allow-read"),
        ("/home/proj", false)
    );
    // 前缀像但不是开关的路径不能被吃掉。
    assert_eq!(
        take_repl_flag("--allow-readme", "--allow-read"),
        ("--allow-readme", false)
    );
}

/// 候选不随「打全」消失：`/sessio` 有、`/session` 也有，在打参数时也有。
/// 以前打全那一刻候选没了，看着像自己把命令打错了（用户实测）。
#[test]
fn suggestions_survive_a_fully_typed_command() {
    assert_eq!(repl_command_suggestions("/sessio"), vec!["/session"]);
    assert_eq!(repl_command_suggestions("/session"), vec!["/session"]);
    assert_eq!(repl_command_suggestions("/session "), vec!["/session"]);
    assert_eq!(repl_command_suggestions("/session 3"), vec!["/session"]);
    assert_eq!(repl_command_suggestions("/variant high"), vec!["/variant"]);
    // 打了参数就不再按前缀筛：`/sessio 3` 什么都不是。
    assert!(repl_command_suggestions("/sessio 3").is_empty());
    // Tab 不碰已经在打参数的行，否则展开会把参数抹掉。
    assert_eq!(complete_repl_command("/session 3"), None);
    assert_eq!(complete_repl_command("/session"), Some("/session"));
}

/// 全屏候选面板：打全了照样在，并带上参数提示；别名那一行注明等于哪条正名。
#[test]
fn command_hint_panel_keeps_showing_a_fully_typed_command() {
    let lines = command_hint_lines("/session", 80);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("/session [name|index]"), "{lines:?}");
    let lines = command_hint_lines("/session 3", 80);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("/session [name|index]"), "{lines:?}");
    // 没打全时不带参数提示，和以前一样只列名字。
    let lines = command_hint_lines("/sessio", 80);
    assert!(lines[0].starts_with("/session  "), "{lines:?}");
    let lines = command_hint_lines("/var", 80);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("/variant"), "{lines:?}");
    assert!(lines[0].contains("= /effort"), "{lines:?}");
    // 无参数的命令打全了不带空的参数提示。
    let lines = command_hint_lines("/usage", 80);
    assert!(lines[0].starts_with("/usage  "), "{lines:?}");
    assert!(command_hint_lines("/sessio 3", 80).is_empty());
}

#[test]
fn command_suggestions_are_prefixed_and_truncated() {
    let suggestions = repl_command_suggestions("/");
    let line = repl_command_suggestions_line(&suggestions, 24);
    assert!(line.starts_with("/new"));
    assert!(visible_width(&line) <= 24);

    let line = repl_command_suggestions_line(&["/compact"], 40);
    assert_eq!(line, "/compact");
}

#[test]
fn truncation_respects_very_narrow_widths() {
    assert_eq!(truncate_visible_width("abcdef", 0), "");
    assert_eq!(truncate_visible_width("abcdef", 1), ".");
    assert_eq!(truncate_visible_width("abcdef", 2), "..");
    assert_eq!(truncate_visible_width("abcdef", 3), "...");
}

#[test]
fn shortcut_hint_line_is_bar_aligned_and_truncated() {
    // Tab 切换模式已随闲聊模式删除,提示行首个词条现在是换行快捷键。
    let line = repl_shortcut_hint_line(PersonaLane::Active, 24);
    assert!(strip_terminal_control_sequences(&line).contains("Shift+Enter"));
    assert!(visible_width(&line) <= 24);
}

#[test]
fn inline_fuzzy_lines_are_bar_aligned_and_truncated() {
    let header = inline_fuzzy_header("big", 12);
    assert!(strip_terminal_control_sequences(&header).contains(t("Select", "选择模型")));
    assert!(visible_width(&header) <= 12);

    let item = inline_fuzzy_item_line("opencode Zen / big-pickle", true, false, 16);
    let item_plain = strip_terminal_control_sequences(&item);
    assert!(item_plain.starts_with("› [ ]"));
    assert!(item_plain.contains("open"));
    assert!(visible_width(&item) <= 16);

    let item = inline_fuzzy_item_line("opencode Zen / big-pickle", false, true, 18);
    let item_plain = strip_terminal_control_sequences(&item);
    assert!(item_plain.starts_with("  [*]"));
    assert!(item_plain.contains("opencode"));
    assert!(visible_width(&item) <= 18);

    let help = inline_fuzzy_help_line(40);
    let help_plain = strip_terminal_control_sequences(&help);
    assert!(help_plain.contains("j/k"));
    assert!(visible_width(&help) <= 40);
}

#[test]
fn wipe_is_its_own_command_not_a_suffix_on_reset() {
    // `/reset` and `/reset all` differed by one word and by everything
    // else: one starts a conversation over, the other erased memory, every
    // session and the generated skills. They answer under separate names
    // now, and `/wipe` is far enough from `/w…` prefixes to be typed on
    // purpose.
    assert!(matches!(
        parse_repl_input("/wipe"),
        ReplInput::Slash(ReplSlashCommand::Wipe, "")
    ));
    assert!(matches!(
        parse_repl_input("/reset"),
        ReplInput::Slash(ReplSlashCommand::Reset, "")
    ));
    assert!(matches!(
        parse_repl_input("/reset all"),
        ReplInput::Slash(ReplSlashCommand::Reset, "all")
    ));
}

/// 前缀展开只活在 Tab 补全里,不在执行路径上。用户按 Tab 看得见展开结果、
/// 能反悔;回车则一律按完整名字算(见 `unique_prefixes_are_not_executed`)。
#[test]
fn tab_completion_still_expands_unique_prefixes() {
    assert_eq!(complete_repl_command("/model"), Some("/models"));
    assert_eq!(complete_repl_command("/compa"), Some("/compact"));
    // 歧义前缀不展开:/config /compact /clear 都以 /c 开头。
    assert_eq!(complete_repl_command("/co"), None);
    assert_eq!(complete_repl_command("hello"), None);
}

/// 空会话撤大厅的判据:以路径开头的第一句话是**消息**,不是命令。
///
/// 退回修复前(两处各写一遍 `starts_with('/')`),`/home/x.md 删掉` 被当成命令
/// 跳过撤场,可它回落成聊天照常发给了模型——大厅留在画面上,流式正文画上去
/// 就是重影,整轮结束视图还停在大厅。
#[test]
fn a_path_like_first_message_still_leaves_the_lobby() {
    for message in [
        "/home/shorin/Downloads/群聊消息线程-实现方案.md 删除这个文件",
        "/home/x.md 删掉",
        "/usr/bin/env 是什么",
        "/rest",
        "你好",
    ] {
        assert!(
            submission_leaves_lobby(message),
            "should leave the lobby: {message:?}"
        );
    }
    // 真命令与空输入不撤大厅。
    for command in ["/config", "  /session  ", "/POP 3", "", "   "] {
        assert!(
            !submission_leaves_lobby(command),
            "should keep the lobby: {command:?}"
        );
    }
}

#[test]
fn parse_repl_input_dispatches_by_table() {
    assert!(matches!(parse_repl_input("hello"), ReplInput::Chat));
    assert!(matches!(
        parse_repl_input("/models"),
        ReplInput::Slash(ReplSlashCommand::Models, "")
    ));
    // `/reset` 与 `/reset all`:名字精确,参数原样带过去。
    assert!(matches!(
        parse_repl_input("/reset all"),
        ReplInput::Slash(ReplSlashCommand::Reset, "all")
    ));
    // Case-insensitive.
    assert!(matches!(
        parse_repl_input("/POP 3"),
        ReplInput::Slash(ReplSlashCommand::Pop, "3")
    ));
}

/// 回车不做前缀展开。这些以前都会**静默执行**:`/n 什么的` 建了个叫「什么的」
/// 的会话,`/d 3` 删掉 3 号会话。用户想说的只是一句普通的话。
#[test]
fn unique_prefixes_are_not_executed() {
    for input in [
        "/n 什么的", // 曾命中 /new [name]
        "/d 3",      // 曾命中 /delete [name|index]
        "/m gpt-4",  // 曾命中 /models
        "/v 高",     // 曾命中 /variant
        "/compa",    // 曾命中 /compact
        "/se",       // 曾命中 /session
    ] {
        assert!(
            matches!(parse_repl_input(input), ReplInput::Chat),
            "{input} 必须当成聊天,不能当命令执行"
        );
    }
}

/// `/` 开头但不命中命令表的输入是**普通消息**,不是「未知命令」。
/// 以前整行被丢弃、输入框也清空,用户只看到一句「未知命令」。
#[test]
fn unmatched_slash_input_falls_through_to_chat() {
    for input in [
        "/home/shorin/notes.md 这个文件讲了什么",
        "/usr/bin 下面有什么",
        "/nope",
        "/rest", // 打错的命令也发给模型,模型会告诉你
        "/",
        "/1234",
    ] {
        assert!(
            matches!(parse_repl_input(input), ReplInput::Chat),
            "{input} 必须落到聊天"
        );
    }
}

#[test]
fn every_repl_slash_command_has_a_table_entry() {
    // repl_command_spec panics on a missing entry; touch every variant.
    for spec in REPL_COMMAND_TABLE {
        assert_eq!(repl_command_spec(spec.command).command, spec.command);
    }
}

/// WebUI 的命令是 REPL 那张表的**子集**，不是另一份清单。
///
/// 命令表原本是 `pub(in crate::cli)`，WebUI 够不到，只能自己再维护一份——
/// 加一条命令忘了改另一边，两个界面就分叉了。上提到 crate 级之后
/// `GET /api/commands` 直接从这张表按 `web` 标记过滤。
#[test]
fn web_commands_are_a_subset_of_the_repl_table() {
    let web = miyu_core::slash_commands::web_commands();
    assert!(!web.is_empty(), "WebUI 一条命令都没开，命令平面等于没做");
    for spec in &web {
        assert!(
            REPL_COMMAND_TABLE
                .iter()
                .any(|entry| entry.name == spec.name),
            "{} 不在 REPL 命令表里——WebUI 不该有自己的命令",
            spec.name
        );
        // 前端拿到的每条都要能渲染成一行「名字 参数提示 —— 帮助」。
        assert!(!spec.help().is_empty(), "{} 没有帮助文案", spec.name);
    }
    // 开了 web 的命令，WebUI 侧必须真有实现（commands.js 的 tryRun 里逐条分支）。
    // 这里钉住当前这批，加新命令时会红，提醒你两边一起改。
    let names = web.iter().map(|spec| spec.name).collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "/sandbox",
            "/pop",
            "/compact",
            "/goal",
            "/reset",
            "/reset-memory",
            "/reset-all-memory"
        ]
    );
}
