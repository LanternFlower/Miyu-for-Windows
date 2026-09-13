//! 全屏 TUI 的过程时间线。
//!
//! inline REPL 把一轮里的工具和思考压成「一行摘要 + 详情行」，因为它没有回翻、
//! 没有点击，展开了就再也收不回去。全屏有屏幕也有鼠标，于是换成时间线：
//!
//! ```text
//!   │ ⚙ 运行命令 · 2.4s
//!   │ ✳ 已思考 · 320 词元 · 7.5s
//! ```
//!
//! 一段连续的过程结束（模型开始说正文／回合结束／面板要抢屏）就**收成一行**：
//!
//! ```text
//!   › Worked for 12.3s · 3 tools · 2 thoughts · 1 err
//! ```
//!
//! 这一行是可展开块，展开出来就是上面那条时间线；时间线里每一项**又**是可展开
//! 块，点开是那个工具的完整输出或那段思考的全文。嵌套由 `screen/expand.rs` 负责，
//! 这里只管把块标记按层套好。
//!
//! 全屏下是这套**可展开**的时间线（[`crate::render::blocks::enabled`]）。
//!
//! 不是全屏、但 stdout 是个终端的那些形态——shellhook、单次 `miyu "…"`——走同一条
//! 时间线的**静态**版（[`StreamRenderer::timeline_static`]）：长相一样，只是没有
//! 鼠标也没有回翻，所以没什么可展开的。每一步跑完就直接落进 scrollback，能展开的
//! 东西（补丁 diff、命令输出的尾巴）就地印在那一步底下；live 区只留一根连线和
//! 正在跑的那一行；也不写 `Worked for …` 收缩行——点不开的把手只是一行废话。
//!
//! 只有 stdout 不是终端（管道）时才还是老的一行摘要。

use super::StreamRenderer;
use crate::render::blocks;
use crate::render::t;
use std::time::{Duration, Instant};

/// 竖线。它和 logo **同在一列**：logo 是这一步的节点，竖线是节点之间的连线，
/// 各占一行。分成两列的话左边会多出一根从头贯到尾的栏杆，那是画框不是时间线。
const RAIL: &str = "│";
/// 全屏下时间线整体的左缩进：第 0–1 列是页边距，正文、用户消息都从第 2 列起。
const INDENT: &str = "  ";

/// 这一刻时间线该缩进多少。
///
/// 两种形态都退两格。静态版一度贴着第 0 列（和原来那块 `~ 工具×1 ok` 卡片同一个
/// 位置），用户看了说整体太靠左、直接贴到边框了——时间线是"过程"，比正文退一步
/// 才读得出主次。
fn indent() -> &'static str {
    INDENT
}

/// 静态时间线里一步底下那几行的前缀：连线穿过去。
///
/// 原来正文那几行是缩进四格、上下各空一行——连线在每一步的正文处断掉，一屏
/// 看下来时间线是碎的（用户实测截图「timeline 断得很严重」）。竖线贯穿正文、
/// 不空行，一眼就能看出这几行属于上面那一步。
fn rail_prefix() -> String {
    format!("\x1b[2m{}{RAIL}\x1b[0m ", indent())
}
/// 图标用 Nerd Font 的字形（私有区）。
///
/// 之前那套 `⚙ ✎ ▤ ⌕` 是从通用符号里凑的：粗细、基线、留白各不相同，排在一列
/// 里参差不齐。Nerd Font 的图标是**同一套字体里画的**，一列排下来才齐。
///
/// 装不了 Nerd Font 的话设 `MIYU_TUI_ASCII=1` 退回通用符号——图标好看不该是
/// 用不了的理由。
fn nerd() -> bool {
    static NERD: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NERD.get_or_init(|| std::env::var_os("MIYU_TUI_ASCII").is_none())
}

/// 认不出的工具。芯片——没归到哪一类，那就是"有个东西在跑"。
fn glyph_tool() -> &'static str {
    if nerd() {
        "\u{f4bc}"
    } else {
        "⚙"
    }
}

/// 出错。**错比「是什么工具」更要紧**，所以它盖过按类型挑的图标。
fn glyph_err() -> &'static str {
    if nerd() {
        "\u{f00d}"
    } else {
        "✗"
    }
}

/// 通知（后台任务完成之类）。铃铛：它不是某一步，是"有件事发生了"。
pub(crate) fn glyph_notice() -> &'static str {
    if nerd() {
        "\u{f0f3}"
    } else {
        "⚙"
    }
}

/// 思考。原子——脑子里的东西在转，灯泡那个更像"想到了"。
///
/// （一度以为截图里那个方框是缺字、把它换掉了，其实那**就是**原子那个字形。
/// 这台机器的字体里 MDI 段是全的，别再改。）
fn glyph_think() -> &'static str {
    if nerd() {
        "\u{f0768}"
    } else {
        "✳"
    }
}

/// 工具图标。一眼分出「这一步在干什么」比认出具体是哪个工具更有用，
/// 所以按**动作类型**归组，不是一个工具一个图标。
pub(crate) fn tool_glyph(name: &str) -> &'static str {
    // 事件名可能带后缀（`subagent:<描述>`、`use_meme:<名字>`），按基名认。
    let name = crate::render::tool_event_base_name(name);
    if !nerd() {
        // 没有 Nerd Font 的时候**别凑**。
        //
        // 通用符号来自不同的字表，粗细、基线、占几格全不一样，排成一列参差不齐
        // （用户原话「ASCII 字符是大小不一的」）。与其凑一堆半像不像的，不如只
        // 留几个**一眼认得出**的：跑命令的提示符、问号，其余统一一个齿轮——
        // "这是个工具"本来就够用了，分得清哪一类是 Nerd Font 那一档的事。
        return match name {
            "run_command" => "$",
            "ask_question" => "?",
            _ => "⚙",
        };
    }

    match name {
        // 终端
        // 跑命令就是 `$`——提示符本身比任何图标都直白。终端那个图标让给脚本：
        // 「跑一条命令」和「管一个脚本」是两件事。
        "run_command" => "$",
        // 铅笔 / 垃圾桶
        "edit" => "\u{f040}",
        "trash_path" => "\u{f1f8}",
        // 文档
        "read" => "\u{f0f6}",
        // 放大镜
        "glob" | "grep" | "search_knowledge_base" | "search_evicted_context" | "kb" => "\u{f002}",
        // 地球
        "web_search"
        | "web_fetch"
        | "search_web_images"
        | "archwiki_query"
        | "archlinux_news"
        | "check_issue"
        | "register_deep_research_reference"
        | "register_deep_research_topic_title"
        | "remove_deep_research_reference" => "\u{f0ac}",
        // 机器人：派出去的那个也是个"它"，不是一条连线。
        "subagent" | "task" | "deep_research" => "\u{f06a9}",
        // 眼睛：看图和"贴一张图"是两件事——它是在**读**。
        "vision_analyze" => "\u{f0208}",
        // 图片
        "print_image" | "generate_image" | "share_file" | "artifact" | "present_artifact" => {
            "\u{f03e}"
        }
        // 表情包单列：圆圈笑脸。它和"贴一张图"不是一回事——一眼看出是在发表情。
        "use_meme" | "manage_meme" => "\u{f118}",
        // 大脑
        "remember_fact" | "recall_memories" => "\u{f09d1}",
        // 清单
        "todowrite" | "goal" | "alarm" => "\u{f03a}",
        // 后台任务：列一列有哪些在跑（用户指名这个码位）。
        "job" => "\u{f0572}",
        // 计算器
        "ledger" | "manage_ledger" | "get_exchange_rate" | "query_api_quota" => "\u{f00ec}",
        // 包：装包、看包、查包全算一类。
        "install_aur_package"
        | "review_aur_package"
        | "aur"
        | "archlinux_official_package_query" => "\u{f487}",
        // 查看系统信息：CoreOS 那个圆里嵌核的标（用户指名「核心的那个」）。
        // 它原来跟装包挤在一类里——查机器和装包不是一回事。
        "check_os_info" => "\u{f305}",
        // 问号
        "ask_question" => "\u{f128}",
        // 魔杖
        // 终端：脚本是"一段能跑的东西"。
        "manage_script" => "\u{f489}",
        // 文档：技能和工具清单都是"一份说明"，装上才有用。
        "manage_skill" | "load_skill" | "load_tools" => "\u{f4a5}",
        _ => "\u{f4bc}",
    }
}

/// 从工具统计里摘出来、还没成形的一步。
struct PendingStep {
    name: String,
    display: String,
    peek: Option<String>,
    /// 子代理烧了多少（短标）。见 `StreamRenderer::subagent_tokens_label`。
    tokens: Option<String>,
    detail: Vec<String>,
    /// 抬头底下留着的那几行。见 [`Step::tail`]。
    tail: Vec<String>,
    failed: bool,
    /// 收进来的时候还没跑完——只有回合被打断（Ctrl+C、断线）才会这样。
    interrupted: bool,
    elapsed: Option<Duration>,
    overlay: Option<u64>,
}

/// 一步：折叠时的那一行，加上点开能看到的正文。
pub(crate) struct Step {
    line: String,
    body: Vec<String>,
    /// 子代理：点开是覆盖层而不是就地展开，用的是它自己那块流水账的 id。
    overlay: Option<u64>,
    /// 就地展开那一块的 id。**收进时间线那一刻就登记**，live 区和收缩之后用的是
    /// 同一个 id——原来只有收成 `Worked for …` 时才登记，于是回合还没结束时已经
    /// 跑完的那几步一个都点不开（用户实测：diff 要等 AI 输出完才看得到）。
    block: Option<u64>,
    /// 这一步是它说的一段正文（子代理面板里），不是动作：没有抬头，也不连线，
    /// `body` 就是整段话。按时序占位，后面再想再动手也排不到它前头。
    speech: bool,
    /// 收缩行：`body` 是收起来的那几步（各自已经是整行、带缩进、连好线），点开时
    /// 不再缩进——和主线 `Worked for …` 展开成时间线一个样子。
    fold: bool,
    /// 不点开也露在抬头底下的那几行（跑完的命令留着的输出尾巴，连线从它们中间
    /// 穿过去）。块的结束标记放在它们之后：点开时展开内容把抬头和尾巴一起换掉。
    tail: Vec<String>,
}

impl Step {
    fn new(line: String, body: Vec<String>, overlay: Option<u64>) -> Self {
        Self {
            line,
            body,
            overlay,
            block: None,
            speech: false,
            fold: false,
            tail: Vec::new(),
        }
    }

    fn speech(body: Vec<String>) -> Self {
        Self {
            line: String::new(),
            body,
            overlay: None,
            block: None,
            speech: true,
            fold: false,
            tail: Vec::new(),
        }
    }
}

/// 抬头后面带上耗时：`已思考 · 2.6s`。不到十分之一秒的不带——`0.0s` 只是噪音
///（用户实测）。
pub(crate) fn timed_label(head: &str, elapsed: Duration) -> String {
    match reported_seconds(elapsed) {
        Some(secs) => format!("{head} · {secs}"),
        None => head.to_string(),
    }
}

/// 值得报出来的耗时：至少十分之一秒。
pub(crate) fn reported_seconds(elapsed: Duration) -> Option<String> {
    (elapsed.as_millis() >= 100).then(|| format_seconds(elapsed))
}

