//! 静态时间线：shellhook／单次 CLI 那种「stdout 是终端、但不是全屏」的形态。
//!
//! 长相照着全屏的时间线来，只是没处点开：每一步跑完当场落进 scrollback，
//! diff 和命令输出的尾巴就地印在那一步底下，没有 `Worked for …` 收缩行。
//!
//! 帧里有转轮的「上移 → 清行 → 重画」，直接按 `\n` 切字节流看到的是错的
//! （被擦掉的行还在流里）。这里把帧喂给 `Term`（全屏那台终端模拟器）再读
//! 屏幕，看到的才是用户看到的。

use crate::cli::repl::tail::screen::term::Term;
use crate::i18n::text as t;
use crate::llm::{ChatStreamChunk, ChatStreamKind};
use crate::render::{ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode};
use crate::tools::CommandOutputStream;

/// 测试里 stdout 不是终端，`live_summary` 默认为假；shellhook 真跑起来时它是
/// 真的，得显式打开才走到静态时间线这条路。
fn static_renderer() -> StreamRenderer {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        4,
    );
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    renderer.live_summary = true;
    assert!(renderer.timeline_static(), "该走静态时间线");
    renderer
}

/// 把帧喂进终端模拟器，取屏幕上每一行的文字（右边的空白去掉）。
struct Screen {
    term: Term,
}

impl Screen {
    fn new() -> Self {
        let mut term = Term::default();
        term.set_cols(100);
        Self { term }
    }

    fn feed(&mut self, frame: &[u8]) {
        self.term.feed(frame);
    }

