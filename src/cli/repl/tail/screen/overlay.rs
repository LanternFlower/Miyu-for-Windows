//! 覆盖层：盖住整屏看一段内容，Esc 关掉。
//!
//! 子代理是它存在的理由——子代理里面自己还在流（它的思考、它调的工具），塞进
//! 正文里就地展开会把主线冲散，而且它**还在长**，就地展开的行数每秒都在变、
//! 视口跟着抖。盖一层就没这些问题：主线原封不动，面板里自己滚。
//!
//! 内容取自 `render::blocks` 的登记处，每帧按版本号看要不要重取——于是「边跑
//! 边看」是白拿的：渲染方往那块里灌，面板下一帧就跟上。

use super::ansi::spans_to_ansi;
use super::expand::{layer_hit, layer_len, layer_row, Body, Expanded, Layer};
use super::Screen;
use crate::cli::t;
use crate::render::blocks;
use crossterm::{
    cursor::MoveTo,
    queue,
    style::Print,
    terminal::{Clear, ClearType},
};
use std::io::Write;

/// 面板的内容从哪来。
enum Source {
    /// 渲染方登记的一块（子代理的内层流水账）。
    Block { id: u64, version: u64 },
    /// 盘上的日志文件（后台任务）。任务跑在 daemon 里，日志本来就落盘，
    /// 直接读比再造一条 IPC 分页通道省事。
    File { path: std::path::PathBuf, size: u64 },
}

/// 日志最多读末尾多少字节。后台任务能跑很久，整个读进来没意义。
const LOG_TAIL_BYTES: u64 = 256 * 1024;

/// 面板左右各留几列。留白之外还有个作用：一眼看得出面板到哪儿为止。
const PANEL_MARGIN: u16 = 2;

/// 面板里真正能写字的宽度：屏幕宽减掉左右留白。
///
/// **面板里的一切都按它算**——排版按整屏宽算的话，行会长出去，画的时候再硬裁
/// 一刀，右边就参差不齐（用户实测：右侧边框有些 broken）。
fn panel_inner_width(cols: u16) -> usize {
    usize::from(cols)
        .saturating_sub(usize::from(PANEL_MARGIN) * 2)
        .max(8)
}

/// 上下各留一行空白。
///
/// 不留的话最后一行贴着按键提示、第一行贴着标题，读起来像是内容被框夹住了
///（用户原话：最后一行离底部的按键提示太近了，顶部也是）。
const PANEL_PAD: u16 = 1;

/// 框线 + 上下留白一共占几行。
const PANEL_CHROME: u16 = 2 + PANEL_PAD * 2;
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// 面板的上下两条横线：`── 标题 ──────── 右边那串 ──`。
///
/// **只有上下，没有左右，也没有圆角**（用户拍板）。左右两根竖线并不解释任何
/// 东西——面板占满整行，上下两条线已经说清楚它从哪到哪；竖线只是让每一行都少
/// 两列可用宽度，还逼着内容再裁一刀。
///
/// 整条线连同标题、按键提示**一律暗色**：它是取景框，不是内容。
fn frame_line(width: usize, label: &str, trailing: Option<&str>) -> String {
    let tail = trailing.map(|text| format!(" {text} ")).unwrap_or_default();
    let tail_width = crate::render::visible_width(&tail);
    let label_room = width.saturating_sub(tail_width + 8);
    let label = crate::render::clip_to_display_width(label, label_room.max(4));
    let label_width = crate::render::visible_width(&label);
    let fill = width.saturating_sub(label_width + tail_width + 6).max(1);
    format!("{DIM}── {label} {}{tail}──{RESET}", "─".repeat(fill))
}

pub(in crate::cli) struct Overlay {
    source: Source,
    /// 这个面板讲的是哪个后台任务。有值才允许按 x 停。
    job_id: Option<String>,
    /// 画面宽度，重新解析内容时要用。
    cols: usize,
    title: String,
    body: Body,
    /// 面板里自己的展开状态。子代理内层也是一条时间线，那里每一步同样能点开，
    /// 而它的开合和正文那边互不相干，所以各带各的表。
    expanded: Expanded,
    scroll: usize,
    /// 停在底部就跟着新内容走；自己往回翻过就别再拽他。
    follow: bool,
    /// 每一步对应的块 id，按位置复用。见 `render_log`。
    step_blocks: Vec<u64>,
    /// 收缩行里每一步的块 id，按 `(收缩行位置, 步位置)` 复用。见 `fold_detail`。
    fold_blocks: std::collections::HashMap<(usize, usize), u64>,
    /// 面板占多高。**开着的时候只涨不缩**。
    ///
    /// 按当前内容每帧重算的话，后台任务每写一行日志面板就长高一点、上边沿
    /// 跟着往上跳——AI 正在输出时日志一秒写十几行，面板就一直在抖
    /// （用户原话「后台命令再究极鬼畜…浮层的位置也不对」）。
    height: u16,
}

impl Overlay {
    fn from_block(id: u64, cols: usize) -> Option<Self> {
        let lines = blocks::get(id)?;
        Some(Self {
            source: Source::Block {
                id,
                version: blocks::version(id),
            },
            job_id: None,
            cols,
            title: blocks::title(id).unwrap_or_else(|| t("detail", "详情").to_string()),
            body: parse_body(&lines, cols),
            expanded: Expanded::new(),
            scroll: 0,
            follow: true,
            height: 0,
            step_blocks: Vec::new(),
            fold_blocks: std::collections::HashMap::new(),
        })
    }

    fn from_file(
        path: std::path::PathBuf,
        title: String,
        job_id: Option<String>,
        cols: usize,
    ) -> Self {
        let mut panel = Self {
            source: Source::File { path, size: 0 },
            job_id,
            title,
            body: parse_body(&[], cols),
            cols,
            expanded: Expanded::new(),
            scroll: 0,
            follow: true,
            height: 0,
            step_blocks: Vec::new(),
            fold_blocks: std::collections::HashMap::new(),
        };
        panel.reload_file(true);
        panel
    }

    /// 屏幕宽变了：面板里的一切按新宽度重排一遍。
    ///
    /// 不重排的话，框跟着新宽度画、内容还按旧宽度折，两边就对不上了。
    fn set_cols(&mut self, cols: usize) {
        if self.cols == cols {
            return;
        }
        self.cols = cols;
        self.expanded.clear();
        match &mut self.source {
            Source::Block { id, version } => {
                let id = *id;
                *version = blocks::version(id);
                if let Some(lines) = blocks::get(id) {
                    self.body = parse_body(&lines, cols);
                }
            }
            Source::File { .. } => self.reload_file(true),
        }
    }

    fn block_id(&self) -> Option<u64> {
        match &self.source {
            Source::Block { id, .. } => Some(*id),
            Source::File { .. } => None,
        }
    }