/// 面板里「正在进行」那一行左边距上的转轮占位格。
///
/// 面板内容是一段静态 ANSI，没人每帧重写它；画面板的那一层每一帧把这个格子换成
/// 当帧的点阵字形，转轮就转起来了。选私有区末尾的码位：不会和任何文字撞上，
/// 宽度也是一格。
pub(crate) const LIVE_SPINNER_CELL: char = '\u{10FFFD}';

/// 面板里正在进行的那一行：转轮占位在第 0 列，logo 留在第 2 列——和主线一样。
pub(crate) fn panel_live_step_line(glyph: &str, text: &str) -> String {
    let text = crate::render::clip_to_display_width(text, panel_step_width());
    format!("\x1b[2m{LIVE_SPINNER_CELL} {glyph} {text}\x1b[0m")
}

/// live 区里「正在进行」的一行。
#[derive(Debug)]
pub(crate) struct LiveRow {
    pub(crate) line: String,
    /// 点开去哪儿：就地那一块，或者子代理的面板。
    pub(crate) target: Option<u64>,
    /// 这一行底下跟着露出来的几行（静态时间线里跑着的命令露出来的输出尾巴）。
    pub(crate) tail: Vec<String>,
}

/// 子代理面板最多留多少步。
const SUBAGENT_LOG_STEPS: usize = 400;

/// 「差事」那一步的图标（文档）。与后台任务面板里那条同一个，见
/// `cli::repl::tail::screen::overlay::PROMPT_GLYPH`。
const PROMPT_GLYPH: &str = "\u{f4a5}";

/// 一段话的开头，给抬头用。
fn peek_head(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::render::clip_to_display_width(&flat, max)
}

/// 把攒着的那段思考结算成一步。
fn flush_subagent_thought(log: &mut SubagentLog) {
    let text = std::mem::take(&mut log.reasoning);
    let elapsed = log
        .reasoning_since
        .take()
        .map(|at| at.elapsed())
        .unwrap_or_default();
    if text.trim().is_empty() {
        return;
    }
    let body = wrap_detail(&text)
        .into_iter()
        .map(|line| format!("\x1b[2m\x1b[38;5;10m{line}\x1b[0m"))
        .collect::<Vec<_>>();
    log.segment.thoughts += 1;
    log.segment.note_start_since(elapsed);
    log.steps.push(Step::new(
        step_line_in(
            glyph_think(),
            &timed_label(t("thought", "已思考"), elapsed),
            panel_step_width(),
        ),
        body,
        None,
    ));
    trim_subagent(log);
}

/// 它刚才说的那段话封成一步，钉在此刻的位置上。
///
/// 正文原来一直挂在面板最底下（`log.speech`）——说完一段再接着想、接着动手时，
/// 新的「思考中」和后面的步骤都排到了它**上面**，看着像它还没开口就先想了下一
/// 步（用户实测截图）。说过的话是时间线上的一件事，按时序占位；只有**还在说**
/// 的那段才留在底下。
fn seal_subagent_speech(log: &mut SubagentLog) {
    let text = std::mem::take(&mut log.speech);
    if text.trim().is_empty() {
        return;
    }
    log.steps.push(Step::speech(render_speech_lines(
        text.trim(),
        detail_width(),
    )));
    trim_subagent(log);
}

/// 面板里它说的正文：先过一遍 markdown，再按宽度折行。原来是裸文本——`**加粗**`
/// 的星号、反引号原样露着（用户实测截图：浮层里正文没有 md 渲染）。主线正文走的
/// 是同一套行渲染器，两边长相才一致。
pub(crate) fn render_speech_lines(text: &str, width: usize) -> Vec<String> {
    let mut renderer = crate::render::MarkdownLineRenderer::new();
    let mut rendered = String::new();
    for line in text.lines() {
        let piece = renderer.render_line(line);
        if piece.is_empty() {
            continue;
        }
        rendered.push_str(&piece);
        if !piece.ends_with('\n') {
            rendered.push('\n');
        }
    }
    let rest = renderer.flush();
    rendered.push_str(&rest);
    rendered
        .lines()
        .flat_map(|line| {
            if line.trim().is_empty() {
                return vec![String::new()];
            }
            crate::render::wrap_display_text(line, width)
        })
        .collect()
}

/// 这一段过程从第几步开始：「提示词」那一行和已经说过的正文都不算过程。
fn segment_start(log: &SubagentLog) -> usize {
    log.steps
        .iter()
        .rposition(|step| step.speech)
        .map(|index| index + 1)
        .unwrap_or(usize::from(log.has_prompt))
}

/// 把面板里已经走完的那几步收成一行 `⌄ Worked for …`，点开还是那几步。
///
/// 「提示词」那一行钉在最前面不参与收缩——它说的是"要干什么"，不是过程。
/// 说过的正文也不收：它是产出，收缩的是产出之前的过程。
fn collapse_subagent_segment(log: &mut SubagentLog) {
    let from = segment_start(log);
    if log.steps.len() <= from + 1 {
        // 一步（或没有）就不值得收：收完那一行比原来还长。
        log.segment = Timeline::default();
        return;
    }
    let counts = Counts {
        tools: log.segment.tools,
        thoughts: log.segment.thoughts,
        errors: log.segment.errors,
    };
    let summary = summary_line(log.segment.elapsed(), counts);
    let collapsed: Vec<Step> = log.steps.drain(from..).collect();
    // 收起来的每一步**还是块**：点开收缩行看到的是时间线，时间线里每一步再点开
    // 才是它的正文。原来只把抬头串起来，工具输出、思考全文在收缩那一刻就没了
    //（用户实测：浮层中的收缩行为异常，会丢失内容）。这些步已经跑完，内容不会
    // 再变，登记一次就够。
    let body: Vec<String> = thread(collapsed.into_iter().map(|step| {
        let line = step.line.clone();
        if step.body.is_empty() {
            return line;
        }
        match blocks::register(step_detail(&step)) {
            Some(id) => format!("{}{line}{}", blocks::begin_marker(id), blocks::END_MARKER),
            None => line,
        }
    }));
    log.step_blocks.truncate(from);
    // 收缩行点开是时间线（`fold`）：抬头、连线、各步同一列，不再当正文缩进。
    let mut fold = Step::new(
        step_line_in(FOLD_GLYPH_CLOSED, &summary, panel_step_width()),
        body,
        None,
    );
    fold.fold = true;
    log.steps.push(fold);
    log.segment = Timeline::default();
}

/// 收缩行的图标。和主线那条 `⌄ Worked for …` 一个样子。
const SUMMARY_GLYPH: &str = "⌄";

/// 收缩行**合着**的时候的图标：`› Worked for …`。点开之后（块内容的第一行）才是
/// `⌄`——主线那条就是这么翻的，面板里原来一直是 `⌄`，合着开着一个样
///（用户实测：Worked for 左侧箭头异常）。
const FOLD_GLYPH_CLOSED: &str = "›";

pub(crate) fn fold_glyph_closed() -> &'static str {
    FOLD_GLYPH_CLOSED
}

/// 收缩行点开之后的抬头：`›` 换成 `⌄`。
pub(crate) fn fold_line_open(line: &str) -> String {
    line.replacen(FOLD_GLYPH_CLOSED, SUMMARY_GLYPH, 1)
}

fn trim_subagent(log: &mut SubagentLog) {
    if log.steps.len() > SUBAGENT_LOG_STEPS {
        let excess = log.steps.len() - SUBAGENT_LOG_STEPS;
        // 「差事」那一行钉在最前面：它是整块面板里唯一说得清"在干什么"的一行，
        // 被挤掉之后剩下的全是过程。
        let from = usize::from(log.has_prompt);
        log.steps.drain(from..from + excess);
        // 块 id 是按位置对齐的，步挪了它也得跟着挪。
        let end = (from + excess).min(log.step_blocks.len());
        if end > from {
            log.step_blocks.drain(from..end);
        }
    }
}

/// 面板标题栏：名字 + 跑了多久（还没动静时说一声，免得看着像死的）。
fn subagent_title(log: &SubagentLog, display: &str) -> String {
    let elapsed = log.started.map(|at| at.elapsed()).unwrap_or_default();
    let mut title = format!("{display} · {}", format_seconds(elapsed));
    // 烧了多少、跑了几个工具：一个子代理可能跑几分钟，标题上没有量就只剩
    // 「running」，看不出它是在干活还是卡住了。
    if let Some(stats) = log.stats.as_deref().filter(|text| !text.trim().is_empty()) {
        title.push_str(" · ");
        title.push_str(stats.trim());
    }
    if log.steps.is_empty() && log.reasoning.trim().is_empty() {
        title.push_str(&format!(" · {}", t("starting…", "启动中…")));
    }
    title
}

/// 子代理面板的内容：串起来的时间线，每一步各自包成块（面板里也能点开）。
///
/// 块 id **按位置复用**。这条路每收到一小段思考就要走一遍（一秒好几次），每次
/// 都新登记一批的话，登记处几秒就被刷爆——而淘汰是按 id 从小到大来的，最先被
/// 端掉的正是这个子代理自己那块覆盖层（它登记得最早）。表现出来就是"面板里不
/// 是流式刷新的"和"工具行点不开了"（用户实测）。后台任务面板那边早就是这么做
/// 的，这里漏了。
/// 面板里的一项：一步（要连线、可点开）、一段它说的话（整段照排），或者最前面
/// 那行「提示词」抬头（可点开，但不在时间线上：和第一步之间不连线、空一行）。
enum PanelEntry {
    Step(String),
    Text(Vec<String>),
    Header(String),
}

/// 把面板里的各项排成行：步与步之间连线，正文段上下各空一行、不连线。
fn thread_panel(entries: Vec<PanelEntry>) -> Vec<String> {
    let indent = indent();
    let mut lines = Vec::new();
    let mut previous_was_step = false;
    let mut previous_was_text = false;
    for entry in entries {
        match entry {
            PanelEntry::Header(line) => {
                lines.push(line);
                // 当作"前面是一段正文"：下一步之前空一行、不连线。
                previous_was_step = false;
                previous_was_text = true;
            }
            PanelEntry::Step(line) => {
                if previous_was_step {
                    lines.push(rail());
                } else if previous_was_text {
                    lines.push(String::new());
                }
                lines.push(line);
                previous_was_step = true;
                previous_was_text = false;
            }
            PanelEntry::Text(body) => {
                lines.push(String::new());
                lines.extend(body.into_iter().map(|line| format!("{indent}{line}")));
                previous_was_step = false;
                previous_was_text = true;
            }
        }
    }
    lines
}

