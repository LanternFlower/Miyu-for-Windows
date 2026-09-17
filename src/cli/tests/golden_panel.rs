//! 后台子代理面板的**逐字节快照**。
//!
//! 和 `render::tests::golden` 是同一张网的两半：那边钉主线三个 surface，这边钉
//! 后台面板——它是**另一个组装器**（从日志行攒步，而主线/前台面板从事件攒步，
//! 见 `docs/plan/2026-09-17-render-unification.md` §1.2）。两个组装器已经漂移过
//! 三处（同文件 §2.3），所以把它单独冻住：合并那两条路的时候，这份 golden 是
//! 「哪一边变了」的唯一裁判。
//!
//! 日志内容写死在这里、不生成：日志格式本身也是契约（`readable_subagent_log_line_timed`
//! 的输出落库在 `job.trace`，AGENTS §2.3 要双兼容），所以样本里**故意混着老格式**
//! （无 `\t` 工具 id、无耗时）与新格式。

use super::tui_blocks::with_blocks;
use crate::cli::repl::tail::screen::Screen;

/// 一份固定的后台流水账：八种前缀 + 新旧两代格式。
const LOG: &str = concat!(
    "[提示] 去看看那个目录里有什么\n",
    "[思考] 0.4s\t先列一下,再决定\n",
    "[工具] 运行命令 · ls -la\n",
    "[结果] 运行命令 ok · ls -la\n",
    // 老格式:没有 `\t` 工具 id,也没有耗时。
    "[工具] 编辑文件 · /tmp/a.txt\n",
    "[结果] 编辑文件 ok · /tmp/a.txt\n",
    "[统计] 词元 1234\n",
    "[正文] 里面是空的。\n",
    "[思考] 先想下一步\n",
    "[工具] 运行命令 · exit 3\n",
    "[结果] 运行命令 err · exit 3\n",
);

/// 样本里的标签必须是**写日志那一侧真的会写**的。
///
/// 这份流水账是手写的（老格式没法让今天的编码器吐出来），手写就会出这种事：
/// 初版把 `[提示]` 写成了 `[提示词]`——那是面板**渲染出来**的抬头，不是日志
/// 标签。解码器认不出来，那一行掉进「无标签续行」分支，收缩行还因此多数了一个
/// tool。冻住一份根本不存在的格式，这张网就是假的，所以对着写日志那一侧的清单
/// （`tools::subagent::protocol::LOG_TAGS`，它两侧各有一条测试钉着）校一遍。
#[test]
fn the_sample_only_uses_tags_the_writer_actually_writes() {
    for line in LOG.lines() {
        // 老格式那几行故意不带 `\t` 工具 id，但标签总是有的。
        assert!(
            miyu_engine::tools::subagent::protocol::LOG_TAGS
                .iter()
                .any(|tag| line.starts_with(tag)),
            "{line:?} 的标签写日志那一侧不会写"
        );
    }
}

/// 收缩行报的 `N tools` 得是**真跑过的工具数**。
///
/// `[统计]` 那一行一度被数进去：同文件里它自己的注释写着「它不是工具调用：没有
/// 结果行」，而计数那一行却把它算上了。更巧的是那一行的内容正好是「工具调用 N
/// 次」——于是收缩行报出来的数，比真跑过的多一个，多的那个就是**报告工具数的
/// 那一行**。前台一直不算它（报告 §6.3 第 2 项）。
///
/// 这里不照抄一个数字，而是从样本里数 `[工具]` 有几条：改了样本，期望跟着走。
#[test]
fn the_fold_line_counts_the_tools_that_actually_ran() {
    with_blocks(|| {
        // 收缩的是「正文之前」那一段，样本里正文之后还有一次调用。
        let before_speech = LOG.split("[正文]").next().expect("样本里有正文");
        let ran = before_speech
            .lines()
            .filter(|line| line.starts_with("[工具]"))
            .count();
        assert_eq!(ran, 2, "样本变了，先确认这条测试还在量同一件事");
        let text = capture();
        assert!(
            text.contains(&format!("{ran} tools")),
            "收缩行报的工具数不是真跑过的 {ran} 个:\n{text}"
        );
    });
}