    fn file_path(&self) -> Option<&std::path::Path> {
        match &self.source {
            Source::File { path, .. } => Some(path.as_path()),
            Source::Block { .. } => None,
        }
    }

    fn reload_file(&mut self, force: bool) {
        let Source::File { path, size } = &mut self.source else {
            return;
        };
        let current = std::fs::metadata(&*path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if !force && current == *size {
            return;
        }
        *size = current;
        let text = read_tail(path, LOG_TAIL_BYTES);
        let lines = self.render_log(&text);
        self.body = parse_body(&lines, self.cols);
        // 展开着的那几块留着，只是把内容换成新的——同 `refresh` 里那条注释：
        // 一刷新就整张清掉的话，刚点开的东西立刻自己缩回去。
        super::expand::reload_expanded(&mut self.expanded, self.cols);
    }

    /// 把流水账渲染成一条**能点开**的时间线。
    ///
    /// 每一步登记成一块（`blocks`），行里带上标记，面板自己那张展开表就认得它。
    /// 块 id **按位置复用**：日志是只增的，第 i 步永远是第 i 步；每次重读都新登记
    /// 一批的话，登记处几秒就被刷爆，而且用户点开的那一块会在下一次刷新时变成
    /// 另一个 id、当场合上。
    fn render_log(&mut self, text: &str) -> Vec<String> {
        let indent = "  ";
        let rail = crate::render::timeline::panel_rail();
        // 抬头能占多宽：和前台那种面板同一把尺（`panel_step_line` 自己还会再
        // 裁一刀兜底）。再扣掉图标那一格和尾巴上的 ` · ok`。
        let head_width = crate::render::timeline::panel_step_width_for_head().max(12);
        // 后台**命令**的日志就是一堆输出行，没有"步"可言——按时间线排会变成
        // 每行一个节点、行行之间一条连线，那是把日志排成了梯子。原样折行就好。
        if !text.lines().any(|line| {
            line.starts_with("[思考]")
                || line.starts_with("[工具]")
                || line.starts_with("[结果]")
                || line.starts_with("[统计]")
                || line.starts_with("[提示]")
                || line.starts_with("[正文]")
        }) {
            self.step_blocks.clear();
            let width = self.cols.saturating_sub(6).max(20);
            return text
                .lines()
                .flat_map(|line| {
                    if line.trim().is_empty() {
                        return vec![String::new()];
                    }
                    crate::render::wrap_display_text(line, width)
                        .into_iter()
                        .map(|piece| format!("{indent}{piece}"))
                        .collect::<Vec<_>>()
                })
                .collect();
        }
        let steps = log_steps(text);
        if steps.len() < self.step_blocks.len() {
            // 日志被从头截断过（只读末尾那一段），位置对不上了，重来一轮。
            self.step_blocks.clear();
        }
        let mut out: Vec<String> = Vec::new();
        // 「提示词」那一行是抬头，不是时间线的一步：它和第一步之间不连线、空一行
        // ——连着画的话，思考那一步和提示词看着是一条线上的两步，收缩时就像被
        // 提示词绑住了（用户原话）。
        // 连线只连**相邻的两步**。步和正文之间、提示词和第一步之间都是空一行：
        // 收缩行底下紧跟着它产出的那段话（和主线「Worked for → 正文」一个次序），
        // 正文之后的下一步另起一段。原来正文之后也画连线，看着像收缩行属于上面
        // 那段话（用户实测：正文和 timeline 反了）。
        #[derive(Clone, Copy, PartialEq)]
        enum Previous {
            None,
            Prompt,
            Speech,
            Step,
        }
        let mut previous = Previous::None;
        for (index, step) in steps.iter().enumerate() {
            // 正文不是"一步"：没有抬头、不挂块、也不连线，整段照排。
            if step.speech {
                if previous != Previous::None {
                    out.push(String::new());
                }
                previous = Previous::Speech;
                // 过一遍 markdown 再折行——和前台面板、主线正文一个样子。
                out.extend(
                    crate::render::timeline::render_speech_lines(
                        &step.body.join("\n"),
                        self.cols.saturating_sub(indent.len()),
                    )
                    .into_iter()
                    .map(|piece| format!("{indent}{piece}")),
                );
                continue;
            }
            match previous {
                Previous::Step => out.push(rail.clone()),
                Previous::Prompt | Previous::Speech => out.push(String::new()),
                Previous::None => {}
            }
            previous = if step.glyph == PROMPT_GLYPH {
                Previous::Prompt
            } else {
                Previous::Step
            };
            // 抬头一律暗色，绿色留给展开之后的思考正文——和主线那边一个规矩。
            // 反过来（抬头绿、正文白）看着像把手比内容还重要，而这一行本来就
            // 只是个把手（用户实测：浮层里思考行和思考展开内容的颜色反了）。
            let _ = step.green;
            // ok 不上抬头：主线和前台面板都不写 ok，跑砸了靠红色和打叉说话。
            // 日志末尾那个还没有结果的调用例外：标出它正在跑，不然看着像卡住了。
            let status = if step.status.is_none() && step.running {
                format!(" · {}", crate::i18n::text("running", "运行中"))
            } else {
                String::new()
            };
            // 收缩行合着的时候是 `›`，点开才翻成 `⌄`（和主线那条一样）。
            let glyph = if step.glyph == SUMMARY_GLYPH {
                crate::render::timeline::fold_glyph_closed()
            } else {
                step.glyph.as_str()
            };
            // 想的那一步按主线的说法写：`已思考  <窥视>`。面板里光甩一句原文
            // 出来，看不出那是"在想"还是工具吐的东西。
            let head = if step.thinking {
                // 窥视取**末尾**：想到哪儿了比想过什么更有用，而且它每刷新一次
                // 就往前走一点，正好是「它还活着」的指示。取开头的话，一整段
                // 思考落下来之后这一行就再也不动了（用户实测：思考的窥视刷新
                // 似乎不太对）。
                format!(
                    "{}{}{}",
                    crate::i18n::text("thought", "已思考"),
                    crate::render::timeline::PEEK_SEP,
                    crate::render::timeline::peek_tail(&step.head, head_width)
                )
            } else {
                step.head.clone()
            };
            let head = crate::render::clip_to_display_width(&head, head_width);
            // 前台那种面板（走事件）和这种（读日志）用的是**同一份**排版代码：
            // 取数的地方不同，长相不该不同（用户原话：后台子代理和前台子代理
            // 应该是一回事啊，为什么感觉你做出来两个浮层）。
            // 正在跑／正在准备的那一行左边距上转着点阵，和主线一样。
            let line = if step.running || step.preparing {
                crate::render::timeline::panel_live_step_line(glyph, &format!("{head}{status}"))
            } else {
                crate::render::timeline::panel_step_line(
                    glyph,
                    &format!("{head}{status}"),
                    step.status == Some("err"),
                )
            };
            let detail = if step.inner.is_empty() {
                log_step_detail(&line, step)
            } else {
                self.fold_detail(index, &line, step)
            };
            let id = match self.step_blocks.get(index) {
                Some(id) => {
                    blocks::update(*id, String::new(), detail);
                    Some(*id)
                }
                None => {
                    let id = blocks::register(detail);
                    if let Some(id) = id {
                        self.step_blocks.push(id);
                    }
                    id
                }
            };
            match id {
                Some(id) => out.push(format!(
                    "{}{line}{}",
                    blocks::begin_marker(id),
                    blocks::END_MARKER
                )),
                None => out.push(line),
            }
        }
        out
    }

    /// 收缩行点开是什么样：收起来的那几步串成时间线，每一步各自登记成块，
    /// 再点开才是它的正文。块 id 按 `(收缩行位置, 步位置)` 复用，刷新不换 id。
    fn fold_detail(&mut self, fold_index: usize, line: &str, fold: &LogStep) -> Vec<String> {
        let head_width = crate::render::timeline::panel_step_width_for_head().max(12);
        let mut rows: Vec<String> = Vec::new();
        for (inner_index, step) in fold.inner.iter().enumerate() {
            if inner_index > 0 {
                rows.push(crate::render::timeline::panel_rail());
            }
            let head = step_head(step, head_width);
            let head = crate::render::clip_to_display_width(&head, head_width);
            let inner_line = crate::render::timeline::panel_step_line(
                &step.glyph,
                &head,
                step.status == Some("err"),
            );
            let detail = log_step_detail(&inner_line, step);
            let id = match self.fold_blocks.get(&(fold_index, inner_index)).copied() {
                Some(id) => {
                    blocks::update(id, String::new(), detail);
                    Some(id)
                }
                None => {
                    let id = blocks::register(detail);
                    if let Some(id) = id {
                        self.fold_blocks.insert((fold_index, inner_index), id);
                    }
                    id
                }
            };
            rows.push(match id {
                Some(id) => format!(
                    "{}{inner_line}{}",
                    blocks::begin_marker(id),
                    blocks::END_MARKER
                ),
                None => inner_line,
            });
        }
        // 收缩行点开是时间线：抬头（`›` 翻成 `⌄`）、连线、各步同一列，不缩进
        // ——和主线那份一个样子。
        let mut detail = Vec::with_capacity(rows.len() + 3);
        detail.push(crate::render::timeline::fold_line_open(line));
        detail.push(crate::render::timeline::panel_rail());
        detail.extend(rows);
        detail.push(String::new());
        detail
    }

    /// 内容有变就重取。
    fn refresh(&mut self) {
        match &mut self.source {
            Source::Block { id, version } => {
                let current = blocks::version(*id);
                if current == *version {
                    return;
                }
                *version = current;
                let id = *id;
                if let Some(title) = blocks::title(id) {
                    self.title = title;
                }
                if let Some(lines) = blocks::get(id) {
                    self.body = parse_body(&lines, self.cols);
                    // 展开着的那几块**留着**，只是把内容换成新的。
                    //
                    // 原来是整表清掉：子代理一边跑一边刷新（一秒好几次），
                    // 于是刚点开的东西立刻自己缩回去，根本读不了一句
                    //（用户实测：窥视的动态刷新会导致已展开内容缩起）。
                    // 块 id 现在是按位置复用的，第 i 步永远是第 i 步，
                    // 保住它是安全的。
                    super::expand::reload_expanded(&mut self.expanded, self.cols);
                }
            }
            Source::File { .. } => self.reload_file(false),
        }
    }
}

/// 读文件末尾 `budget` 字节。从中间切开的第一行丢掉，免得开头是半个字符。
fn read_tail(path: &std::path::Path, budget: u64) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let from = size.saturating_sub(budget);
    if from > 0 && file.seek(SeekFrom::Start(from)).is_err() {
        return String::new();
    }
    let mut buffer = Vec::new();
    if file.read_to_end(&mut buffer).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    if from > 0 {
        match text.find('\n') {
            Some(index) => text[index + 1..].to_string(),
            None => text,
        }
    } else {
        text
    }
}