fn subagent_lines(log: &mut SubagentLog) -> Vec<String> {
    if log.step_blocks.len() > log.steps.len() {
        // 步被从前面裁过，位置对不上了，重来一轮。
        log.step_blocks.clear();
    }
    let mut entries = Vec::with_capacity(log.steps.len() + 2);
    for (index, step) in log.steps.iter().enumerate() {
        if step.speech {
            entries.push(PanelEntry::Text(step.body.clone()));
            continue;
        }
        if step.body.is_empty() {
            entries.push(PanelEntry::Step(step.line.clone()));
            continue;
        }
        let detail = step_detail(step);
        let id = match log.step_blocks.get(index).copied() {
            Some(id) => {
                blocks::update(id, String::new(), detail);
                Some(id)
            }
            None => {
                let id = blocks::register(detail);
                if let Some(id) = id {
                    // 位置要对齐：正文为空的那些步不登记，用 0 占位。
                    while log.step_blocks.len() < index {
                        log.step_blocks.push(0);
                    }
                    log.step_blocks.push(id);
                }
                id
            }
        };
        let line = match id.filter(|id| *id != 0) {
            Some(id) => format!(
                "{}{}{}",
                blocks::begin_marker(id),
                step.line,
                blocks::END_MARKER
            ),
            None => step.line.clone(),
        };
        // 「提示词」是抬头，不进时间线（用户：提示词 tag 行可以不参与 timeline）。
        if index == 0 && log.has_prompt {
            entries.push(PanelEntry::Header(line));
        } else {
            entries.push(PanelEntry::Step(line));
        }
    }
    // 正在跑的内层工具 / 正在流参数的那一个，各露一行——和主线的 live 区一个
    // 规矩。面板不归转轮管（它按块版本刷新），所以这两行是静态文字。
    if let Some((glyph, display, peek, since)) = &log.running {
        let mut label = format!(
            "{display} · {} · {}",
            t("running", "运行中"),
            format_seconds(since.elapsed())
        );
        if let Some(peek) = peek {
            label.push_str(PEEK_SEP);
            label.push_str(peek);
        }
        entries.push(PanelEntry::Step(panel_live_step_line(glyph, &label)));
    } else if let Some((phase, since)) = &log.preparing {
        entries.push(PanelEntry::Step(panel_live_step_line(
            glyph_tool(),
            &format!("{phase} · {}", format_seconds(since.elapsed())),
        )));
    }
    // 还在想的那一段也露一行，不然「正在思考」期间面板看着是死的。
    //
    // 这一行**也要能点开**：它常常是面板里最下面那一行，而正在想什么恰恰是
    // 此刻最值得看的（用户实测：浮层内最下面一行无法交互）。块 id 存在
    // `live_block` 里复用，每刷新一次只更新内容。
    if !log.reasoning.trim().is_empty() {
        let line = panel_live_step_line(
            glyph_think(),
            &format!(
                "{}{PEEK_SEP}{}",
                t("thinking", "思考中"),
                peek_tail(&log.reasoning, panel_step_width())
            ),
        );
        let detail =
            {
                let indent = indent();
                let mut lines = vec![line.clone(), String::new()];
                lines.extend(wrap_detail(&log.reasoning).into_iter().map(|piece| {
                    format!("\x1b[2m\x1b[38;5;10m{indent}{DETAIL_INDENT}{piece}\x1b[0m")
                }));
                lines.push(String::new());
                lines
            };
        let id = match log.live_block {
            Some(id) => {
                blocks::update(id, String::new(), detail);
                Some(id)
            }
            None => {
                let id = blocks::register(detail);
                log.live_block = id;
                id
            }
        };
        entries.push(PanelEntry::Step(match id {
            Some(id) => format!("{}{line}{}", blocks::begin_marker(id), blocks::END_MARKER),
            None => line,
        }));
    }
    // 还在说的那段话排在最底下——它是此刻正在发生的事。说完的会被
    // `seal_subagent_speech` 封成一步，按时序留在该在的位置上。
    if !log.speech.trim().is_empty() {
        entries.push(PanelEntry::Text(render_speech_lines(
            log.speech.trim(),
            detail_width(),
        )));
    }
    thread_panel(entries)
}

/// 一个子代理的内层时间线。
///
/// 形状和主线一模一样——子代理干的事和主体是同一类事，没有理由换一套看法。
#[derive(Default)]
pub(crate) struct SubagentLog {
    pub(crate) id: Option<u64>,
    /// 最近一次统计（工具次数 / 词元估算）。挂在面板标题上。
    stats: Option<String>,
    /// 同一份量的短标（`≈3.1K`）。挂在**时间线那一行**上——不点开就想知道
    /// 它烧了多少（用户实测：前台子代理完成后没有显示 token 消耗）。
    tokens: Option<String>,
    /// 跑完了没有。跑完的那个不再开窗——它已经收成主线时间线里的一步了。
    finished: bool,
    steps: Vec<Step>,
    /// 正在累积的思考。下一步工具落下时（或收尾时）结算成一步。
    reasoning: String,
    /// 这一段思考是什么时候开始的。
    reasoning_since: Option<Instant>,
    /// 正在跑的那个子工具是什么时候开始的。内层事件本身不带耗时，只能自己掐表。
    tool_since: Option<Instant>,
    started: Option<Instant>,
    /// 第一步是不是「差事」那一行。见 [`StreamRenderer::subagent_prompt`]。
    has_prompt: bool,
    /// 每一步对应的块 id，按位置复用（0 表示这一步没有正文、不登记）。
    /// 见 [`subagent_lines`]。
    step_blocks: Vec<u64>,
    /// 「思考中」那一行的块 id。同样复用，见 [`subagent_lines`]。
    live_block: Option<u64>,
    /// 内层正在流工具参数：`准备编辑 · 1.2s`。主线有这一行，面板里原来没有
    ///（用户实测：浮层中没有「准备xx」系列输出）。
    preparing: Option<(&'static str, Instant)>,
    /// 内层正在跑的工具：`(图标, 名字, 窥视, 起点)`。原来调用发出到结果回来
    /// 之间面板里什么都没有，看着像卡住了。
    running: Option<(&'static str, String, Option<String>, Instant)>,
    /// 它正在说的那段正文。见 [`StreamRenderer::subagent_content`]。
    speech: String,
    /// 这一段过程的计数与起点，收成 `Worked for …` 那一行时要用。
    segment: Timeline,
}

/// 一段连续的过程。
#[derive(Default)]
pub(crate) struct Timeline {
    started: Option<Instant>,
    /// 这一段里每一步**自己**花掉的时间之和。见 [`Timeline::elapsed`]。
    spent: Duration,
    steps: Vec<Step>,
    /// 静态时间线：前多少步已经落进 scrollback 了。live 区只画这之后的。
    /// 全屏下一直是 0——那儿整段都留在 live 区里，收缩时一起写。
    committed: usize,
    tools: usize,
    thoughts: usize,
    errors: usize,
}

impl Timeline {
    fn note_start(&mut self) {
        self.note_start_since(Duration::ZERO);
    }

    /// 记下这一段过程的起点，`spent` 是这一步**自己**已经花掉的时间。
    ///
    /// 起点不是"这一步被收进来的那一刻"——步是跑完才收的，两者正好差出这一步的
    /// 耗时。一轮只想了一次就交卷时，这个差就是整段思考，摘要于是报出刺眼的
    /// `Worked for 0.0s`：看着像这一轮瞬间就完了。
    fn note_start_since(&mut self, spent: Duration) {
        let at = Instant::now()
            .checked_sub(spent)
            .unwrap_or_else(Instant::now);
        if self.started.is_none_or(|existing| at < existing) {
            self.started = Some(at);
        }
        self.spent = self.spent.saturating_add(spent);
    }

    fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// 这一段过程花了多久。
    ///
    /// 平时就是墙上时间。**回放时墙上时间是零**——整段是一瞬间喂完的，于是
    /// `Worked for` 那一截整个消失，重开之后只剩 `1 tool · 2 thoughts`
    ///（用户实测对比图）。回放时每一步自己带着耗时，累加起来就是这一段的下限，
    /// 取两者的大者：实时不受影响，回放拿得回那个数。
    fn elapsed(&self) -> Duration {
        let wall = self.started.map(|at| at.elapsed()).unwrap_or_default();
        wall.max(self.spent)
    }
}

/// 秒数。亚秒给一位小数（`0.3s`），进了分钟就换成 `1m 02s`——
/// 「跑了多久」这件事在不同量级上关心的精度不一样。
pub(crate) fn format_seconds(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 60.0 {
        format!("{:.0}s", secs)
    } else {
        let whole = elapsed.as_secs();
        format!("{}m {:02}s", whole / 60, whole % 60)
    }
}

/// 命令的单行窥视：取第一条有内容的行，截到能放下。
pub(crate) fn command_peek(arguments: &str) -> Option<String> {
    let command = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get("command")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| arguments.to_string());
    let line = command.lines().find(|line| !line.trim().is_empty())?;
    Some(crate::render::clip_to_display_width(line.trim(), 72))
}

/// 工具输出切成可展开的行。
///
/// JSON 先排版再给——工具的返回十有八九是一长串 JSON，原样贴出来是一行糊到
/// 屏幕外的字符汤，点开等于没点。整段走暗色：这是"想看再看"的附注，不该和
/// 正文抢注意力。
pub(crate) fn tool_output_lines(output: &str) -> Vec<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let formatted = crate::render::format_tool_payload(trimmed);
    // 先折行再上色：反过来的话折行要拆 ANSI，切在转义序列中间就是乱码。
    wrap_detail(&formatted)
        .into_iter()
        .take(MAX_DETAIL_LINES)
        .map(|line| format!("\x1b[2m{line}\x1b[0m"))
        .collect()
}

/// 一步的详情最多留多少行。再多就不是「点开看看」而是把内存当日志用了。
const MAX_DETAIL_LINES: usize = 400;

/// 全屏下跑着的命令在抬头底下露几行输出；跑完之后这几行留着（用户拍板六行，
/// 完成后保留区域）。
pub(crate) const LIVE_PREVIEW_ROWS: usize = 6;

/// 展开内容相对页边距再缩进多少。
const DETAIL_INDENT: &str = "  ";

/// 展开内容能用多宽。
///
/// **折行得自己折**：交给缓冲硬折的话，续行从第 0 列开始，冒到页边距外面去，
/// 看着就是"左边莫名其妙多出半个字"。所以内容在这儿就按这个宽度折好，
/// 每一行都自带缩进。
pub(crate) fn detail_width() -> usize {
    crate::render::command_terminal_width()
        .saturating_sub(indent().len() + DETAIL_INDENT.len() + 1)
        .max(20)
}

/// 去掉 inline 那套从属装饰（`↳` / `│`）。
///
/// 时间线已经用连线表达了"这几行属于上面那一步"，再套一层箭头和竖条就是同一件事
/// 说两遍；两套缩进还对不齐，看着就是乱的。用户原话：「命令展开内容的对齐有问题，
/// 我觉得是不是没必要有那个箭头和竖线」。
///
/// 行首的转义序列（颜色）和空白（缩进）原样留着，只摘掉那一个记号。
pub(crate) fn undecorate(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| {
            let mut head = String::new();
            let mut rest = line.as_str();
            loop {
                if let Some(len) = crate::render::escape_len(rest) {
                    head.push_str(&rest[..len]);
                    rest = &rest[len..];
                    continue;
                }
                match rest.strip_prefix(' ') {
                    Some(tail) => {
                        head.push(' ');
                        rest = tail;
                    }
                    None => break,
                }
            }
            for marker in ["↳ ", "│ ", "↳", "│"] {
                if let Some(tail) = rest.strip_prefix(marker) {
                    rest = tail;
                    break;
                }
            }
            head.push_str(rest);
            head
        })
        .collect()
}