    fn lines(&self) -> Vec<String> {
        (0..self.term.line_count())
            .map(|index| {
                self.term
                    .row_spans(index)
                    .into_iter()
                    .map(|span| span.text)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn text(&self) -> String {
        self.lines().join("\n")
    }
}

fn strip_ansi(text: &str) -> String {
    crate::render::strip_ansi_text(text)
}

#[test]
fn each_step_lands_in_the_scrollback_as_soon_as_it_finishes() {
    let mut renderer = static_renderer();
    let mut screen = Screen::new();
    renderer
        .write_tool_call("web_search", r#"{"query":"第一个"}"#)
        .unwrap();
    let frame = renderer.take_output_frame();
    let raw = String::from_utf8_lossy(&frame);
    // 跑着的时候是 live 区里的转轮行，还没落地。
    assert!(
        !raw.contains("×1"),
        "静态时间线不该再画 inline 那套 `工具×1` 卡片: {raw:?}"
    );
    screen.feed(&frame);
    renderer
        .write_tool_result("web_search", true, "{}")
        .unwrap();
    screen.feed(&renderer.take_output_frame());
    let lines = screen.lines();
    let step = lines
        .iter()
        .find(|line| line.contains("第一个"))
        .unwrap_or_else(|| panic!("跑完的那一步没落地: {lines:?}"));
    assert!(!step.contains("×1"), "还是 inline 那套写法: {step:?}");
    // 落地的那一步退两格（用户：贴到左边框太靠左）。
    assert!(
        step.starts_with("  ") && !step.starts_with("   "),
        "静态时间线该整体退两格: {step:?}"
    );

    // 第二步：和上一步之间要有连线。
    renderer
        .write_tool_call("read", r#"{"path":"/tmp/b.txt"}"#)
        .unwrap();
    renderer.write_tool_result("read", true, "{}").unwrap();
    screen.feed(&renderer.take_output_frame());
    let lines = screen.lines();
    let first = lines
        .iter()
        .position(|line| line.contains("第一个"))
        .expect("第一步没了");
    let second = lines
        .iter()
        .position(|line| line.contains("/tmp/b.txt"))
        .unwrap_or_else(|| panic!("第二步没落地: {lines:?}"));
    assert!(first < second, "顺序乱了: {lines:?}");
    assert!(
        lines[first + 1..second]
            .iter()
            .any(|line| line.trim() == "│"),
        "两步之间没有连线: {lines:?}"
    );
}

#[test]
fn an_edit_prints_its_diff_right_under_the_step() {
    let mut renderer = static_renderer();
    let mut screen = Screen::new();
    renderer
        .write_tool_call("edit", r#"{"patchText":"*** Begin Patch\n*** Update File: /tmp/a.txt\n@@\n-旧的一行\n+新的一行\n*** End Patch\n"}"#)
        .unwrap();
    let preview = serde_json::json!({
        "path": "/tmp/a.txt",
        "diff": "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-旧的一行\n+新的一行\n",
    })
    .to_string();
    renderer
        .write_tool_progress("edit", &format!("__patch_preview__{preview}"))
        .unwrap();
    renderer
        .write_tool_result("edit", true, r#"{"ok":true}"#)
        .unwrap();
    screen.feed(&renderer.take_output_frame());
    let lines = screen.lines();
    let step = lines
        .iter()
        .position(|line| line.contains("/tmp/a.txt"))
        .unwrap_or_else(|| panic!("编辑那一步没落地: {lines:?}"));
    let after = &lines[step..];
    let plus = after
        .iter()
        .find(|line| line.contains("+ 新的一行"))
        .unwrap_or_else(|| panic!("diff 没印在那一步底下: {after:?}"));
    assert!(
        after.iter().any(|line| line.contains("- 旧的一行")),
        "diff 没印在那一步底下: {after:?}"
    );
    // diff 行从连线穿过（`  │ `），紧贴抬头不空行；行号栏按最大行号定宽（最少两格）。
    assert!(plus.starts_with("  │  1 + "), "diff 的缩进不对: {plus:?}");
    assert!(
        after[1].starts_with("  │ ") && after[1].contains("旧的一行"),
        "diff 和抬头之间不该空行: {after:?}"
    );
}

#[test]
fn a_command_prints_only_the_tail_of_its_output() {
    let mut renderer = static_renderer();
    let mut screen = Screen::new();
    renderer
        .write_tool_call("run_command", r#"{"command":"seq 1 12"}"#)
        .unwrap();
    for index in 1..=12 {
        renderer
            .write_command_output(
                "run_command",
                CommandOutputStream::Stdout,
                format!("第 {index} 行\n").as_bytes(),
            )
            .unwrap();
    }
    // 跑着的时候：转轮行底下露出输出尾巴（最多 `command_output_lines` 行）。
    let (_, live) = renderer.timeline_waiting();
    let live = strip_ansi(&live.expect("live 区是空的"));
    assert!(live.contains("seq 1 12"), "跑着的那一行没有命令: {live:?}");
    assert!(live.contains("第 12 行"), "输出尾巴没露出来: {live:?}");
    assert!(!live.contains("第 1 行\n"), "露出来的不是尾巴: {live:?}");
    screen.feed(&renderer.take_output_frame());

    renderer
        .write_tool_result("run_command", true, r#"{"success":true,"exit_code":0}"#)
        .unwrap();
    screen.feed(&renderer.take_output_frame());
    let lines = screen.lines();
    let step = lines
        .iter()
        .position(|line| line.trim_start().starts_with("$ ") && line.contains("seq 1 12"))
        .unwrap_or_else(|| panic!("命令那一步没落地: {lines:?}"));
    let after = &lines[step..];
    assert!(
        after
            .iter()
            .any(|line| line.contains(t("earlier output omitted", "已省略较早输出"))),
        "超出的部分没标省略: {after:?}"
    );
    assert!(
        after
            .iter()
            .any(|line| line.starts_with("  │ ") && line.contains("第 12 行")),
        "尾巴没落地／没从连线穿过: {after:?}"
    );
    assert!(
        after[1].contains(t("earlier output omitted", "已省略较早输出")),
        "抬头底下第一行该是省略标记，不空行: {after:?}"
    );
    assert!(
        !after.iter().any(|line| line.contains("第 3 行")),
        "落地的应该只是尾巴: {after:?}"
    );
    // 命令本身不再单独印一遍（那一行上已经有了）。
    assert_eq!(
        after
            .iter()
            .filter(|line| line.contains("seq 1 12"))
            .count(),
        1,
        "命令印了两遍: {after:?}"
    );
    // 跑着时那一行（转轮 + 命令）被擦干净了，换成了落地的静态行；屏上剩下的
    // 转轮只是"等下一步"的那一个，独自落在连线底下。
    let spinner_rows = lines
        .iter()
        .filter(|line| {
            line.trim_start()
                .chars()
                .next()
                .is_some_and(|ch| ('⠀'..='⣿').contains(&ch))
        })
        .collect::<Vec<_>>();
    assert!(
        spinner_rows.iter().all(|line| !line.contains("seq 1 12")),
        "跑着时那一行没擦掉: {lines:?}"
    );
    assert_eq!(spinner_rows.len(), 1, "该只剩等下一步的那个转轮: {lines:?}");
}

#[test]
fn a_failed_step_is_red_and_only_commands_show_their_output() {
    let mut renderer = static_renderer();
    let mut screen = Screen::new();
    // 普通工具跑砸了：那一行红，但**不印**报错——多半是一团裸 JSON，印出来只会丑
    //（用户拍板：除了命令，其他工具报错不需要报错信息）。
    renderer.write_tool_call("gpustoggle", "{}").unwrap();
    renderer
        .write_tool_result("gpustoggle", false, r#"{"error":"tool error: 显卡不见了"}"#)
        .unwrap();
    let frame = renderer.take_output_frame();
    let raw = String::from_utf8_lossy(&frame);
    assert!(raw.contains("\x1b[31m"), "失败那一步没标红: {raw:?}");
    screen.feed(&frame);
    let lines = screen.lines();
    assert!(
        !lines.iter().any(|line| line.contains("显卡不见了")),
        "普通工具的报错不该印出来: {lines:?}"
    );

    // 命令跑砸了：输出照印（几行尾巴），而且整段是红的。
    renderer
        .write_tool_call("run_command", r#"{"command":"seq 1 9 >&2; exit 3"}"#)
        .unwrap();
    for index in 1..=9 {
        renderer
            .write_command_output(
                "run_command",
                CommandOutputStream::Stdout,
                format!("错误第 {index} 行\n").as_bytes(),
            )
            .unwrap();
    }
    renderer
        .write_tool_result(
            "run_command",
            true,
            r#"{"success":false,"exit_code":3,"stdout":"","stderr":""}"#,
        )
        .unwrap();
    let frame = renderer.take_output_frame();
    let raw = String::from_utf8_lossy(&frame);
    screen.feed(&frame);
    let lines = screen.lines();
    let shown = lines.iter().filter(|line| line.contains("错误第")).count();
    assert!(shown > 0, "命令的报错输出一行都没印: {lines:?}");
    assert!(
        shown <= 4,
        "命令的报错输出没按行数限制（{shown} 行）: {lines:?}"
    );
    assert!(
        raw.contains("\x1b[31m错误第 9 行"),
        "命令的报错输出不是红的: {raw:?}"
    );
    // 输出从连线穿过，不空行。
    let step = lines
        .iter()
        .position(|line| line.contains("exit 3"))
        .expect("命令那一步没落地");
    assert!(
        lines[step + 1].starts_with("  │ "),
        "输出没从连线穿过: {lines:?}"
    );

    // 跑成的普通工具只留那一行，不印输出。
    renderer
        .write_tool_call("read", r#"{"path":"/tmp/x"}"#)
        .unwrap();
    renderer
        .write_tool_result("read", true, "{\"content\":\"一大段文件内容\"}")
        .unwrap();
    screen.feed(&renderer.take_output_frame());
    let lines = screen.lines();
    assert!(
        lines.iter().any(|line| line.contains("/tmp/x")),
        "跑成的那一步没落地: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("一大段文件内容")),
        "跑成的工具不该把输出印出来: {lines:?}"
    );
}

#[test]
fn a_thought_is_one_line_and_there_is_no_worked_for_handle() {
    let mut renderer = static_renderer();
    let mut screen = Screen::new();
    renderer
        .start_reasoning_phase(std::time::Instant::now())
        .unwrap();
    renderer
        .write_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Reasoning,
            text: "先看一眼再说，这段想法不该整段印出来".to_string(),
        })
        .unwrap();
    renderer
        .write_tool_call("web_search", r#"{"query":"查一下"}"#)
        .unwrap();
    renderer
        .write_tool_result("web_search", true, "{}")
        .unwrap();
    renderer
        .write_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: "正文来了".to_string(),
        })
        .unwrap();
    renderer.finish().unwrap();
    screen.feed(&renderer.take_output_frame());
    let lines = screen.lines();
    let thought = lines
        .iter()
        .position(|line| line.contains(t("thought", "已思考")))
        .unwrap_or_else(|| panic!("想的那一步没落地: {lines:?}"));
    assert!(
        !lines.iter().any(|line| line.contains("不该整段印出来")),
        "思考全文被印出来了: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Worked for")),
        "静态时间线不该有点不开的收缩行: {lines:?}"
    );
    let search = lines
        .iter()
        .position(|line| line.contains("查一下"))
        .expect("工具那一步没落地");
    let body = lines
        .iter()
        .position(|line| line.contains("正文来了"))
        .expect("正文没了");
    assert!(thought < search && search < body, "顺序乱了: {lines:?}");
    // 时间线和正文之间空一行分开；跑着时那根连线和转轮行都擦干净了。
    assert_eq!(body - search, 2, "时间线到正文该正好隔一行空: {lines:?}");
    assert!(lines[search + 1].trim().is_empty(), "{lines:?}");
}

#[test]
fn the_live_area_continues_the_rail_after_a_committed_step() {
    let mut renderer = static_renderer();
    renderer
        .start_reasoning_phase(std::time::Instant::now())
        .unwrap();
    renderer
        .write_tool_call("web_search", r#"{"query":"x"}"#)
        .unwrap();
    renderer
        .write_tool_result("web_search", true, "{}")
        .unwrap();
    // 跑完那一刻转轮就回来了，live 区从一根连线接上去。
    assert!(renderer.wait_spinner.is_some(), "收完那一步转轮没回来");
    let (_, live) = renderer.timeline_waiting();
    let live = live.expect("live 区是空的");
    let first = strip_ansi(live.lines().next().unwrap_or_default());
    assert_eq!(first.trim(), "│", "live 区没有从连线接上去: {live:?}");
    // 已经落地的那一步不再出现在 live 区里。
    assert!(
        !live.contains("web_search") && !strip_ansi(&live).contains("· x"),
        "落地的步骤又在 live 区里画了一遍: {live:?}"
    );
}

/// 上一个工具刚回来、下一次模型请求还在路上：转轮独自落在 logo 那一列上，
/// 而不是整个 live 区消失。
#[test]
fn the_spinner_stays_on_the_rail_between_steps() {
    let mut renderer = static_renderer();
    renderer
        .write_tool_call("web_search", r#"{"query":"x"}"#)
        .unwrap();
    renderer
        .write_tool_result("web_search", true, "{}")
        .unwrap();
    let (_, live) = renderer.timeline_waiting();
    let live = live.expect("live 区不该是空的");
    let rows = live.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2, "该是连线 + 转轮两行: {live:?}");
    assert!(
        rows[1].contains(crate::render::wait_spinner::BLOCK_MARKER),
        "第二行不是转轮: {live:?}"
    );
}

#[test]
fn a_command_still_running_at_the_end_is_folded_in_as_interrupted() {
    let mut renderer = static_renderer();
    let mut screen = Screen::new();
    renderer
        .write_tool_call("run_command", r#"{"command":"sleep 30"}"#)
        .unwrap();
    renderer
        .write_command_output(
            "run_command",
            CommandOutputStream::Stdout,
            "开始\n".as_bytes(),
        )
        .unwrap();
    renderer.finish().unwrap();
    let frame = renderer.take_output_frame();
    let raw = String::from_utf8_lossy(&frame);
    assert!(
        !raw.contains("×1") && !raw.contains("↳"),
        "inline 那套命令卡片漏出来了: {raw:?}"
    );
    screen.feed(&frame);
    let lines = screen.lines();
    let step = lines
        .iter()
        .find(|line| line.contains("sleep 30"))
        .unwrap_or_else(|| panic!("没跑完的命令没收进时间线: {lines:?}"));
    assert!(
        step.contains(t("interrupted", "已中断")),
        "没说明它是被打断的: {step:?}"
    );
    assert!(raw.contains("\x1b[31m"), "被打断的那一步没标红: {raw:?}");
    assert!(
        lines.iter().any(|line| line.contains("开始")),
        "打断前的输出丢了: {lines:?}"
    );
}

/// 管道里（stdout 不是终端）还是老的一行摘要。
#[test]
fn piped_output_keeps_the_plain_summary() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        4,
    );
    renderer.use_buffered_output();
    renderer.live_summary = false;
    assert!(!renderer.timeline_enabled());
    renderer
        .write_tool_call("web_search", r#"{"query":"x"}"#)
        .unwrap();
    renderer
        .write_tool_result("web_search", true, "{}")
        .unwrap();
    let text = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
    assert!(text.contains("×1"), "管道里该还是老摘要: {text:?}");
}