/// 后台任务的日志排成时间线。
///
/// 日志里是 `[思考] …` / `[工具] …` / `[结果] …` 这样的行，原样贴出来是一份
/// 流水账；而前台子代理点开看到的是一条时间线。**同一件事不该有两种看法**，
/// 所以这里把日志翻译成同样的形状：思考、工具各占一步，之间用竖线串起来。
///
/// 认不出前缀的行（命令的裸输出）原样保留——那本来就该原样看。
/// 日志里的一步。
#[derive(Default)]
struct LogStep {
    glyph: String,
    green: bool,
    head: String,
    status: Option<&'static str>,
    body: Vec<String>,
    /// 这一步是不是"想"。连续的思考要并成一条，和主线一个规矩。
    thinking: bool,
    /// 这一步是不是它说的正文。正文没有抬头，整段就是内容。
    speech: bool,
    /// 这一步花了多久（`[结果]` 行上带的 `· 1.2s`）。收缩行的 Worked for 靠它加。
    elapsed: Option<std::time::Duration>,
    /// 收缩行：收起来的那几步。点开收缩行看到的是它们，每一步再点开才是正文。
    inner: Vec<LogStep>,
    /// 日志末尾那个还没有结果的调用：它正在跑。
    running: bool,
    /// 日志末尾的 `[准备]`：参数还在流。
    preparing: bool,
    /// 这一步的主题（命令全文、路径、检索词——`[工具] 运行命令 · ls` 里 ` · ` 后面
    /// 那段）。点开之后正文第一段是它，不是把抬头再说一遍。
    subject: Option<String>,
}

/// `运行命令 · ls` → `ls`：抬头里 ` · ` 后面那段是主题。
fn subject_of(text: &str) -> Option<String> {
    text.split_once(" · ")
        .map(|(_, subject)| subject.trim().to_string())
        .filter(|subject| !subject.is_empty())
}

/// `<工具 id>\t<中文名> · <主题>` → `(图标, 去掉 id 的正文)`。
///
/// 老日志里没有那个制表符（改格式之前写的），那就退回通用齿轮。
fn split_tool_line(rest: &str) -> (String, String) {
    match rest.split_once('\t') {
        Some((name, text)) => (
            crate::render::tool_glyph_for(name.trim()).to_string(),
            text.trim().to_string(),
        ),
        None => ("\u{f013}".to_string(), rest.trim().to_string()),
    }
}

/// 「交给它的差事」那一步的图标（文档）。
const PROMPT_GLYPH: &str = "\u{f4a5}";

