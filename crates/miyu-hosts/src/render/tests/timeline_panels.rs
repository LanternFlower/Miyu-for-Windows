//! 过程时间线:子代理面板、命令展开、预览行数这些「面板形态」的断言。
//! 从 `timeline.rs` 拆出来(09-16,那份超过了文件规模基线);共用的夹具留在那边。

use super::timeline::{
    block_id_in, timeline_renderer, timeline_renderer_with_preview_rows, with_blocks,
};
use crate::render::stream::timeline::LIVE_SPINNER_CELL;
use crate::render::t;
use std::time::Duration;

/// 面板里正在准备／正在跑的那一步，左边距上有转轮占位格：画面板的那一层每一帧
/// 把它换成当帧的点阵字形（用户实测：子代理浮层没有转轮）。跑完就没了。
#[test]
fn a_running_subagent_step_carries_the_spinner_cell() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        renderer.subagent_tool_preparing("subagent", "run_command");
        let preparing = crate::render::blocks::get(id).unwrap_or_default();
        assert!(
            preparing.iter().any(|line| {
                line.contains(LIVE_SPINNER_CELL)
                    && crate::render::strip_ansi_text(line)
                        .contains(t("Preparing command", "准备执行"))
            }),
            "准备那一行没有转轮占位: {preparing:?}"
        );
        renderer.subagent_tool_started(
            "subagent",
            "run_command",
            "运行命令",
            r#"{"command":"sleep 5"}"#,
        );
        let running = crate::render::blocks::get(id).unwrap_or_default();
        let row = running
            .iter()
            .find(|line| crate::render::strip_ansi_text(line).contains("sleep 5"))
            .unwrap_or_else(|| panic!("跑着的那一步不见了: {running:?}"));
        let text = crate::render::strip_ansi_text(row);
        assert!(
            text.starts_with(&format!("{LIVE_SPINNER_CELL} ")),
            "占位格不在第 0 列: {text:?}"
        );
        renderer.subagent_tool(
            "subagent",
            "run_command",
            "运行命令",
            r#"{"command":"sleep 5"}"#,
            true,
            "输出",
        );
        let done = crate::render::blocks::get(id).unwrap_or_default();
        assert!(
            !done.iter().any(|line| line.contains(LIVE_SPINNER_CELL)),
            "跑完了占位格还在: {done:?}"
        );
    });
}

/// 没有主题规则的工具，面板里的窥视是参数的值串起来，不是裸 JSON；不到十分之一
/// 秒的步也不报 `0.0s`。
#[test]
fn a_subagent_step_without_a_subject_rule_spells_out_its_arguments() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查包","prompt":"去查"}"#)
            .unwrap();
        renderer.subagent_tool(
            "subagent",
            "aur_query",
            "AUR 查询",
            r#"{"action":"info","package_name":"zzq"}"#,
            true,
            "输出",
        );
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let rows: Vec<String> = crate::render::blocks::get(id)
            .unwrap_or_default()
            .iter()
            .map(|line| crate::render::strip_ansi_text(line))
            .collect();
        let row = rows
            .iter()
            .find(|line| line.contains("AUR 查询"))
            .unwrap_or_else(|| panic!("那一步不见了: {rows:?}"));
        assert!(row.contains("info · zzq"), "窥视不是人话: {row:?}");
        assert!(!row.contains("{\"action\""), "窥视是裸 JSON: {row:?}");
        assert!(!row.contains("0.0s"), "报了个 0.0s: {row:?}");
    });
}

/// 主线上不到十分之一秒的步不报秒数：`· 0.0s` 只是噪音（用户实测）。
#[test]
fn a_quick_tool_step_does_not_report_zero_seconds() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("web_search", r#"{"query":"miyu 转轮"}"#)
            .unwrap();
        renderer
            .write_tool_result("web_search", true, "done")
            .unwrap();
        renderer.finalize_tools_summary().unwrap();
        let step = renderer
            .timeline_step_lines()
            .into_iter()
            .map(|line| crate::render::strip_ansi_text(&line))
            .find(|line| line.contains("miyu 转轮"))
            .expect("没有那一步");
        assert!(!step.contains("0.0s"), "报了个 0.0s: {step:?}");
    });
}

/// 参数开始流（「准备xx」）那一刻，这一段思考就结算成一步、排在准备行**上面**；
/// 原来要等结果回来才结算，面板里「准备执行」一直压在「思考中」上头，思考的
/// 耗时还把工具跑的时间算了进去（用户实测截图）。
#[test]
fn a_preparing_subagent_settles_its_thought_first() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        renderer.subagent_thought("subagent", "先想想");
        renderer.subagent_tool_preparing("subagent", "run_command");
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let rows: Vec<String> = crate::render::blocks::get(id)
            .unwrap_or_default()
            .iter()
            .map(|line| crate::render::strip_ansi_text(line))
            .collect();
        let thought = rows
            .iter()
            .position(|line| line.contains(t("thought", "已思考")))
            .unwrap_or_else(|| panic!("思考没结算成一步: {rows:?}"));
        let preparing = rows
            .iter()
            .position(|line| line.contains(t("Preparing command", "准备执行")))
            .unwrap_or_else(|| panic!("没有准备那一行: {rows:?}"));
        assert!(thought < preparing, "准备行压在思考上头: {rows:?}");
        assert!(
            !rows
                .iter()
                .any(|line| line.contains(t("thinking", "思考中"))),
            "还挂着「思考中」: {rows:?}"
        );
    });
}