/// 一段纯文本按 `detail_width()` 折行。返回的每一行都**没有**缩进
/// （缩进由 `step_detail` 统一加，免得两处各加一次）。
pub(crate) fn wrap_detail(text: &str) -> Vec<String> {
    let width = detail_width();
    text.lines()
        .flat_map(|line| {
            if line.trim().is_empty() {
                return vec![String::new()];
            }
            crate::render::wrap_display_text(line, width)
        })
        .collect()
}

/// 正文的左边距。
///
/// 时间线的节点在第 2 列，用户消息的竖条在第 0 列、文字在第 2 列——正文贴着第 0
/// 列的话整屏只有它一条不在同一条基准线上。缩进两格，左边就有了一条统一的
/// 装订边。
pub(crate) fn indent_body(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let width = body_width();
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = text;
    loop {
        let (line, tail) = match rest.find('\n') {
            Some(position) => (&rest[..position], Some(&rest[position + 1..])),
            None => (rest, None),
        };
        push_wrapped(&mut out, line, width);
        match tail {
            Some(tail) => {
                out.push('\n');
                if tail.is_empty() {
                    break;
                }
                rest = tail;
            }
            None => break,
        }
    }
    out
}

/// 正文能用多宽。
///
/// 就是 `command_terminal_width()` 本身，不再减缩进：那个数已经把左右两条边距
/// 都刨掉了，表格、代码块也都是按它排的。这儿再减一次，就会把刚好排满的表格
/// 又折一道——比不折还难看。
fn body_width() -> usize {
    crate::render::command_terminal_width().max(20)
}

/// 一行正文：先按正文宽折好，每一折都自带缩进。
///
/// **折行得自己折**。交给缓冲硬折的话续行从第 0 列开始，装订边在那一行断掉，
/// 看着就是"左边莫名其妙冒出半句话"——用户报的「严重的软换行问题」就是这个。
fn push_wrapped(out: &mut String, line: &str, width: usize) {
    // 行尾的 `\r` 不算内容，折完再补回去（图片占位行走的是 `\r\n`）。
    let (line, carriage) = match line.strip_suffix('\r') {
        Some(stripped) => (stripped, true),
        None => (line, false),
    };
    if line.is_empty() {
        if carriage {
            out.push('\r');
        }
        return;
    }
    // 带图形传输段的行一个字都不折。宽度是按转义序列跳过算的（`escape_len`
    // 现在认 APC），理论上不会折到它头上——但折错一次的代价是整张图不出来，
    // 这条便宜的保险值得留着。
    if line.contains("\x1b_G") {
        out.push_str(INDENT);
        out.push_str(line);
        if carriage {
            out.push('\r');
        }
        return;
    }
    for (index, piece) in crate::render::wrap_display_text(line, width)
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(INDENT);
        out.push_str(&piece);
    }
    if carriage {
        out.push('\r');
    }
}

/// 一步展开后的样子：头行 + 空行 + 缩进的正文 + 空行。
///
/// 头行留着是因为它是把手（再点一次才收得回去）；上下各留一行空，否则展开的
/// 内容会和上下两步的连线糊成一片。
/// 时间线里一步占的那几行：抬头（挂着块的话包上标记），底下跟着它露出来的尾巴
///（跑完的命令留着的那几行输出，连线从中间穿过）。块的结束标记放在尾巴之后：
/// 点开时展开内容把抬头和尾巴**一起**换掉——和跑着的时候一个规矩。
fn step_rows(step: &Step, id: Option<u64>) -> String {
    let mut row = match id {
        Some(id) => format!("{}{}", blocks::begin_marker(id), step.line),
        None => step.line.clone(),
    };
    for extra in &step.tail {
        row.push('\n');
        row.push_str(&rail_prefix());
        row.push_str(extra);
    }
    if id.is_some() {
        row.push_str(blocks::END_MARKER);
    }
    row
}

fn step_detail(step: &Step) -> Vec<String> {
    let mut detail = Vec::with_capacity(step.body.len() + 3);
    if step.fold {
        // 收缩行点开是时间线：抬头（`›` 翻成 `⌄`）、连线、各步同一列，不缩进
        // 不铺底（用户拿主线那份对比：「这个才是正确的」）。
        detail.push(fold_line_open(&step.line));
        detail.push(rail());
        detail.extend(step.body.iter().cloned());
        detail.push(String::new());
        return detail;
    }
    detail.push(step.line.clone());
    detail.extend(indented_body(&step.body));
    detail
}

/// 正文那几行：前后各一行空，每行缩进到竖线右边。
fn indented_body(body: &[String]) -> Vec<String> {
    let indent = indent();
    let mut lines = Vec::with_capacity(body.len() + 2);
    lines.push(String::new());
    lines.extend(body.iter().map(|line| {
        if line.trim().is_empty() {
            String::new()
        } else {
            format!("{indent}{DETAIL_INDENT}{line}")
        }
    }));
    lines.push(String::new());
    lines
}

/// 出错那一步：整行红色。
///
/// 只换图标不够——一屏暗色里多一个小记号根本扫不到，而"哪一步失败了"正是
/// 回头翻这条时间线时最想先看见的。
fn step_line_failed(glyph: &str, text: &str) -> String {
    step_line_failed_in(glyph, text, step_width())
}

fn step_line_failed_in(glyph: &str, text: &str, width: usize) -> String {
    let text = crate::render::clip_to_display_width(text, width);
    format!("\x1b[31m{}{glyph} {text}\x1b[0m", indent())
}

/// 一步：`  <glyph> <text>`。glyph 占的就是竖线那一列。
/// 抬头和窥视之间的分隔。
///
/// 原来是两个空格，和「名字 · 秒数」那半截的 `·` 不是一个写法，同一行上
/// 两种分隔读起来就是断句不齐（用户实测，指着「差事」和「已思考」两行说的）。
pub(crate) const PEEK_SEP: &str = " · ";

/// 时间线上一行能占多宽。
fn step_width() -> usize {
    crate::render::command_terminal_width()
        .saturating_sub(indent().len() + 3)
        .max(8)
}

/// 子代理面板里一步能占多宽。
///
/// 面板没有竖线，左右只各留两列——正好和正文那条装订边同宽，所以能占的宽度
/// 和主线时间线一样。留两列富余：图标是 Nerd Font 字形，某些终端把它算成两列。
/// 见 `cli::repl::tail::screen::overlay::panel_inner_width`。
fn panel_step_width() -> usize {
    step_width().saturating_sub(2).max(8)
}

fn step_line(glyph: &str, text: &str) -> String {
    step_line_in(glyph, text, step_width())
}

/// 整行裁到给定宽度：窥视可长可短，让它把一行挤成两行的话，时间线的竖线就
/// 对不上列了（续行从第 0 列开始）。
fn step_line_in(glyph: &str, text: &str, width: usize) -> String {
    let text = crate::render::clip_to_display_width(text, width);
    format!("\x1b[2m{}{glyph} {text}\x1b[0m", indent())
}

/// 面板里一步那一行。两种子代理面板（前台走事件、后台读日志）共用它——
/// 取数的地方不同，**长相必须是同一份代码**。
pub(crate) fn panel_step_line(glyph: &str, text: &str, failed: bool) -> String {
    if failed {
        step_line_failed_in(glyph, text, panel_step_width())
    } else {
        step_line_in(glyph, text, panel_step_width())
    }
}

/// 面板里两步之间的连线。见 [`panel_step_line`]。
pub(crate) fn panel_rail() -> String {
    rail()
}

/// 面板里一步点开之后是什么样。见 [`panel_step_line`]。
pub(crate) fn panel_step_detail(line: &str, body: &[String]) -> Vec<String> {
    let mut detail = Vec::with_capacity(body.len() + 3);
    detail.push(line.to_string());
    detail.extend(indented_body(body));
    detail
}

/// 面板里一步的**抬头**能占多宽：整行宽度减掉图标那一格和它后面的空格。
pub(crate) fn panel_step_width_for_head() -> usize {
    panel_step_width().saturating_sub(2)
}

/// 面板里正文那一层能用多宽（折行用）。
pub(crate) fn panel_detail_width() -> usize {
    detail_width()
}

/// 两步之间的连线：`  │`。
fn rail() -> String {
    format!("\x1b[2m{}{RAIL}\x1b[0m", indent())
}

/// 把若干步骤行用连线串起来。
fn thread(steps: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    for line in steps {
        if !out.is_empty() {
            out.push(rail());
        }
        out.push(line);
    }
    out
}

impl StreamRenderer {
    /// 有没有时间线：全屏的可展开版，或者普通终端里的静态版。
    ///
    /// `blocks::enabled()` 就是「全屏后端在驱动」的信号，不另设一个会和它走散
    /// 的开关。
    pub(crate) fn timeline_enabled(&self) -> bool {
        blocks::enabled() || self.timeline_static()
    }

    /// 静态时间线：不是全屏、但 stdout 是个终端（shellhook、单次 `miyu "…"`），
    /// 而且工具是按摘要档显示的。
    ///
    /// `live_summary` 就是「stdout 是终端」——管道里没有转轮也没有回翻，那儿
    /// 还是老的一行摘要。`Full` 档把每个工具的参数和输出整块打出来，本来就
    /// 不是给人扫一眼的，不改。
    pub(crate) fn timeline_static(&self) -> bool {
        !blocks::enabled()
            && self.live_summary
            && !self.plain
            && self.tool_call_mode == crate::render::ToolCallDisplayMode::Summary
    }