/// 收缩行的图标。和主线那条 `⌄ Worked for …` 一个样子。
const SUMMARY_GLYPH: &str = "⌄";

/// `[统计]` 那一行的图标。它不是工具调用：没有结果行，也永远不该被当成
/// 「末尾那个还没回来的调用」挂上转轮（测具截图：`⠏ 工具调用 3 次 · 运行中`）。
const STATS_GLYPH: &str = "\u{f200}";

/// 把已经走完的那几步收成一行 `⌄ Worked for …`，点开还是那几步。
///
/// 「提示词」那一行钉在最前面不参与收缩——它说的是"要干什么"，不是过程。
fn collapse_log_segment(steps: &mut Vec<LogStep>) {
    // 这一段从哪儿开始：**上一段正文之后**；没说过话就是提示词之后。原来一律
    // 取"提示词后面第一步"，第二次开口时把上一个收缩行、上一段正文连同新的几步
    // 全卷进一个收缩行——面板里永远只剩开头那一个 Worked for（用户实测）。
    let from = steps
        .iter()
        .rposition(|step| step.speech)
        .map(|index| index + 1)
        .unwrap_or_else(|| {
            steps
                .iter()
                .position(|step| step.glyph != PROMPT_GLYPH)
                .unwrap_or(steps.len())
        });
    if steps.len() <= from + 1 {
        return;
    }
    let collapsed: Vec<LogStep> = steps.drain(from..).collect();
    let tools = collapsed
        .iter()
        .filter(|step| !step.thinking && !step.speech && step.glyph != SUMMARY_GLYPH)
        .count();
    let thoughts = collapsed.iter().filter(|step| step.thinking).count();
    let errors = collapsed
        .iter()
        .filter(|step| step.status == Some("err"))
        .count();
    // 这一段花了多久：每一步自己的耗时加起来（流水账里没有时间戳，只有 `[结果]`
    // 行上带的那个数）。思考没记时，所以这是下限——总比"什么都不报"强。
    let elapsed = collapsed
        .iter()
        .filter_map(|step| step.elapsed)
        .fold(std::time::Duration::ZERO, |sum, step| sum + step);
    let summary = crate::render::timeline::summary_line(
        elapsed,
        crate::render::timeline::Counts {
            tools,
            thoughts,
            errors,
        },
    );
    // 收起来的每一步原样留着（`inner`），渲染时各自登记成块——点开收缩行是
    // 时间线，时间线里每一步再点开才是它的正文。原来只把抬头串成一段文字，
    // 工具输出和思考全文在收缩那一刻就没了（用户实测：会丢失内容）。
    steps.push(LogStep {
        glyph: SUMMARY_GLYPH.to_string(),
        head: summary,
        elapsed: Some(elapsed),
        inner: collapsed,
        ..Default::default()
    });
}

/// 去掉结果正文里那个 ok/err：状态由 `LogStep::status` 单独盖。
fn strip_result_status(text: &str) -> String {
    for status in [" ok", " err"] {
        // 夹在中间：`运行命令 ok · ls` → `运行命令 · ls`。
        if let Some(index) = text.find(&format!("{status} · ")) {
            return format!("{}{}", &text[..index], &text[index + status.len()..]);
        }
        // 在末尾：`运行命令 ok` → `运行命令`。
        if let Some(head) = text.strip_suffix(status) {
            return head.to_string();
        }
    }
    text.to_string()
}

/// 这一步是一次工具调用吗——结果与输出只认领这种。
///
/// 正文段和收缩行（`⌄ Worked for …`）都**不是**：它俩一度也被当成工具步，于是
/// 子代理开口说过话之后，下一条 `[结果]` 给收缩行盖了个 `ok`，跟着的 `[输出]`
/// 全贴进正文段里——面板里就是一段话底下拖着几十行裸 grep 输出（用户实测截图）。
fn is_tool_step(step: &LogStep) -> bool {
    !step.thinking
        && !step.speech
        && !step.preparing
        && step.glyph != PROMPT_GLYPH
        && step.glyph != SUMMARY_GLYPH
        && step.glyph != STATS_GLYPH
}

/// 从 `运行命令 ok · 1.2s · ls` 这种正文里把耗时摘出来（第一个 ` · ` 之后那一段
/// 要是像 `1.2s` / `12s` / `1m 05s`），返回去掉耗时的正文和耗时本身。
fn split_elapsed(text: &str) -> (String, Option<std::time::Duration>) {
    let Some((head, rest)) = text.split_once(" · ") else {
        return (text.to_string(), None);
    };
    let (candidate, tail) = match rest.split_once(" · ") {
        Some((candidate, tail)) => (candidate, Some(tail)),
        None => (rest, None),
    };
    let Some(elapsed) = parse_seconds(candidate) else {
        return (text.to_string(), None);
    };
    let stripped = match tail {
        Some(tail) => format!("{head} · {tail}"),
        None => head.to_string(),
    };
    (stripped, Some(elapsed))
}

/// `format_seconds` 的逆：`0.3s` / `12s` / `1m 05s`。
fn parse_seconds(text: &str) -> Option<std::time::Duration> {
    let text = text.trim();
    if let Some((minutes, seconds)) = text.split_once("m ") {
        let minutes: u64 = minutes.parse().ok()?;
        let seconds: u64 = seconds.strip_suffix('s')?.parse().ok()?;
        return Some(std::time::Duration::from_secs(minutes * 60 + seconds));
    }
    let seconds: f64 = text.strip_suffix('s')?.parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| std::time::Duration::from_secs_f64(seconds))
}