/// 面板每个 tick 重灌一遍：「准备执行 · 0.0s」的秒数会走（原来停在事件到来那一刻）。
#[test]
fn live_subagent_panels_tick_between_events() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        renderer.subagent_tool_preparing("subagent", "run_command");
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let before = crate::render::blocks::get(id)
            .unwrap_or_default()
            .join("\n");
        assert!(before.contains("0.0s"), "刚开始不是 0.0s: {before:?}");
        std::thread::sleep(Duration::from_millis(250));
        renderer.refresh_subagent_panels();
        let after = crate::render::blocks::get(id)
            .unwrap_or_default()
            .join("\n");
        assert!(!after.contains("0.0s"), "重灌之后秒数没走: {after:?}");
    });
}

/// 子代理的命令那一步点开：命令本身一段、空一行、输出——和主线那一步一个样子，
/// 正文里不再带 `$`（那是抬头上的图标）。
#[test]
fn a_subagent_command_step_opens_like_the_main_line() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        renderer.subagent_tool(
            "subagent",
            "run_command",
            "运行命令",
            r#"{"command":"ls -la"}"#,
            true,
            "total 0",
        );
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let panel = crate::render::blocks::get(id).unwrap_or_default();
        let step = panel
            .iter()
            // 抬头上现在是 title(这里没给,所以只有工具名);命令搬进了正文。
            .find(|line| {
                crate::render::strip_ansi_text(line).contains(t("Run command", "运行命令"))
            })
            .unwrap_or_else(|| panic!("那一步不见了: {panel:?}"));
        let step_id = block_id_in(step).expect("那一步没挂块");
        let detail: Vec<String> = crate::render::blocks::get(step_id)
            .unwrap_or_default()
            .iter()
            .map(|line| crate::render::strip_ansi_text(line))
            .collect();
        let command = detail
            .iter()
            .position(|line| line.trim() == "ls -la")
            .unwrap_or_else(|| panic!("点开没有命令本身: {detail:?}"));
        assert!(
            !detail.iter().any(|line| line.trim().starts_with("$ ls")),
            "正文里带了 $: {detail:?}"
        );
        assert!(
            detail[command + 1].trim().is_empty(),
            "命令和输出之间没空一行: {detail:?}"
        );
        assert!(
            detail.iter().any(|line| line.contains("total 0")),
            "点开没有输出: {detail:?}"
        );
    });
}
#[test]
fn panel_speech_is_markdown_rendered() {
    let lines = crate::render::timeline::render_speech_lines(
        "**Phase 2** 与 `code` 完成\n\n- 一条\n- 两条",
        60,
    );
    let text = lines.join("\n");
    assert!(!text.contains("**"), "星号还裸着: {text:?}");
    assert!(text.contains("\x1b[1m"), "没有加粗样式: {text:?}");
    let plain = crate::render::strip_ansi_text(&text);
    assert!(
        plain.contains("Phase 2") && plain.contains("code"),
        "内容丢了: {plain:?}"
    );
    assert!(
        plain
            .lines()
            .filter(|line| line.contains("一条") || line.contains("两条"))
            .count()
            == 2,
        "列表项没了: {plain:?}"
    );
}