    /// 工具跑完了：收成一步。详情是那个工具的完整块。
    pub(crate) fn timeline_push_tools(&mut self) -> anyhow::Result<()> {
        if self.tool_stats.is_empty() {
            return Ok(());
        }
        let static_timeline = self.timeline_static();
        // 先收集再改：`ordered_tool_stats` 借着 `self`，循环里要往 `self.timeline`
        // 里写，借用检查过不去。
        let entries: Vec<PendingStep> = self
            .ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| PendingStep {
                name: name.to_string(),
                display: self.display_tool_name(name),
                // 子代理不给窥视：名字里已经带着描述了（`开发中·画鹅鹅骑车`），
                // 再把 `subject` 当窥视就是同一句话说两遍（用户截图实录）。
                peek: (!crate::render::is_subagent_tool(name))
                    .then(|| {
                        stats
                            .peek
                            .as_deref()
                            .or(stats.subject.as_deref())
                            .map(str::to_string)
                    })
                    .flatten(),
                tokens: self.subagent_tokens_label(name),
                // 真实输出优先：`tool_block_lines` 只是「跑没跑成」的统计，
                // 点开却看不到工具到底吐了什么，收起来就等于丢了。
                detail: if !stats.detail.is_empty() {
                    stats.detail.clone()
                } else if static_timeline {
                    // 静态版没有"点开"：主题那一行已经挂在抬头上了，统计那几行
                    // 就地印出来只是把时间线撑长。
                    Vec::new()
                } else {
                    // 丢掉 `tool_block_lines` 的表头（`名字×1 err` 那行）：
                    // 时间线那一步已经写了名字、耗时、成没成——展开之后又来一遍，
                    // 而且那一行的图标是通用齿轮、颜色也不是红的，看着像"展开之后
                    // 就不是报错的样子了"（用户原话）。
                    undecorate(
                        self.tool_block_lines(name, stats, false)
                            .into_iter()
                            .skip(1)
                            .collect(),
                    )
                },
                // 没跑完就被收进来 = 回合被打断了。它没成功，但也不是"报错"——
                // 抬头上要说清楚。
                tail: stats.tail.clone(),
                failed: stats.error > 0 || !stats.settled(),
                interrupted: !stats.settled(),
                // 不到十分之一秒的不报（`0.0s` 只是噪音）；交到后台的子代理是
                // 立刻返回的，它的秒数不是它干活的时间，也不报。
                elapsed: stats
                    .elapsed()
                    .filter(|elapsed| reported_seconds(*elapsed).is_some() && !stats.detached),
                overlay: crate::render::is_subagent_tool(name)
                    .then(|| self.subagent_overlay_id(name))
                    .flatten(),
            })
            .collect();
        // 这一批工具最早也是 `max(各自耗时)` 之前开始的——并发跑的话取最大值是
        // 唯一稳妥的下界，串行跑的话它也不会比真实起点晚太多。
        let spent = entries
            .iter()
            .filter_map(|entry| entry.elapsed)
            .max()
            .unwrap_or_default();
        self.timeline.note_start_since(spent);
        for PendingStep {
            name,
            display,
            peek,
            tokens,
            detail,
            failed,
            interrupted,
            elapsed,
            overlay,
            tail,
        } in entries
        {
            let glyph = if failed {
                glyph_err()
            } else {
                tool_glyph(&name)
            };
            // 先名字、再秒数，窥视挂最后——秒数是这一步的度量，窥视是内容，
            // 夹在中间读起来像是「运行命令 ls -la 花了 2.4 秒」的断句错位。
            let mut label = match (tokens.as_deref(), elapsed) {
                (Some(tokens), Some(elapsed)) => {
                    format!("{display} · {tokens} · {}", format_seconds(elapsed))
                }
                (Some(tokens), None) => format!("{display} · {tokens}"),
                (None, Some(elapsed)) => format!("{display} · {}", format_seconds(elapsed)),
                (None, None) => display,
            };
            if interrupted {
                label.push_str(" · ");
                label.push_str(t("interrupted", "已中断"));
            }
            if let Some(peek) = peek {
                label.push_str(PEEK_SEP);
                label.push_str(&peek);
            }
            if failed {
                self.timeline.errors += 1;
            }
            self.timeline.tools += 1;
            let line = if failed {
                step_line_failed(glyph, &label)
            } else {
                step_line(glyph, &label)
            };
            let mut step = Step::new(line, detail, overlay);
            step.tail = tail;
            self.timeline.steps.push(step);
        }
        self.tool_stats.clear();
        self.last_tool_summary.clear();
        self.live_block = None;
        self.live_tool_blocks.clear();
        self.settle_new_steps()
    }

    /// 刚收进来的那几步：全屏下登记成块（live 区里就能点开），静态版直接落进
    /// scrollback。
    fn settle_new_steps(&mut self) -> anyhow::Result<()> {
        if self.timeline_static() {
            return self.commit_static_steps();
        }
        for step in &mut self.timeline.steps {
            if step.block.is_none() && step.overlay.is_none() && !step.body.is_empty() {
                step.block = blocks::register(step_detail(step));
            }
        }
        Ok(())
    }

    /// 静态时间线：把还没落地的步骤写进 scrollback。
    ///
    /// 每一步跑完就落，live 区只留连线和正在跑的那一行——全屏那种"整段留在
    /// live 区里、结束时一起收"在这儿不成立：没有回翻、没有点击，一段 diff
    /// 留在每帧重画的 live 区里只会闪，而且超过一屏就擦不干净了。
    fn commit_static_steps(&mut self) -> anyhow::Result<()> {
        use std::io::Write as _;
        let from = self.timeline.committed;
        if from >= self.timeline.steps.len() {
            return Ok(());
        }
        // 转轮先收掉：它那几行还留在屏上的话，新落的步骤会写在它们中间。
        self.stop_waiting()?;
        let mut out = String::new();
        let prefix = rail_prefix();
        for (offset, step) in self.timeline.steps[from..].iter().enumerate() {
            if from + offset > 0 {
                out.push_str(&rail());
                out.push('\n');
            }
            out.push_str(&step.line);
            out.push('\n');
            // 正文紧贴抬头、每一行都从连线穿过，不空行——见 `rail_prefix`。
            for line in &step.body {
                out.push_str(&prefix);
                out.push_str(line);
                out.push('\n');
            }
        }
        let stdout = &mut self.output;
        write!(stdout, "{out}")?;
        stdout.flush()?;
        self.timeline.committed = self.timeline.steps.len();
        // 这一步干出来的结果（todo 表、图片占位）紧跟着它。
        self.flush_after_timeline()
    }

    /// 想完了：收成一步。详情是思考全文。
    /// 时间线上每一步那一行（测试用）：颜色也要能断言。
    #[cfg(test)]
    pub(crate) fn timeline_step_lines(&self) -> Vec<String> {
        self.timeline
            .steps
            .iter()
            .map(|step| step.line.clone())
            .collect()
    }

    pub(crate) fn timeline_push_thought(&mut self) -> anyhow::Result<()> {
        if self.reasoning_title.is_none() && self.reasoning_text.trim().is_empty() {
            return Ok(());
        }
        let elapsed = self
            .reasoning_elapsed
            .or_else(|| self.reasoning_started_at.map(|at| at.elapsed()))
            .unwrap_or_default();
        self.timeline.note_start_since(elapsed);
        let mut label = t("thought", "已思考").to_string();
        if self.reasoning_tokens > 0 {
            label = format!(
                "{label} · {} {}",
                self.reasoning_tokens,
                t("tokens", "词元")
            );
        }
        let label = timed_label(&label, elapsed);
        // 静态版没处点开，思考全文就不留了：抬头上的词元数和秒数说明"想过"，
        // 想了什么本来也只是折叠起来备查的。
        let detail = if self.timeline_static() {
            Vec::new()
        } else {
            wrap_detail(&self.reasoning_text)
                .into_iter()
                .map(|line| format!("\x1b[2m\x1b[38;5;10m{line}\x1b[0m"))
                .collect::<Vec<_>>()
        };
        self.timeline.thoughts += 1;
        self.timeline
            .steps
            .push(Step::new(step_line(glyph_think(), &label), detail, None));
        self.reasoning_text.clear();
        self.reasoning_tokens = 0;
        self.reasoning_title = None;
        self.reasoning_started_at = None;
        self.reasoning_elapsed = None;
        self.live_block = None;
        self.settle_new_steps()
    }

    /// 给等待转轮用的 live 画面：已完成的步骤照原样，后面接上**正在进行**的
    /// 那些（带转轮标记，由 `wait_spinner` 画上动画字形）。
    ///
    /// `current` 是一串，不是一条：并行派出去的几个子代理各占一行，各自点得开
    /// 自己的面板。压成一行加个 `+2` 的话，看着就像"只能跑一个"。
    pub(crate) fn timeline_live(&self, current: Vec<LiveRow>) -> (String, Option<String>) {
        // 已经跑完的那几步**也挂着块**：它们各自的 id 在收进来时就登记好了，
        // 收成 `Worked for …` 之后用的还是同一个，展开状态跟着走。
        let mut steps: Vec<String> = self.timeline.steps[self.timeline.committed..]
            .iter()
            .map(|step| step_rows(step, step.overlay.or(step.block)))
            .collect();
        let current_is_empty = current.is_empty();
        for LiveRow { line, target, tail } in current {
            // 正在跑的这一步**不带 logo**：点阵转轮会落在那一列上，跑完了
            // 收进 `steps` 时才换回静态图标。
            //
            // 外面再包一层块标记：正在想的时候也该点得开看到「想到哪儿了」，
            // 不必等它结束。标记不占显示宽度，转轮那边照常裁剪。
            // 跑着的是子代理时，点开该进**它的面板**，不是就地展开一段窥视——
            // 那条线还在长，就地展开的行数每秒都在变。
            let (open, close) = match target {
                Some(id) => (blocks::begin_marker(id), blocks::END_MARKER),
                None => (String::new(), ""),
            };
            // 转轮落在**左边距**那一列（第 0 列），logo 留在自己那一列——
            // 原来转轮顶掉 logo 的位置，跑完再换回来，一行两副面孔。
            //（用户：左边不是有一个边距吗，`<转轮><logo><抬头>` 就不用替代 logo 了。）
            let mut row = format!("{}{open}{line}", crate::render::wait_spinner::BLOCK_MARKER);
            // 底下跟着的几行（跑着的命令此刻的输出）和这一行是同一项：连线从
            // 它们中间穿过去。块的结束标记放在最后一行之后——点开的时候展开
            // 内容把抬头和这几行**一起**换掉（用户：如果展开命令的话就替换掉
            // 那个内容刷新行）。
            for extra in tail {
                row.push('\n');
                row.push_str(&rail_prefix());
                row.push_str(&extra);
            }
            row.push_str(close);
            steps.push(row);
        }
        // 什么都没在跑（上一个工具刚回来、下一次模型请求还在路上）：转轮独自
        // 落在 logo 那一列上。空着的话 live 区整个消失，等 `reasoning.start` 才
        // 回来——网络那一秒里整条时间线闪没了又闪回来。
        let lone = current_is_empty && (!steps.is_empty() || self.timeline.committed > 0);
        let mut lines = thread(steps);
        if lone {
            // 转轮在左边距、连线照常延续：`⠋ │`。
            lines.push(format!(
                "{}{RAIL}",
                crate::render::wait_spinner::BLOCK_MARKER
            ));
        }
        if lines.is_empty() {
            return (String::new(), None);
        }
        // 静态版：前面的步骤已经落进 scrollback 了，live 区从一根连线接上去。
        if self.timeline.committed > 0 {
            lines.insert(0, rail());
        }
        (String::new(), Some(lines.join("\n")))
    }

    /// 把「正在进行」那几行的可展开内容刷一遍。id 保持不变。
    ///
    /// **一个工具一块**。并行跑的时候，几行共用同一个 id 会让展开层把同一块内容
    /// 插好几遍——行号、偏移、点击命中全跟着错，表现出来就是"所有工具行都点不
    /// 开了"（用户实测）。
    pub(crate) fn refresh_live_block(&mut self) {
        // 静态版没有块：登记了也没人点。
        if !blocks::enabled() {
            return;
        }
        if !self.reasoning_text.trim().is_empty() || self.tool_stats.is_empty() {
            // 在想（或者什么都没跑）：还是那一块。
            let lines = self.live_block_lines();
            if lines.is_empty() {
                return;
            }
            match self.live_block {
                Some(id) => blocks::update(id, String::new(), lines),
                None => self.live_block = blocks::register(lines),
            }
            return;
        }
        let entries: Vec<(String, Vec<String>)> = self
            .ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| (name.clone(), self.live_tool_lines(name, stats)))
            .collect();
        for (name, lines) in entries {
            if lines.is_empty() {
                continue;
            }
            match self.live_tool_blocks.get(&name).copied() {
                Some(id) => blocks::update(id, String::new(), lines),
                None => {
                    if let Some(id) = blocks::register(lines) {
                        self.live_tool_blocks.insert(name, id);
                    }
                }
            }
        }
    }

    /// 跑着的某一个工具点开能看到什么。
    fn live_tool_lines(&self, name: &str, stats: &crate::render::ToolStats) -> Vec<String> {
        let indent = indent();
        let mut lines = vec![step_line(tool_glyph(name), &self.display_tool_name(name))];
        // 跑着的命令：完整命令 + 此刻为止的输出，每帧重算——展开着的那一块就
        // 跟着输出一起长。它的输出在 `command_display` 里，`stats` 上只有一句
        // 窥视（用户实测：命令展开后没有流式输出，展开内容居然是窥视行）。
        if crate::render::is_command_tool(name) {
            if let Some(display) = self.command_display.as_ref() {
                lines.extend(indented_body(&display.live_detail(detail_width())));
                return lines;
            }
        }
        // 工具已经吐出详情（比如编辑文件的 diff）就直接给：等这一段过程收完
        // 才看得到的话，"用了工具"和"看得到它改了什么"之间隔着整段回复
        //（用户实测：diff 是在 AI 正文回复结束之后才有）。
        if !stats.detail.is_empty() {
            lines.extend(indented_body(&stats.detail));
            return lines;
        }
        lines.push(String::new());
        if let Some(subject) = stats.peek.as_deref().or(stats.subject.as_deref()) {
            for piece in wrap_detail(subject) {
                lines.push(format!("\x1b[2m{indent}{DETAIL_INDENT}{piece}\x1b[0m"));
            }
        }
        if let Some(progress) = stats.progress.as_deref() {
            for line in progress.lines().filter(|line| !line.trim().is_empty()) {
                for piece in wrap_detail(line) {
                    lines.push(format!("\x1b[2m{indent}{DETAIL_INDENT}{piece}\x1b[0m"));
                }
            }
        }
        lines.push(String::new());
        lines
    }

    /// 正在进行那一步点开能看到什么：想到哪儿了 / 这个工具在忙什么。
    fn live_block_lines(&self) -> Vec<String> {
        let indent = indent();
        if !self.reasoning_text.trim().is_empty() {
            let mut lines = vec![
                step_line(
                    glyph_think(),
                    &format!("{} · {}", t("thinking", "思考中"), self.reasoning_tokens),
                ),
                String::new(),
            ];
            // `wrap_detail` 吐的是**没有缩进**的行——步那条路上缩进由
            // `step_detail` 统一加，而这儿是直接当块内容用的，得自己加。
            // 不加的话点开之后正文贴着第 0 列，比它的抬头还靠左。
            lines.extend(
                wrap_detail(&self.reasoning_text).into_iter().map(|line| {
                    format!("\x1b[2m\x1b[38;5;10m{indent}{DETAIL_INDENT}{line}\x1b[0m")
                }),
            );
            lines.push(String::new());
            return lines;
        }
        let mut lines = Vec::new();
        for (name, stats) in self.ordered_tool_stats() {
            lines.push(step_line(tool_glyph(name), &self.display_tool_name(name)));
            if let Some(subject) = stats.peek.as_deref().or(stats.subject.as_deref()) {
                lines.push(format!("    \x1b[2m{subject}\x1b[0m"));
            }
            if let Some(progress) = stats.progress.as_deref() {
                lines.extend(
                    progress
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(|line| format!("    \x1b[2m{line}\x1b[0m")),
                );
            }
        }
        lines
    }

    /// live 区：已完成的步骤 + 正在做的那一件。
    pub(crate) fn timeline_waiting(&self) -> (String, Option<String>) {
        let width = crate::render::command_terminal_width();
        // 顺序即优先级：准备态 → 正在跑的工具 → 正在想。准备态排最前，
        // 因为它一定会被后面两者之一替换掉，本来就是个占位。
        let current: Vec<LiveRow> = if let Some((glyph, prepare)) = self.timeline_preparing_line() {
            vec![LiveRow {
                line: format!("{glyph} {prepare}"),
                target: self.live_block,
                tail: Vec::new(),
            }]
        } else if !self.tool_stats.is_empty() {
            self.timeline_running_tool_lines()
        } else if self.reasoning_started_at.is_some() {
            // 给窥视留下的宽度：整屏减掉「  │ ✳ 思考中 · 320 词元 · 7.5s  」
            vec![LiveRow {
                line: format!(
                    "{} {}",
                    glyph_think(),
                    self.timeline_thinking_line(width.saturating_sub(46).max(12))
                ),
                target: self.live_block,
                tail: Vec::new(),
            }]
        } else {
            Vec::new()
        };
        self.timeline_live(current)
    }

    /// 准备态那一行：`准备编辑 · 1.2s`。没有准备态就返回 `None`。
    fn timeline_preparing_line(&self) -> Option<(&'static str, String)> {
        if let Some(started_at) = self.preparing_question_started_at {
            return Some((
                tool_glyph("ask_question"),
                format!(
                    "{} · {}",
                    t("Preparing question", "准备问题"),
                    format_seconds(started_at.elapsed())
                ),
            ));
        }
        let (phase, started_at) = self.tool_preparing?;
        // 工具已经开跑了就不再报准备——那一行该让给真正的工具。
        if !self.tool_stats.is_empty() {
            return None;
        }
        Some((
            glyph_tool(),
            format!("{phase} · {}", format_seconds(started_at.elapsed())),
        ))
    }

    /// 正在跑的**每一个**工具各一行，连同它点开之后该去哪儿。
    ///
    /// 原来是把第一个拿出来、后面缀个 `+2`。并行派三个子代理时屏幕上就只有一行
    /// 在转，看着像"只能跑一个"——而它们确实在同时跑（用户问的就是这个）。
    pub(crate) fn timeline_running_tool_lines(&self) -> Vec<LiveRow> {
        let ordered = self.ordered_tool_stats();
        if ordered.is_empty() {
            return vec![LiveRow {
                line: format!("{} {}", glyph_tool(), t("running", "运行中")),
                target: self.live_block,
                tail: Vec::new(),
            }];
        }
        let static_timeline = self.timeline_static();
        ordered
            .into_iter()
            .map(|(name, stats)| {
                let display = self.display_tool_name(name);
                // 子代理那一行按「名字 · 烧了多少 · 跑了多久」写：量放在时间前面
                //（用户拍板），而且它每报一次就变，正好也是"还活着"的指示。
                let tokens = self.subagent_tokens_label(name);
                let mut line = match (tokens.as_deref(), stats.elapsed()) {
                    (Some(tokens), Some(elapsed)) => {
                        format!("{display} · {tokens} · {}", format_seconds(elapsed))
                    }
                    (Some(tokens), None) => format!("{display} · {tokens}"),
                    (None, Some(elapsed)) => format!("{display} · {}", format_seconds(elapsed)),
                    (None, None) => display,
                };
                // 窥视：命令文本 / 检索词。这一行是这个工具在时间线上唯一
                // 露出来的信息，不给窥视就只剩一个名字。
                //
                // 子代理特殊：名字里已经带着描述了（`开发中·审查 xxx`），再把
                // 描述当窥视就是同一句话说两遍。它该露的是**此刻在干什么**，
                // 一直刷新——这也是"它还活着"的唯一指示。想看全的点开进面板。
                let subagent = crate::render::is_subagent_tool(name);
                let peek = if subagent {
                    self.subagent_peek(name)
                } else {
                    stats
                        .peek
                        .as_deref()
                        .or(stats.subject.as_deref())
                        .map(str::to_string)
                };
                if let Some(peek) = peek {
                    line.push_str(PEEK_SEP);
                    line.push_str(&peek);
                }
                // 整行裁到屏宽——**必须裁**。
                //
                // 窥视是子代理内层的思考末尾，长度不受这儿控制；裁之前它能把
                // 这一行顶出屏幕，于是缓冲把它折成两行，块的起止就跨了行：
                // 点上去命中不到，整行变成死的（用户实测：开始刷新思考窥视
                // 之后就没法交互了）。`step_line` 一直是裁的，这条路漏了。
                let line = crate::render::clip_to_display_width(&line, step_width());
                // logo 留在自己那一列，转轮另落在左边距上。
                let line = format!("{} {line}", tool_glyph(name));
                // 子代理优先进面板；面板还没登记出来就先退回它自己那一块，
                // 别让这一行变成点不开的死行。
                let own = self.live_tool_blocks.get(name).copied();
                let target = if subagent {
                    self.subagent_overlay_id(name).or(own)
                } else {
                    own
                };
                // 跑着的命令把此刻的输出露在这一行底下。静态版没处点开，就地
                // 给几行（和它跑完之后落下来的那几行同一个量）；全屏给四行，
                // 超出的换成省略标记——想看全的点开，展开内容会把这几行一起换掉。
                let tail = if crate::render::is_command_tool(name) {
                    let rows = if static_timeline {
                        self.command_output_lines
                    } else {
                        LIVE_PREVIEW_ROWS
                    };
                    self.command_display
                        .as_ref()
                        .map(|display| display.live_tail(detail_width(), rows))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                LiveRow { line, target, tail }
            })
            .collect()
    }

    /// 跑着的那个子代理此刻在干什么，压成一行。
    ///
    /// 取它内层时间线的**最后一步**：正在想就露想到哪儿了，正在跑工具就露那个
    /// 工具，正在说话就露说到哪儿了。每帧都在变，一眼看得出它还活着。
    fn subagent_peek(&self, name: &str) -> Option<String> {
        let log = self.subagent_logs.get(name)?;
        let width = crate::render::command_terminal_width()
            .saturating_sub(48)
            .max(16);
        if !log.reasoning.trim().is_empty() {
            return Some(peek_tail(&log.reasoning, width));
        }
        if !log.speech.trim().is_empty() {
            return Some(peek_tail(&log.speech, width));
        }
        let last = log.steps.last()?;
        if last.speech {
            return Some(peek_tail(&last.body.join(" "), width));
        }
        // 步那一行自带缩进和颜色，窥视要的是干净的一句话。
        let text = crate::render::strip_ansi_text(&last.line);
        Some(peek_tail(text.trim(), width))
    }

    /// 正在想的那一行：`思考中 · N 词元 · Xs   <窥视>`。
    ///
    /// 窥视取思考正文的**末尾**一段并压成一行——想到哪儿了比想过什么更有用，
    /// 而且它每帧都在变，正好当作「还活着」的指示。
    pub(crate) fn timeline_thinking_line(&self, peek_width: usize) -> String {
        let elapsed = self
            .reasoning_started_at
            .map(|at| at.elapsed())
            .unwrap_or_default();
        let mut head = format!("{} · {}", t("thinking", "思考中"), format_seconds(elapsed));
        if self.reasoning_tokens > 0 {
            head = format!(
                "{} · {} {} · {}",
                t("thinking", "思考中"),
                self.reasoning_tokens,
                t("tokens", "词元"),
                format_seconds(elapsed)
            );
        }
        let peek = peek_tail(&self.reasoning_text, peek_width);
        if peek.is_empty() {
            head
        } else {
            format!("{head}{PEEK_SEP}{peek}")
        }
    }

    /// 提问也算一步。
    ///
    /// 问用户是这一轮真真切切做过的一件事，时间线里不该没有它——尤其是被取消
    /// 的那次：正文里什么都不留（那是对的），时间线里再不留就彻底查无此事了。
    pub(crate) fn timeline_push_question(
        &mut self,
        request: &crate::question::QuestionRequest,
        response: &crate::question::QuestionResponse,
    ) -> anyhow::Result<()> {
        use crate::question::QuestionResponse;
        if !self.timeline_enabled() {
            return Ok(());
        }
        let answered = matches!(response, QuestionResponse::Answered(_));
        // 面板开着的那段时间也算进这一段过程里：人在那儿看题、想答案，那就是
        // 这一轮真正花掉的时间。不算的话收缩行会写成光秃秃的 `1 tool`。
        let waited = self
            .preparing_question_started_at
            .map(|at| at.elapsed())
            .unwrap_or_default();
        self.timeline.note_start_since(waited);
        let headline = request
            .questions
            .first()
            .map(|prompt| prompt.header.trim().to_string())
            .filter(|header| !header.is_empty())
            .unwrap_or_else(|| t("question", "提问").to_string());
        let status = if answered {
            t("answered", "已回答")
        } else {
            t("cancelled", "已取消")
        };
        let label = format!(
            "{} · {status}{PEEK_SEP}{headline}",
            t("Ask the user", "询问用户")
        );
        let body = request
            .questions
            .iter()
            .flat_map(|prompt| {
                let mut lines = wrap_detail(prompt.question.trim());
                lines.push(String::new());
                lines
            })
            .collect::<Vec<_>>();
        self.timeline.tools += 1;
        if !answered {
            self.timeline.errors += 1;
        }
        let glyph = tool_glyph("ask_question");
        // 静态版：面板退场时不留它自己那块「已回答」（那块带自己的竖条，落在
        // 抬头**上面**，和时间线是两套东西——用户实测截图）。一问一答改成这一步
        // 的正文，从连线穿过去，和别的步一个样子：
        //
        // ```text
        //    询问用户 · 已回答 · 今晚的打算
        //   │
        //   │ 已回答 3 个问题
        //   │ 今晚的打算：只是测工具（推荐）
        //   │
        // ```
        let body = if self.timeline_static() {
            match response {
                QuestionResponse::Answered(answers) => {
                    let mut lines = vec![
                        String::new(),
                        format!(
                            "\x1b[2m{} {} {}\x1b[0m",
                            t("Answered", "已回答"),
                            request.questions.len(),
                            t("questions", "个问题")
                        ),
                    ];
                    for (prompt, selected) in request.questions.iter().zip(answers) {
                        let line = format!(
                            "{}：{}",
                            prompt.header.trim(),
                            selected.join("、").replace('\n', " ")
                        );
                        lines.extend(
                            wrap_detail(&line)
                                .into_iter()
                                .map(|piece| format!("\x1b[2m{piece}\x1b[0m")),
                        );
                    }
                    // 收尾不再空一行：下一步之前本来就有一根连线，两根叠着就是
                    // 两行 `│`。
                    lines
                }
                _ => Vec::new(),
            }
        } else {
            body
        };
        self.timeline.steps.push(Step::new(
            if answered {
                step_line(glyph, &label)
            } else {
                step_line_failed(glyph, &label)
            },
            body,
            None,
        ));
        self.settle_new_steps()
    }

    /// 把一次提问与它的答案写进正文。
    ///
    /// 照搬 inline 那份的样子：一条暗竖条 + 「已回答 N 个问题」+ 每题一行
    /// 「标题：答案」。一问一答各占一行的写法（`? …` / `↳ …`）在屏幕上散成
    /// 一片，而这份本来就是给"扫一眼当时选了什么"用的。
    pub(crate) fn write_question_exchange(
        &mut self,
        request: &crate::question::QuestionRequest,
        response: &crate::question::QuestionResponse,
    ) -> anyhow::Result<()> {
        use crate::question::QuestionResponse;
        use std::io::Write as _;
        // 只有全屏需要：面板是盖上去的，退场就没了。普通终端里提问面板自己
        // 会把「已回答」那几行留在原地，再写一遍就是两份。
        if !blocks::enabled() {
            return Ok(());
        }
        // 取消/关闭没产生任何**内容**：它只是"这一下没成"。往正文里逐题写一遍
        // 「已取消」，等于把一次误触变成永久的一屏垃圾。
        let QuestionResponse::Answered(answers) = response else {
            return Ok(());
        };
        self.stop_waiting()?;
        let indent = indent();
        let bar = "\x1b[2m\x1b[90m┃\x1b[0m";
        let width = crate::render::command_terminal_width()
            .saturating_sub(indent.len() + 3)
            .max(20);
        let stdout = &mut self.output;
        writeln!(
            stdout,
            "{indent}{bar} \x1b[2m\x1b[90m{} {} {}\x1b[0m",
            t("Answered", "已回答"),
            request.questions.len(),
            t("questions", "个问题")
        )?;
        for (prompt, selected) in request.questions.iter().zip(answers) {
            let line = format!(
                "{}：{}",
                prompt.header.trim(),
                selected.join("、").replace('\n', " ")
            );
            writeln!(
                stdout,
                "{indent}{bar} \x1b[2m\x1b[90m{}\x1b[0m",
                crate::render::clip_to_display_width(&line, width)
            )?;
        }
        writeln!(stdout)?;
        stdout.flush()?;
        Ok(())
    }

    /// 子代理想了一段。先攒着，等它开始动手（或收尾）才结算成一步——
    /// 每来一个 delta 就记一步的话，面板里全是碎片。
    pub(crate) fn subagent_thought(&mut self, name: &str, text: &str) {
        if !self.timeline_enabled() || text.trim().is_empty() {
            return;
        }
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        // 说完一段又开始想 = 那段话说完了，按时序封成一步。
        seal_subagent_speech(log);
        // 新一轮思考开始了，上一轮的「准备xx」不再作数。
        log.preparing = None;
        log.reasoning_since.get_or_insert_with(Instant::now);
        log.reasoning.push_str(text);
        self.publish_subagent(name);
    }

    /// 子代理正在流某个工具的参数（主线那种「准备编辑」）。
    pub(crate) fn subagent_tool_preparing(&mut self, name: &str, tool: &str) {
        if !self.timeline_enabled() {
            return;
        }
        let Some(phase) = crate::tools::preparing_phase(tool) else {
            return;
        };
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        // 参数开始流 = 这一段想完了、话也说完了：先按时序封掉，「准备xx」才排
        // 在它们后面。原来思考要等结果回来才结算，面板里「准备执行」一直压在
        // 「思考中」上头，思考的耗时还把工具跑的时间算了进去。
        seal_subagent_speech(log);
        flush_subagent_thought(log);
        if log.preparing.is_none() {
            log.preparing = Some((phase, Instant::now()));
        }
        self.publish_subagent(name);
    }

    /// 子代理开始调一个工具：掐表，并在面板里露出「正在跑」那一行。
    /// 内层事件不带耗时，不自己记就只能不显示。
    pub(crate) fn subagent_tool_started(
        &mut self,
        name: &str,
        tool: &str,
        display: &str,
        args: &str,
    ) {
        if !self.timeline_enabled() {
            return;
        }
        let peek = crate::render::tool_peek(tool, args)
            .filter(|subject| !subject.trim().is_empty())
            .map(|subject| crate::render::clip_to_display_width(&subject, 72));
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        seal_subagent_speech(log);
        flush_subagent_thought(log);
        log.preparing = None;
        log.running = Some((tool_glyph(tool), display.to_string(), peek, Instant::now()));
        log.tool_since = Some(Instant::now());
        self.publish_subagent(name);
    }

    /// 子代理调完一个工具：结算掉在它之前那段思考，再记这一步。
    pub(crate) fn subagent_tool(
        &mut self,
        name: &str,
        tool: &str,
        display: &str,
        args: &str,
        ok: bool,
        output: &str,
    ) {
        if !self.timeline_enabled() {
            return;
        }
        // 窥视按工具自己的规矩摘一句，摘不出来才退回原文。原样甩一行
        // `{"patchText": "*** Begin Patch\n…"}` 出来，那一行就再也读不出是在
        // 改哪个文件了（用户实测截图）。
        let subject = crate::render::tool_peek(tool, args).unwrap_or_default();
        let peek = (!subject.trim().is_empty())
            .then(|| crate::render::clip_to_display_width(&subject, 72));
        let mut body = Vec::new();
        // 编辑类工具：正文给 **diff**，不给参数也不给那份结果 JSON。
        // 工具自己跑那条路会用改前改后算真 diff（`__patch_preview__`），但那条
        // 只到发起它的渲染器；子代理内层的编辑手上只有调用参数里的信封，照它画。
        let diff = crate::render::patch_envelope_lines_from_args(tool, args, detail_width());
        match diff {
            Some(lines) => body.extend(lines),
            None => {
                // 和主线那一步点开一个样子：主题（命令全文／路径）一段、空一行、
                // 然后是输出。`$` 是抬头上的图标，正文里不再写一遍。
                if !subject.trim().is_empty() {
                    body.extend(
                        wrap_detail(&subject)
                            .into_iter()
                            .map(|piece| format!("\x1b[2m{piece}\x1b[0m")),
                    );
                }
                let output = tool_output_lines(output);
                if !body.is_empty() && !output.is_empty() {
                    body.push(String::new());
                }
                body.extend(output);
            }
        }
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        seal_subagent_speech(log);
        flush_subagent_thought(log);
        log.running = None;
        log.preparing = None;
        let glyph = if ok { tool_glyph(tool) } else { glyph_err() };
        let failed = !ok;
        let elapsed_of_step = log
            .tool_since
            .take()
            .map(|since| since.elapsed())
            .unwrap_or_default();
        let mut label = timed_label(display, elapsed_of_step);
        if let Some(peek) = peek {
            label.push_str(PEEK_SEP);
            label.push_str(&peek);
        }
        log.segment.tools += 1;
        if failed {
            log.segment.errors += 1;
        }
        log.segment.note_start_since(elapsed_of_step);
        log.steps.push(Step::new(
            if failed {
                step_line_failed_in(glyph, &label, panel_step_width())
            } else {
                step_line_in(glyph, &label, panel_step_width())
            },
            body,
            None,
        ));
        trim_subagent(log);
        self.publish_subagent(name);
    }

    /// 每个 tick 把还活着的面板重新灌一遍，「准备执行 · 1.2s」、标题上的秒数才会
    /// 走。面板内容不归转轮管，只在事件到来时重生成——不灌的话它停在上一次事件
    /// 那一刻（用户实测：浮层里 `准备执行 · 0.0s` 不动）。十分之一秒灌一次够了，
    /// 秒数就是这个精度。
    pub(crate) fn refresh_subagent_panels(&mut self) {
        if !blocks::enabled() {
            return;
        }
        let now = Instant::now();
        if self
            .last_subagent_refresh
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(100))
        {
            return;
        }
        self.last_subagent_refresh = Some(now);
        let live: Vec<String> = self
            .subagent_logs
            .iter()
            .filter(|(_, log)| log.id.is_some() && !log.finished)
            .map(|(name, _)| name.clone())
            .collect();
        for name in live {
            self.publish_subagent(&name);
        }
    }

    /// 把一个子代理的时间线灌进它的覆盖层块。
    fn publish_subagent(&mut self, name: &str) {
        let display = self.display_tool_name(name);
        let Some(log) = self.subagent_logs.get_mut(name) else {
            return;
        };
        let title = subagent_title(log, &display);
        let lines = subagent_lines(log);
        let id = log.id;
        match id {
            Some(id) => blocks::update(id, title, lines),
            None => {
                let id = blocks::register_overlay(title, lines);
                if let Some(log) = self.subagent_logs.get_mut(name) {
                    log.id = id;
                }
            }
        }
    }

    /// 子代理收尾：把最后一段思考也结算掉。
    /// 派出去时交给它的差事。面板里的第一步，点开看全文。
    ///
    /// 面板里原来全是它自己的动作——思考、调工具——唯独没有"它被要求干什么"。
    /// 那件事只有派它出去的那一轮知道，而隔几分钟回来看这个面板的人是没有那
    /// 一轮的（用户：子代理的开头应该是 prompt 工具行）。
    pub(crate) fn subagent_prompt(&mut self, name: &str, arguments: &str) {
        if !self.timeline_enabled() {
            return;
        }
        let prompt = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|value| {
                ["prompt", "task", "instructions"].iter().find_map(|key| {
                    value
                        .get(*key)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
            })
            .unwrap_or_default();
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        if log.has_prompt {
            return;
        }
        log.started.get_or_insert_with(Instant::now);
        log.has_prompt = true;
        let body = wrap_detail(&prompt)
            .into_iter()
            .map(|line| format!("\x1b[2m{line}\x1b[0m"))
            .collect::<Vec<_>>();
        let head = format!(
            "{}{PEEK_SEP}{}",
            t("prompt", "提示词"),
            peek_head(&prompt, panel_step_width())
        );
        // 插在最前面 → 后面每一步的位置都往后挪了一格，块表按位置对齐，重来一轮。
        log.step_blocks.clear();
        log.steps.insert(
            0,
            Step::new(
                step_line_in(PROMPT_GLYPH, &head, panel_step_width()),
                body,
                None,
            ),
        );
        self.publish_subagent(name);
    }

    /// 子代理开口说正文了。
    ///
    /// 和主线一个规矩：**开始说话就把前面那一段过程收成一行** `Worked for …`
    /// （用户：可以把前面已经完成的 timeline 在浮层里缩成 Worked for）。面板里
    /// 一路平铺着几十步的话，真正的产出反而被埋在最底下。
    pub(crate) fn subagent_content(&mut self, name: &str, text: &str) {
        if !self.timeline_enabled() || text.is_empty() {
            return;
        }
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        if log.speech.is_empty() {
            flush_subagent_thought(log);
            collapse_subagent_segment(log);
        }
        log.speech.push_str(text);
        self.publish_subagent(name);
    }

    /// 子代理报了一次统计。
    pub(crate) fn subagent_stats(&mut self, name: &str, text: &str, tokens: Option<&str>) {
        if !self.timeline_enabled() {
            return;
        }
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        log.stats = Some(text.to_string());
        if let Some(tokens) = tokens.map(str::trim).filter(|text| !text.is_empty()) {
            log.tokens = Some(tokens.to_string());
        }
        self.publish_subagent(name);
    }

    /// 这个子代理至此烧了多少（短标，给时间线那一行用）。
    pub(crate) fn subagent_tokens_label(&self, name: &str) -> Option<String> {
        self.subagent_logs.get(name)?.tokens.clone()
    }

    pub(crate) fn finish_subagent_log(&mut self, name: &str) {
        if !self.timeline_enabled() {
            return;
        }
        if let Some(log) = self.subagent_logs.get_mut(name) {
            flush_subagent_thought(log);
            // 跑完了就不再开那扇四行的窗——它已经收成主线上的一步了。
            log.finished = true;
        }
        self.publish_subagent(name);
    }

    /// 回放：把某一步真实花掉的时间喂回去。
    ///
    /// 回放是一瞬间喂完的，墙上时间是零——`Worked for …` 那一截于是整个消失
    ///（用户实测对比图：重开前 `Worked for 6.6s · 1 tool · 2 thoughts`，
    /// 重开后只剩 `1 tool · 2 thoughts`）。把起点往回倒，后面的计时照常走，
    /// 连带那一步自己那行的 `· 1.2s` 也一并回来了。
    pub(crate) fn replay_tool_elapsed(&mut self, name: &str, elapsed: std::time::Duration) {
        if elapsed.is_zero() {
            return;
        }
        let stats = self.tool_stats_entry(name);
        stats.started_at = Instant::now().checked_sub(elapsed);
    }

    /// 同 [`Self::replay_tool_elapsed`]，这一段思考想了多久。
    pub(crate) fn replay_reasoning_elapsed(&mut self, elapsed: std::time::Duration) {
        if elapsed.is_zero() {
            return;
        }
        self.reasoning_elapsed = Some(elapsed);
    }

    /// 这一刻跑着的子代理**一共**烧了多少词元。
    ///
    /// 会话累计（footer 上的 Σ）要等子代理跑完、审计会话落盘才动；而一个子代理
    /// 能跑好几分钟，那几分钟里 Σ 纹丝不动（用户问：这个 token 消耗记录有每步
    /// 刷新到会话累计吗）。跑着的时候先把这份加上去，回合收尾时 Σ 从库里重读、
    /// 这份清零，不会算两遍。
    pub(crate) fn running_subagent_tokens(&self) -> u64 {
        self.subagent_tokens.values().copied().sum()
    }

    /// 这个子代理的覆盖层 id（没有就没有）。
    pub(crate) fn subagent_overlay_id(&self, name: &str) -> Option<u64> {
        self.subagent_logs.get(name).and_then(|log| log.id)
    }

    /// 收尾：把这一段连续过程压成一行 `Worked for …`，并把整条时间线挂成它的
    /// 展开内容。时间线里的每一项**自己也是块**，于是能再点开看详情。
    /// 把攒着的"结果"放出来。时间线收完才轮到它们——见
    /// [`StreamRenderer::pending_after_timeline`]。
    pub(crate) fn flush_after_timeline(&mut self) -> anyhow::Result<()> {
        use std::io::Write as _;
        if self.pending_after_timeline.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.pending_after_timeline);
        let stdout = &mut self.output;
        for chunk in pending {
            write!(stdout, "{chunk}")?;
        }
        stdout.flush()?;
        Ok(())
    }

    /// 把一段"结果"排到时间线后面去。
    pub(crate) fn queue_after_timeline(&mut self, text: String) {
        self.pending_after_timeline.push(text);
    }

    pub(crate) fn cut_timeline(&mut self) -> anyhow::Result<()> {
        use std::io::Write as _;
        if self.timeline.is_empty() {
            self.timeline = Timeline::default();
            // 没有时间线可收，攒着的结果也没有理由再等。
            return self.flush_after_timeline();
        }
        // live 区先收掉：不收的话它那几行留在屏上，摘要会接在它们下面，
        // 于是「收缩」看起来根本没发生。
        self.stop_waiting()?;
        if self.timeline_static() {
            // 静态版：步骤早就一步一步落下去了，这里只是这一段到此为止——
            // 空一行和后面的正文分开。没有 `Worked for …`：点不开的把手只是
            // 一行废话。
            self.commit_static_steps()?;
            // 段尾空一行。上一步欠的那一行空就是它，不再多空一行。
            self.timeline = Timeline::default();
            let stdout = &mut self.output;
            writeln!(stdout)?;
            stdout.flush()?;
            return self.flush_after_timeline();
        }
        let timeline = std::mem::take(&mut self.timeline);
        let summary = summary_line(
            timeline.elapsed(),
            Counts {
                tools: timeline.tools,
                thoughts: timeline.thoughts,
                errors: timeline.errors,
            },
        );
        // 展开内容：头行 + 用连线串起来的每一步（各自包成块）。
        let mut steps = Vec::with_capacity(timeline.steps.len());
        for step in &timeline.steps {
            if step.overlay.is_none() && step.body.is_empty() {
                steps.push(step_rows(step, None));
                continue;
            }
            // 展开这一步时**头行留着**：它是把手，再点一次才收得回去；
            // 正文缩进到竖线右边，和折叠态对得上列。
            let target = match step.overlay {
                // 子代理直接挂它那块流水账：点开是覆盖层。
                Some(id) => Some(id),
                // live 区里用的那块，收缩之后还是它：点开着的保持点开。
                None => step.block.or_else(|| blocks::register(step_detail(step))),
            };
            steps.push(step_rows(step, target));
        }
        let mut expanded = vec![format!("\x1b[2m{INDENT}⌄ {summary}\x1b[0m")];
        expanded.push(rail());
        expanded.extend(thread(steps));
        expanded.push(String::new());
        let stdout = &mut self.output;
        blocks::write_expandable(stdout, expanded, |writer| {
            writeln!(writer, "\x1b[2m{INDENT}› {summary}\x1b[0m")?;
            // 收缩行后面留一行空：不留的话它和紧跟的正文（或下一段过程）挤在
            // 一起，看着像同一段。
            writeln!(writer)
        })?;
        stdout.flush()?;
        // 收缩行落地了，这一段干出来的结果接在它下面。
        self.flush_after_timeline()
    }
}