/// 后台任务的流水账 → 和主线**一模一样**的时间线。
///
/// 流水账是一行一条记录（`[思考] …` / `[工具] …` / `[结果] …`），直接一行一行贴
/// 出来是两个毛病：一是同一次工具调用会出现两遍（叫的时候一条、回来的时候一条），
/// 二是长记录被硬切或者硬折，整条线看着是散的。
///
/// 这里把它折成"步"：工具的调用与结果合成一条（结果只是给它盖个 ok/err），
/// 续行归到上一步的正文里。每一步都是一行窥视，点开才看全文——和主线一个规矩。
fn log_steps(text: &str) -> Vec<LogStep> {
    let mut steps: Vec<LogStep> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("[思考]") {
            // 连续的思考并成一条。桥那边是按自然段落盘的，一段一行——原样贴出来
            // 一次思考会在面板里占十几个节点，那是把一段话排成了梯子
            //（用户原话「思考每一行都有标」）。并成一条之后，抬头是最新那一段的
            // 窥视，全文点开看。
            let (text, elapsed) = split_thought_elapsed(rest);
            match steps.last_mut() {
                Some(last) if last.thinking => {
                    last.body.push(last.head.clone());
                    last.head = text;
                    if let Some(elapsed) = elapsed {
                        last.elapsed = Some(last.elapsed.unwrap_or_default() + elapsed);
                    }
                }
                _ => steps.push(LogStep {
                    glyph: "\u{f0768}".to_string(),
                    green: true,
                    head: text,
                    thinking: true,
                    elapsed,
                    ..Default::default()
                }),
            }
        } else if let Some(rest) = line.strip_prefix("[提示]") {
            // 只留第一条：派出去时写一条，Full 档的任务简介又是一条，说的是
            // 同一件事。
            if steps.iter().any(|step| step.glyph == PROMPT_GLYPH) {
                continue;
            }
            // 交给它的差事。放在最前面，点开就知道这个子代理到底被要求干什么
            // ——否则一条跑了五分钟的后台子代理，面板里只剩它自己的碎碎念。
            let body = rest
                .trim()
                .split('\u{1}')
                .map(str::to_string)
                .collect::<Vec<_>>();
            // 抬头带上开头那一句：只写一个「差事」的话，这一行看着像个空标签，
            // 不点开根本不知道它派出去干什么。
            let peek = body
                .iter()
                .map(|line| line.trim())
                .find(|line| !line.is_empty())
                .unwrap_or_default();
            steps.push(LogStep {
                glyph: PROMPT_GLYPH.to_string(),
                green: false,
                head: format!(
                    "{}{}{peek}",
                    crate::i18n::text("prompt", "提示词"),
                    crate::render::timeline::PEEK_SEP
                ),
                status: None,
                body,
                thinking: false,
                speech: false,
                ..Default::default()
            });
        } else if let Some(rest) = line.strip_prefix("[正文]") {
            // 它开口说话了：**前面那一段过程收成一行**，和主线一个规矩
            //（用户：可以把前面已经完成的 timeline 在浮层里缩成 Worked for）。
            // 面板里一路平铺着几十步的话，真正的产出反而被埋在最底下。
            let text = rest.trim();
            if text.is_empty() {
                continue;
            }
            match steps.last_mut() {
                Some(last) if last.speech => last.body.push(text.to_string()),
                _ => {
                    collapse_log_segment(&mut steps);
                    steps.push(LogStep {
                        // 正文没有抬头也没有图标——整段就是内容。
                        glyph: " ".to_string(),
                        green: false,
                        head: String::new(),
                        status: None,
                        body: vec![text.to_string()],
                        thinking: false,
                        speech: true,
                        ..Default::default()
                    });
                }
            }
        } else if let Some(rest) = line.strip_prefix("[输出]") {
            // 工具吐的东西，挂到刚才那一步的详情里。
            if let Some(step) = steps.iter_mut().rev().find(|step| is_tool_step(step)) {
                step.body.push(rest.trim_end().to_string());
            }
        } else if let Some(rest) = line.strip_prefix("[工具]") {
            let (glyph, head) = split_tool_line(rest);
            steps.push(LogStep {
                glyph,
                green: false,
                subject: subject_of(&head),
                head,
                status: None,
                body: Vec::new(),
                thinking: false,
                speech: false,
                ..Default::default()
            });
        } else if let Some(rest) = line.strip_prefix("[结果]") {
            let rest = rest.trim();
            let failed = rest.contains(" err");
            // 结果配给最近那次还没有结果的调用：它俩说的是同一件事。
            let (glyph, text) = split_tool_line(rest);
            // `运行命令 ok · 1.2s · ls`：耗时摘出来单存，抬头照主线的写法
            // 「名字 · 秒数 · 窥视」。
            let (text, elapsed) = split_elapsed(&strip_result_status(&text));
            let matched = steps
                .iter_mut()
                .rev()
                .find(|step| step.status.is_none() && is_tool_step(step));
            match matched {
                Some(step) => {
                    // 结果只负责给这一步盖个 ok/err（和耗时）。它的正文和调用那一行
                    // 是同一句话（同一个工具、同一份参数），塞进详情里就是把抬头
                    // 又说一遍（用户实测：展开之后第一行和标题一模一样）。
                    step.status = Some(if failed { "err" } else { "ok" });
                    if failed {
                        step.glyph = "\u{f00d}".to_string();
                    }
                    if let Some(elapsed) = elapsed {
                        step.elapsed = Some(elapsed);
                        step.head = with_elapsed(&step.head, elapsed);
                    }
                    let _ = text;
                }
                // 没有对应的调用行——Full 档下 `run_command` 的调用事件是不发的
                // （网页端由结果整块渲染）。那就拿结果这一行自己立一步：图标用
                // **工具自己的**，正文里那个 ok/err 去掉（状态由 `status` 单独
                // 盖，留着会读成「运行命令 ok · … · ok」）。
                None => steps.push(LogStep {
                    glyph: if failed {
                        "\u{f00d}".to_string()
                    } else {
                        glyph
                    },
                    subject: subject_of(&text),
                    head: match elapsed {
                        Some(elapsed) => with_elapsed(&text, elapsed),
                        None => text,
                    },
                    status: Some(if failed { "err" } else { "ok" }),
                    elapsed,
                    ..Default::default()
                }),
            }
        } else if let Some(rest) = line.strip_prefix("[准备]") {
            // 参数还在流。只有作为日志**末尾**那一行时才是"此刻"，别处的都是
            // 已经过去的准备，收尾时统一扔掉。
            let (glyph, phase) = split_tool_line(rest);
            let _ = glyph;
            steps.push(LogStep {
                glyph: "\u{f4bc}".to_string(),
                head: phase,
                preparing: true,
                ..Default::default()
            });
        } else if let Some(rest) = line.strip_prefix("[统计]") {
            steps.push(LogStep {
                glyph: STATS_GLYPH.to_string(),
                green: false,
                head: rest.trim().to_string(),
                status: None,
                body: Vec::new(),
                thinking: false,
                speech: false,
                ..Default::default()
            });
        } else {
            match steps.last_mut() {
                // 没打标签的行只有跟在"思考"后面时才是续行（一段话里的换行）。
                // 跟在工具后面的那些是内层渲染器自己的进度回声
                //（`工具 #7: 编辑文件 · … ok`），和上一行说的是同一件事，
                // 贴进详情里只会让人以为出了两次（用户：「展开后的内容不太对」）。
                Some(last) if last.thinking => last.body.push(line.to_string()),
                Some(_) => {}
                None => steps.push(LogStep {
                    glyph: " ".to_string(),
                    head: line.trim().to_string(),
                    ..Default::default()
                }),
            }
        }
    }
    // 「准备」只在日志末尾才算数；末尾那个没结果的调用就是正在跑的那个。
    let last = steps.len().saturating_sub(1);
    let mut index = 0;
    steps.retain(|step| {
        let keep = !step.preparing || index == last;
        index += 1;
        keep
    });
    if let Some(step) = steps.last_mut() {
        if step.status.is_none() && is_tool_step(step) {
            step.running = true;
        }
    }
    steps
}