/// 面板里的正文按面板宽度渲染：代码块、表格都不能比面板宽，长行折进框里
///（用户实测截图：按整屏宽度排完再折进面板，是碎行和大片空白）。
#[test]
fn panel_speech_blocks_fit_the_panel_width() {
    let text = "```sh\nfor i in $(seq 1 120); do echo \"a very long command line that keeps going on and on\"; sleep 1; done\n```\n\n| Metric | Value |\n|---|---|\n| calls | 10 |\n";
    let lines = crate::render::timeline::render_speech_lines(text, 40);
    for line in &lines {
        let width = crate::render::command_ansi_width(line);
        assert!(width <= 40, "有一行比面板宽 ({width}): {line:?}");
    }
    let plain = lines
        .iter()
        .map(|line| crate::render::strip_ansi_text(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        plain.contains("sleep 1; done"),
        "代码长行被截掉了: {plain:?}"
    );
    assert!(
        plain.contains('┌') && plain.contains("calls"),
        "表格没画出来: {plain:?}"
    );
    // 渲染完把宽度还回去，别影响这条线程后面的渲染。
    assert_eq!(crate::render::cols_override(), 0);
}

/// Arch 那一家子的工具挂 Arch 的 Nerd Font 标（U+F08C7，用户指名），官方包、AUR、
/// Wiki、新闻一个样子。
#[test]
fn arch_family_tools_get_the_arch_logo() {
    if std::env::var_os("MIYU_TUI_ASCII").is_some() {
        return;
    }
    for name in [
        "aur",
        "archlinux_official_package_query",
        "archwiki_query",
        "archlinux_news",
        "install_aur_package",
        "review_aur_package",
    ] {
        assert_eq!(
            crate::render::tool_glyph_for(name),
            "\u{f08c7}",
            "{name} 没挂 Arch 的标"
        );
    }
    // 别的联网工具还是地球。
    assert_eq!(crate::render::tool_glyph_for("web_search"), "\u{f0ac}");
}

/// 「准备xx」那一行挂的是那个工具自己的图标：准备编辑=铅笔、准备执行=`$`，
/// 和它跑起来之后那一步一个样子（用户 09-14 要求）。主线、子代理面板都是。
#[test]
fn a_preparing_row_wears_the_tools_own_glyph() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.write_tool_preparing("edit", false).unwrap();
        let (glyph, text) = renderer.timeline_preparing_line().expect("没有准备那一行");
        assert_eq!(
            glyph,
            crate::render::tool_glyph_for("edit"),
            "准备编辑没挂铅笔"
        );
        assert!(text.contains(t("Preparing edit", "准备编辑")), "{text:?}");

        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        renderer.subagent_tool_preparing("subagent", "run_command");
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let rows: Vec<String> = crate::render::blocks::get(id)
            .unwrap_or_default()
            .iter()
            .map(|line| crate::render::strip_ansi_text(line))
            .collect();
        let row = rows
            .iter()
            .find(|line| line.contains(t("Preparing command", "准备执行")))
            .unwrap_or_else(|| panic!("面板里没有准备那一行: {rows:?}"));
        assert!(
            row.contains(&format!(
                " {} ",
                crate::render::tool_glyph_for("run_command")
            )),
            "面板里准备执行没挂 $: {row:?}"
        );
    });
}

/// 参数每流一片就来一条准备事件：同一阶段的转轮不能每条都重起——重起就是在
/// 第 0、1 帧之间抖（用户实测：主体「准备xx」的转轮特别快、特别鬼畜）。
#[test]
fn repeated_preparing_events_do_not_restart_the_spinner() {
    with_blocks(|| {
        // 测试里 stdout 不是终端；报个宽度转轮才认自己在往终端画。
        crate::render::set_cols_override(100);
        let mut renderer = timeline_renderer();
        renderer.use_buffered_output();
        renderer.write_tool_preparing("edit", false).unwrap();
        let first = renderer.take_output_frame();
        assert!(!first.is_empty(), "第一条准备事件该把转轮画出来");
        for _ in 0..5 {
            renderer.write_tool_preparing("edit", false).unwrap();
        }
        let again = renderer.take_output_frame();
        assert!(
            again.is_empty(),
            "同一阶段的准备事件重画了转轮: {:?}",
            String::from_utf8_lossy(&again)
        );
        // 换了阶段（另一个工具开始流参数）才换文字，也不必重起。
        renderer.write_tool_preparing("run_command", false).unwrap();
        let (glyph, _) = renderer.timeline_preparing_line().expect("准备那一行");
        assert_eq!(glyph, crate::render::tool_glyph_for("run_command"));
        crate::render::set_cols_override(0);
    });
}

/// 全屏下的自动压缩：提示是时间线那种带图标的一行，摘要不往正文里流，压完收成
/// 一块 `› 上下文已压缩`，点开才是全文（用户：压缩上下文只有右上角的通知）。
#[test]
fn auto_compact_folds_its_summary_into_a_block_in_fullscreen() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_buffered_output();
        renderer.write_system_message("正在压缩上下文...").unwrap();
        let notice = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        assert!(
            notice.contains(crate::render::timeline::glyph_notice())
                && notice.contains("正在压缩上下文"),
            "提示行没有图标: {notice:?}"
        );
        for piece in ["摘要第一段\n", "摘要第二段\n"] {
            renderer
                .write_compact_chunk(&miyu_core::llm::ChatStreamChunk {
                    kind: miyu_core::llm::ChatStreamKind::Content,
                    text: piece.to_string(),
                })
                .unwrap();
        }
        assert!(
            renderer.take_output_frame().is_empty(),
            "摘要流到正文里去了"
        );
        renderer.finish_compact().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        let plain = crate::render::strip_ansi_text(&frame);
        assert!(
            plain.contains(&format!(
                "› {}",
                miyu_base::i18n::text("context compacted", "上下文已压缩")
            )),
            "没收成一块: {plain:?}"
        );
        assert!(!plain.contains("摘要第二段"), "摘要平铺出来了: {plain:?}");
        let id = block_id_in(&frame).expect("那一块没登记");
        let detail = crate::render::blocks::get(id)
            .unwrap_or_default()
            .join("\n");
        assert!(
            crate::render::strip_ansi_text(&detail).contains("摘要第二段"),
            "点开没有摘要: {detail:?}"
        );
    });
}