fn golden_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        // golden 和主线那四份放一起（渲染层现在在 `miyu-hosts` 里）。
        .join("crates/miyu-hosts/src/render/tests/golden/background-panel.ansi")
}

fn capture() -> String {
    let dir = std::env::temp_dir().join(format!("miyu-golden-panel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建目录");
    let path = dir.join("job.log");
    std::fs::write(&path, LOG).expect("写日志");
    let mut screen = Screen::detached(80, 24);
    assert!(
        screen.open_log_overlay(path, "走查子代理".into(), None, String::new()),
        "面板没开起来"
    );
    let rows = screen.overlay_rows_ansi();
    let raw = rows.join("\n");
    // 块 id 是全局自增计数,随谁先跑而变——它不是表现。
    fancy_regex::Regex::new(r"miyu-block=\d+")
        .expect("掩码正则")
        .replace_all(&raw, "miyu-block=<ID>")
        .into_owned()
}

#[test]
fn background_panel_output_is_frozen() {
    with_blocks(|| {
        // golden 是**默认（Nerd Font）那一档**的快照。`MIYU_TUI_ASCII=1` 下图标
        // 本来就该不一样（那正是 `glyphs_never_mix_nerd_and_ascii` 要的东西），
        // 拿它去比是拿两把尺量同一件事。
        if std::env::var_os("MIYU_TUI_ASCII").is_some() {
            return;
        }
        let actual = capture();
        let path = golden_path();
        if std::env::var_os("MIYU_GOLDEN_WRITE").is_some() {
            std::fs::create_dir_all(path.parent().expect("父目录")).expect("建目录");
            std::fs::write(&path, &actual).expect("写 golden");
            return;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "{} 不存在;用 MIYU_GOLDEN_WRITE=1 跑一次生成",
                path.display()
            )
        });
        if expected == actual {
            return;
        }
        let at = expected
            .lines()
            .zip(actual.lines())
            .position(|(left, right)| left != right);
        panic!(
            "后台面板的输出变了。第一处不同在第 {} 行:\n  golden: {:?}\n  现在  : {:?}\n\
             确实想改表现的话:MIYU_GOLDEN_WRITE=1 重写,并在提交里说明改了哪一段。",
            at.map_or(0, |index| index + 1),
            at.and_then(|index| expected.lines().nth(index)),
            at.and_then(|index| actual.lines().nth(index)),
        );
    });
}