/// 一步的抬头。想的那一步按主线的说法写：`已思考 · 1.2s · <窥视>`——窥视取
/// **末尾**：想到哪儿了比想过什么更有用，而且它每刷新一次就往前走一点，正好是
/// 「它还活着」的指示；取开头的话一整段思考落下来之后这一行就再也不动了。
fn step_head(step: &LogStep, head_width: usize) -> String {
    if !step.thinking {
        return step.head.clone();
    }
    let mut head = crate::i18n::text("thought", "已思考").to_string();
    if let Some(secs) = step
        .elapsed
        .and_then(crate::render::timeline::reported_seconds)
    {
        head.push_str(" · ");
        head.push_str(&secs);
    }
    head.push_str(crate::render::timeline::PEEK_SEP);
    head.push_str(&crate::render::timeline::peek_tail(&step.head, head_width));
    head
}

/// `[思考] 1.2s\t正文`：桥把这段想了多久写在最前面，制表符隔开。老日志没有。
fn split_thought_elapsed(rest: &str) -> (String, Option<std::time::Duration>) {
    let rest = rest.trim();
    if let Some((secs, text)) = rest.split_once('\t') {
        if let Some(elapsed) = parse_seconds(secs) {
            return (text.trim().to_string(), Some(elapsed));
        }
    }
    (rest.to_string(), None)
}

/// 把耗时插进抬头：`运行命令 · ls` → `运行命令 · 1.2s · ls`（名字后面、窥视前面，
/// 和主线一个次序）。
fn with_elapsed(head: &str, elapsed: std::time::Duration) -> String {
    // 不到十分之一秒的不报：`0.0s` 只是噪音（用户实测）。收缩行照样把它算进总数。
    let Some(secs) = crate::render::timeline::reported_seconds(elapsed) else {
        return head.to_string();
    };
    match head.split_once(" · ") {
        Some((name, rest)) => format!("{name} · {secs} · {rest}"),
        None => format!("{head} · {secs}"),
    }
}

/// 一步展开之后看到的东西：头行 + 空行 + 折好行的正文。和主线的 `step_detail`
/// 是同一个形状。
fn log_step_detail(line: &str, step: &LogStep) -> Vec<String> {
    let inner = crate::render::timeline::panel_detail_width();
    let color = if step.thinking { "\x1b[38;5;10m" } else { "" };
    let mut body: Vec<String> = Vec::new();
    // 正文第一段是这一步的**主题**（命令全文、路径、检索词），空一行，然后是输出
    // ——和主线那一步点开一个样子。原来是把抬头（`运行命令 · 5.3s · echo …`）整个
    // 再说一遍（用户实测：命令展开处理异常）。没有主题的（思考、提示词）还是
    // 抬头本身：行里那一份是裁过的，这儿这份是完整的。
    let mut texts: Vec<&str> = Vec::new();
    match &step.subject {
        Some(subject) => {
            texts.push(subject);
            if !step.body.is_empty() {
                texts.push("");
            }
        }
        None => texts.push(step.head.as_str()),
    }
    texts.extend(step.body.iter().map(String::as_str));
    for text in texts {
        if text.trim().is_empty() {
            body.push(String::new());
            continue;
        }
        for piece in crate::render::wrap_display_text(text, inner) {
            body.push(format!("\x1b[2m{color}{piece}\x1b[0m"));
        }
    }
    crate::render::timeline::panel_step_detail(line, &body)
}

fn parse_body(lines: &[String], cols: usize) -> Body {
    Body::parse(&lines.join("\r\n"), cols)
}

impl Overlay {
    fn layer(&self) -> Layer<'_> {
        Layer::Body(&self.body)
    }

    fn len(&self) -> usize {
        layer_len(&self.layer(), &self.expanded)
    }

    fn row(&self, index: usize) -> Vec<super::ansi::AnsiSpan> {
        layer_row(&self.layer(), &self.expanded, index)
    }
}

impl Screen {
    /// 打开一层。已经开着同一块就当作关闭（再点一次收起来）。
    pub(in crate::cli) fn open_overlay(&mut self, id: u64) -> bool {
        if self
            .overlay
            .as_ref()
            .is_some_and(|panel| panel.block_id() == Some(id))
        {
            return self.close_overlay();
        }
        let Some(panel) = Overlay::from_block(id, panel_inner_width(self.cols)) else {
            return false;
        };
        self.overlay = Some(panel);
        self.invalidate();
        self.needs_clear = true;
        true
    }

    /// 打开一个后台任务的日志面板。再点同一个就收起来。
    pub(in crate::cli) fn open_log_overlay(
        &mut self,
        path: std::path::PathBuf,
        title: String,
        job_id: Option<String>,
    ) -> bool {
        if self
            .overlay
            .as_ref()
            .is_some_and(|panel| panel.file_path() == Some(path.as_path()))
        {
            return self.close_overlay();
        }
        self.overlay = Some(Overlay::from_file(
            path,
            title,
            job_id,
            panel_inner_width(self.cols),
        ));
        self.invalidate();
        self.needs_clear = true;
        true
    }

    /// 后台任务面板的抬头跟着任务快照走（量在涨，抬头得跟上）。
    ///
    /// 抬头是点开那一刻定下来的，之后再没人改过——于是「消耗词元」那一截一直
    /// 停在点开时的数（用户实测：后台子代理浮层上方的 token 计数没有动态刷新）。
    /// 前台那种面板的抬头跟着块一起更新，这条是给后台那种补上同样的事。
    pub(in crate::cli) fn refresh_overlay_title(&mut self, title: &str) {
        if let Some(panel) = &mut self.overlay {
            if panel.job_id.is_some() && panel.title != title {
                panel.title = title.to_string();
                self.invalidate();
            }
        }
    }

    /// 屏幕宽变了：开着的面板跟着重排。
    pub(in crate::cli) fn resize_overlay(&mut self, cols: u16) {
        let inner = panel_inner_width(cols);
        if let Some(panel) = &mut self.overlay {
            panel.set_cols(inner);
            // 高度是只涨不缩的，换了宽度之后那个值对不上新内容，放开重算一次。
            panel.height = 0;
        }
    }

    pub(in crate::cli) fn close_overlay(&mut self) -> bool {
        if self.overlay.take().is_some() {
            self.invalidate();
            self.needs_clear = true;
            return true;
        }
        false
    }