/// 这一段里各做了多少件事。
#[derive(Clone, Copy, Default)]
pub(crate) struct Counts {
    pub(crate) tools: usize,
    pub(crate) thoughts: usize,
    pub(crate) errors: usize,
}

/// `Worked for 12s · 3 tools · 2 thoughts · 1 err`。为零的项不写。
///
/// 措辞和 WebUI 的过程时间线一致（两端看到的是同一件事，不该换说法）。
pub(crate) fn summary_line(elapsed: Duration, counts: Counts) -> String {
    // 回放历史时**完全**没有计时（库里存的是做过什么，不是花了多久）。那种
    // 情况下报个 `Worked for 0.0s` 比不报还糟——它看着像"这一轮瞬间就完了"。
    //
    // 判据是「够不够一位小数」而不是「是不是零」：回放那条路上时间线还是会被
    // 现场掐一次表，量出来是几十微秒，比零大但照样打印成 `0.0s`。
    let mut parts = Vec::new();
    if elapsed >= Duration::from_millis(100) {
        parts.push(format!("Worked for {}", format_seconds(elapsed)));
    }
    if counts.tools > 0 {
        parts.push(format!(
            "{} {}",
            counts.tools,
            if counts.tools == 1 { "tool" } else { "tools" }
        ));
    }
    if counts.thoughts > 0 {
        parts.push(format!(
            "{} {}",
            counts.thoughts,
            if counts.thoughts == 1 {
                "thought"
            } else {
                "thoughts"
            }
        ));
    }
    if counts.errors > 0 {
        parts.push(format!("{} err", counts.errors));
    }
    if parts.is_empty() {
        return t("done", "已完成").to_string();
    }
    parts.join(" · ")
}

/// 取文本末尾能放进 `width` 的一段，压成单行。
pub(crate) fn peek_tail(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if width == 0 {
        return String::new();
    }
    let flat: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    let mut taken: Vec<char> = Vec::new();
    let mut used = 0usize;
    for ch in flat.chars().rev() {
        let w = ch.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        taken.push(ch);
    }
    taken.reverse();
    let peek: String = taken.into_iter().collect();
    if peek.len() < flat.len() {
        format!("…{peek}")
    } else {
        peek
    }
}