/// 图标会撞，所以「这一步是什么」不能看图标猜。
///
/// `load_tools` / `manage_skill` / `load_skill` 的图标正好就是「差事」那一步的
/// `PROMPT_GLYPH`（都是 `\u{f4a5}`）。而 `is_tool_step` 原来是拿图标跟三个特殊
/// 图标比出来的，于是这三个工具被判成「不是工具调用」：
///
/// ```text
///    提示词 · 去装点工具
///    加载工具 · 装上 web            ← 调用立了一步，配不到结果，没 ok 没耗时
///    加载工具 · 400ms · 装上 web    ← 结果又立了一步
/// ```
///
/// 同一次调用出现两遍，`[输出]` 还因为找不到工具步整个丢掉。子代理装工具是家常
/// 便饭，这条路天天走。
#[test]
fn a_tool_whose_icon_collides_with_the_prompt_icon_is_still_one_step() {
    with_blocks(|| {
        let log = concat!(
            "[提示] 去装点工具\n",
            "[工具] load_tools\t加载工具 · 装上 web\n",
            "[输出] 装好了\n",
            "[结果] load_tools\t加载工具 ok · 0.4s · 装上 web\n",
        );
        let dir = std::env::temp_dir().join(format!("miyu-icon-clash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(&path, log).expect("写日志");
        let mut screen = Screen::detached(80, 24);
        assert!(
            screen.open_log_overlay(path, "走查".into(), None, String::new()),
            "面板没开起来"
        );
        let rows = screen.overlay_rows_ansi();
        let hits = rows
            .iter()
            .filter(|row| row.contains("加载工具"))
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 1, "同一次调用出现了不止一步: {rows:#?}");
        assert!(
            hits[0].contains("400ms"),
            "结果没配上这一步（耗时丢了）: {:?}",
            hits[0]
        );
    });
}

/// 一块面板里的图标要么都是 Nerd Font，要么都不是——**不许混**。
///
/// 工具那几个图标本来就走 `MIYU_TUI_ASCII` 开关，而面板里「已思考 / 跑砸了 /
/// 认不出的工具」三个是硬编码的 Nerd 码位。没装 Nerd Font 的人于是在同一块面板
/// 里看到一半豆腐块：
///
/// ```text
///   \u{f0768} 已思考 · 想一下        ← 豆腐块
///   ⚙ 编辑文件 · 400ms · /tmp/a     ← 认得出
///   \u{f00d} 老格式没有 id           ← 豆腐块
/// ```
///
/// 这条测试在**两种模式下都成立**（Nerd 下四个都该是 PUA，ASCII 下四个都不该是），
/// 所以默认那次跑也拦得住反向的漏：谁把工具图标改成硬编码，它一样会红。
/// 跑 ASCII 那一侧：`MIYU_TUI_ASCII=1 cargo test --lib -- glyphs_never_mix`。
#[test]
fn glyphs_never_mix_nerd_and_ascii() {
    /// Nerd Font 把图标放在私有使用区。
    fn is_private_use(text: &str) -> bool {
        text.chars().any(
            |ch| matches!(ch as u32, 0xE000..=0xF8FF | 0xF_0000..=0xF_FFFD | 0x10_0000..=0x10_FFFD),
        )
    }
    // 基准：工具图标那一套（它认 `MIYU_TUI_ASCII`）。拿一个两种模式取值不同的。
    let baseline = is_private_use(miyu_hosts::render::tool_glyph_for("edit"));
    for (what, glyph) in [
        ("已思考", miyu_hosts::render::timeline::glyph_think()),
        ("跑砸了", miyu_hosts::render::timeline::glyph_err()),
        ("差事", miyu_hosts::render::prompt_glyph()),
    ] {
        assert_eq!(
            is_private_use(glyph),
            baseline,
            "{what} 那个图标（{glyph:?}）和工具图标不在同一个模式里"
        );
    }
}

/// 想的那一步，抬头上的耗时不能丢。
///
/// `[思考] 1.2s\t…` 那个耗时是桥专门写进流水账的（`stamp_thought_lines`），面板
/// 该按主线的说法报 `已思考 · 1.2s`。合并步模型时我把它弄丢了：带耗时的那份抬头
/// 构造成了死代码，而 `#![allow(dead_code)]` 把警告盖住了，golden 里那条思考又
/// 恰好没带耗时，所以一声没吭。
#[test]
fn a_thought_step_reports_how_long_it_took() {
    with_blocks(|| {
        let log = concat!(
            "[提示] 去看看\n",
            "[工具] run_command\t运行命令 · ls\n",
            "[结果] run_command\t运行命令 ok · 0.4s · ls\n",
            "[思考] 1.2s\t想到哪儿了\n",
        );
        let dir = std::env::temp_dir().join(format!("miyu-thought-secs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(&path, log).expect("写日志");
        let mut screen = Screen::detached(80, 24);
        assert!(screen.open_log_overlay(path, "走查".into(), None, String::new()));
        let rows = screen.overlay_rows_ansi();
        let thought = rows
            .iter()
            .find(|row| row.contains("已思考"))
            .unwrap_or_else(|| panic!("没有思考那一步:\n{rows:#?}"));
        assert!(thought.contains("1.2s"), "想的那一步没报耗时:\n{thought:?}");
        // §6.3 第 1 项拍板：不带窥视（取前台那份）。
        assert!(
            !thought.contains("想到哪儿了"),
            "已结算的思考不该再带窥视（用户 09-17 拍板取前台）:\n{thought:?}"
        );
    });
}