    /// 面板占多高：按内容来，最多吃掉屏幕的六成。
    ///
    /// 占满整屏没必要——面板讲的是**某一步**的细节，把主线全遮住反而让人忘了
    /// 自己在哪儿。留着上面那截正文，关掉时也不会有「换了个世界」的突兀感。
    /// 面板占多高。**按屏幕算，不按内容算**。
    ///
    /// 跟着内容长的话，刚点开时里面只有一两行，面板就只有指甲盖那么大，随后
    /// 一边读一边往上窜（用户原话「一开始这个浮层特别小」）。面板是个固定的
    /// 取景窗，大小该是稳的；内容多了滚就是了。
    /// 面板占多高。
    ///
    /// 五分之三看着太压人——它盖着的那截正文才是"我刚才在看什么"的上下文
    /// （用户：可以整体矮三分之一左右）。五分之二正好：面板里还能一眼看到
    /// 七八步，上面也留得下几行正文。
    pub(in crate::cli) fn overlay_height(&self, _content: usize, rows: u16) -> u16 {
        (rows * 2 / 5)
            .max(PANEL_CHROME + 2)
            .min(rows.saturating_sub(2).max(PANEL_CHROME + 2))
    }

    /// 面板在屏幕上占的行区间（含首尾）。没开面板就是 `None`。
    ///
    /// 用**画的时候那个**高度，和 `overlay_click` 同一口径——高度是只涨不缩的，
    /// 按当前内容重算出来的值和屏幕上那个框对不上。
    pub(in crate::cli) fn overlay_span(&self) -> Option<(u16, u16)> {
        let panel = self.overlay.as_ref()?;
        let height = panel.height.max(1);
        let bottom = self.rows.saturating_sub(2);
        Some((bottom.saturating_sub(height.saturating_sub(1)), bottom))
    }

    /// 面板对应的后台任务（有的话）。按 x 停的就是它。
    pub(in crate::cli) fn overlay_job_id(&self) -> Option<String> {
        self.overlay.as_ref().and_then(|panel| panel.job_id.clone())
    }

    /// 面板里点了一下：命中哪一块就开合哪一块。返回真表示这一下被面板吃掉了。
    ///
    /// `row` 是屏幕行；面板第 0 行是标题，内容从第 1 行起。
    pub(in crate::cli) fn overlay_click(&mut self, row: u16) -> bool {
        let rows = self.rows;
        let Some(panel) = &self.overlay else {
            return false;
        };
        // 用**画的时候那个**高度，不是按当前内容重算的——面板高度是只涨不缩的
        // （见 `Overlay::height`），重算出来的值和屏幕上的框对不上，点击就会
        // 整体差几行。
        let height = panel.height.max(1);
        let bottom = rows.saturating_sub(2);
        let top = bottom.saturating_sub(height.saturating_sub(1));
        // 面板上面那截正文也得跟着新内容走。
        //
        // 平时是 `paint` 在管跟随，而面板开着时那条路整个不走——于是正文冻在
        // 点开面板的那一刻，看着像"流式输出停了、时间线不动了"（用户实测）。
        //
        // 落点见 `follow_target`：**和面板没关系**。
        if self.follow {
            self.scroll = self.follow_target();
        }
        // 点在面板外面：让它落回正文的逻辑去。
        if row < top || row > bottom {
            return false;
        }
        let Some(panel) = &mut self.overlay else {
            return false;
        };
        // 上下那两条线、以及紧挨着它们的两行留白，都不是内容。
        let first = top.saturating_add(1 + PANEL_PAD);
        let last = bottom.saturating_sub(1 + PANEL_PAD);
        if row < first || row > last {
            return true;
        }
        let index = panel.scroll + usize::from(row - first);
        let hit = layer_hit(&Layer::Body(&panel.body), &panel.expanded, index);
        if let Some((id, _)) = hit {
            if super::expand::toggle_in(&mut panel.expanded, id, panel.cols) {
                self.invalidate();
            }
        }
        true
    }

    /// 开合面板里第 `index` 行挂着的那一块（测试用）。
    ///
    /// `overlay_click` 要的是屏幕行，而屏幕行要等面板画过一次才算得准
    /// （高度只涨不缩，见 `Overlay::height`）；测试里没有那一次绘制。
    /// 这条走的是同一段命中与开合逻辑，只是省掉了几何换算。
    #[cfg(test)]
    pub(in crate::cli) fn overlay_toggle(&mut self, index: usize) -> bool {
        let Some(panel) = &mut self.overlay else {
            return false;
        };
        let Some((id, _)) = layer_hit(&Layer::Body(&panel.body), &panel.expanded, index) else {
            return false;
        };
        super::expand::toggle_in(&mut panel.expanded, id, panel.cols)
    }

    /// 按新内容刷一遍面板（测试用）。产品里这一步在 `paint_overlay` 里做。
    #[cfg(test)]
    pub(in crate::cli) fn overlay_refresh(&mut self) {
        if let Some(panel) = &mut self.overlay {
            panel.refresh();
        }
    }

    /// 面板里现在是哪几行，**带转义**（测试用）：颜色也要能断言。
    #[cfg(test)]
    pub(in crate::cli) fn overlay_rows_ansi(&self) -> Vec<String> {
        let Some(panel) = &self.overlay else {
            return Vec::new();
        };
        (0..panel.len())
            .map(|index| spans_to_ansi(&panel.row(index)))
            .collect()
    }

    /// 面板里现在是哪几行（测试用）。
    #[cfg(test)]
    pub(in crate::cli) fn overlay_rows(&self) -> Vec<String> {
        let Some(panel) = &self.overlay else {
            return Vec::new();
        };
        (0..panel.len())
            .map(|index| super::ansi::spans_text(&panel.row(index)))
            .collect()
    }

    pub(in crate::cli) fn overlay_open(&self) -> bool {
        self.overlay.is_some()
    }

    pub(in crate::cli) fn scroll_overlay(&mut self, delta: isize) {
        let rows = self.rows;
        let Some(panel) = &self.overlay else {
            return;
        };
        let page = usize::from(
            self.overlay_height(panel.len(), rows)
                .saturating_sub(2)
                .max(1),
        );
        let Some(panel) = &mut self.overlay else {
            return;
        };
        let max = panel.len().saturating_sub(page);
        panel.scroll = if delta < 0 {
            panel.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            panel.scroll.saturating_add(delta as usize)
        }
        .min(max);
        panel.follow = panel.scroll >= max;
        self.invalidate();
    }

    /// 画覆盖层。返回真表示这一帧由面板接管，正文和活动区都不用画了。
    /// 面板里转轮当前该画哪一帧。每帧都会来问，但字形最快 80ms 换一次。
    fn overlay_spinner_frame(&mut self) -> usize {
        let now = std::time::Instant::now();
        let (frame, last) = &mut self.overlay_spinner;
        if last.is_none_or(|last| now.duration_since(last) >= std::time::Duration::from_millis(80))
        {
            *frame = frame.wrapping_add(1);
            *last = Some(now);
        }
        *frame
    }

    pub(in crate::cli) fn paint_overlay(&mut self) -> anyhow::Result<bool> {
        let rows = self.rows;
        let cols = self.cols;
        let spinner = format!(
            "\x1b[36m{}\x1b[39m",
            crate::render::wait_spinner::braille_frame(self.overlay_spinner_frame())
        );
        let Some(panel) = &mut self.overlay else {
            return Ok(false);
        };
        panel.refresh();
        let content = panel.len();
        let wanted = self.overlay_height(content, rows);
        let Some(panel) = &mut self.overlay else {
            return Ok(false);
        };
        // 只涨不缩：后台任务每写一行日志就重算高度的话，面板上边沿会跟着往上
        // 跳，AI 一边输出一边写日志时就是一直在抖。
        panel.height = panel.height.max(wanted).min(rows.saturating_sub(2).max(6));
        let height = panel.height;
        let body = height.saturating_sub(PANEL_CHROME).max(1);
        let total = panel.len();
        let max = total.saturating_sub(usize::from(body));
        if panel.follow {
            panel.scroll = max;
        } else {
            panel.scroll = panel.scroll.min(max);
        }
        let scroll = panel.scroll;
        let title = panel.title.clone();
        let stoppable = panel.job_id.is_some();
        // 左右各留 `PANEL_MARGIN` 列。没有竖线，这一列留白就是边界。
        let left = PANEL_MARGIN;
        let inner = panel_inner_width(cols);
        let lines: Vec<String> = (0..usize::from(body))
            .map(|offset| {
                let index = scroll + offset;
                if index >= total {
                    return String::new();
                }
                let spans = panel.row(index);
                // 点开的那一片在面板里也该有暗底，和正文里一个样子——不然同一个
                // 东西换个地方看就换了张脸。
                let spans = if super::expand::body_in_expansion(&panel.body, &panel.expanded, index)
                {
                    super::select::paint_expansion_bg(spans, inner)
                } else {
                    spans
                };
                // 「正在进行」那一行左边距上的占位格换成当帧的点阵字形。
                crate::render::clip_to_display_width(&spans_to_ansi(&spans), inner)
                    .replace(crate::render::timeline::LIVE_SPINNER_CELL, &spinner)
            })
            .collect();

        let bottom = rows.saturating_sub(2);
        let top = bottom.saturating_sub(height.saturating_sub(1));
        // 落点见 `follow_target`：**和面板没关系**。
        if self.follow {
            self.scroll = self.follow_target();
        }
        let mut stdout = std::io::stdout();
        queue!(stdout, crossterm::cursor::Hide)?;
        if self.needs_clear {
            queue!(stdout, Clear(ClearType::All))?;
            self.needs_clear = false;
        }
        // 面板上下那两截正文照常画，不然那儿会是一片空白。
        self.paint_body_above(&mut stdout, top, bottom)?;

        let range = format!(
            "{}–{}/{total}",
            (scroll + 1).min(total.max(1)),
            (scroll + usize::from(body)).min(total),
        );
        let hint = if stoppable {
            t(
                "Esc close · wheel/PgUp scroll · x stop",
                "Esc 关闭 · 滚轮/PgUp 翻页 · x 停止",
            )
        } else {
            t("Esc close · wheel/PgUp scroll", "Esc 关闭 · 滚轮/PgUp 翻页")
        };
        queue!(
            stdout,
            MoveTo(0, top),
            Clear(ClearType::UntilNewLine),
            MoveTo(left, top),
            Print(frame_line(inner, &title, Some(&range)))
        )?;
        // 上下各一行空白，内容夹在中间。
        for row in [top + 1, bottom.saturating_sub(1)] {
            queue!(stdout, MoveTo(0, row), Clear(ClearType::UntilNewLine))?;
        }
        for (offset, line) in lines.iter().enumerate() {
            let row = top + 1 + PANEL_PAD + u16::try_from(offset).unwrap_or(0);
            if row + PANEL_PAD >= bottom {
                break;
            }
            queue!(
                stdout,
                MoveTo(0, row),
                Clear(ClearType::UntilNewLine),
                MoveTo(left, row),
                Print(line)
            )?;
        }
        queue!(
            stdout,
            MoveTo(0, bottom),
            Clear(ClearType::UntilNewLine),
            MoveTo(left, bottom),
            Print(frame_line(inner, hint, None))
        )?;
        stdout.flush()?;
        Ok(true)
    }

    /// 滚一下视口，然后只重画**面板上面**那一截。
    ///
    /// 提问面板开着的时候屏幕是让出去的，正文那边不画；但用户还是想往回翻
    /// （要答的问题往往就指着上面那几行）。面板自己那几行不碰。
    pub(in crate::cli) fn scroll_above_panel(
        &mut self,
        delta: isize,
        panel_rows: u16,
    ) -> anyhow::Result<()> {
        let top = self.rows.saturating_sub(panel_rows);
        // 临界点和 `paint_overlay` 用同一个口径（没有面板时那个高度）。
        //
        // 两处不一致的话：翻上去之后 `follow` 仍是真、下一帧又被拉回底部
        //（用户实测：面板一开外面就翻不动了）；反过来口径比 paint 宽的话，
        // 翻回底部会停在一个 paint 下一帧又要改的位置上，屏幕跳一下。
        let max = self.follow_target();
        let next = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll.saturating_add(delta as usize)
        };
        self.scroll = next.min(max);
        self.follow = self.scroll >= max;
        self.invalidate();
        let mut stdout = std::io::stdout();
        let rows = self.rows;
        self.paint_body_above(&mut stdout, top, rows)?;
        stdout.flush()?;
        Ok(())
    }

    /// 面板上方那截正文。
    fn paint_body_above(
        &mut self,
        stdout: &mut std::io::Stdout,
        top: u16,
        bottom: u16,
    ) -> anyhow::Result<()> {
        let pad = self.top_pad();
        let paint = |stdout: &mut std::io::Stdout, y: u16| -> anyhow::Result<()> {
            let line = match usize::from(y).checked_sub(pad) {
                Some(offset) => spans_to_ansi(&self.view_row(self.scroll_of() + offset)),
                None => String::new(),
            };
            queue!(
                stdout,
                MoveTo(0, y),
                Clear(ClearType::UntilNewLine),
                Print(&line)
            )?;
            Ok(())
        };
        for y in 0..top {
            paint(stdout, y)?;
        }
        // 面板下面那一行也得擦：面板收起来之前它一直是上一帧的残留。
        for y in bottom.saturating_add(1)..self.rows {
            queue!(stdout, MoveTo(0, y), Clear(ClearType::UntilNewLine))?;
        }
        // 这几行归面板管了，正文那边的缓存作废，收起面板时才会重画。
        self.invalidate();
        Ok(())
    }
}
