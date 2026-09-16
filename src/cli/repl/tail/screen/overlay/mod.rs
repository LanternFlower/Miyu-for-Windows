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
use crossterm::{
    cursor::MoveTo,
    queue,
    style::Print,
    terminal::{Clear, ClearType},
};
use miyu_hosts::render::blocks;
use std::io::Write;

mod log;
mod screen;

use log::*;

/// 面板的内容从哪来。
enum Source {
    /// 渲染方登记的一块（子代理的内层流水账）。
    Block { id: u64, version: u64 },
    /// 盘上的日志文件（后台任务）。任务跑在 daemon 里，日志本来就落盘，
    /// 直接读比再造一条 IPC 分页通道省事。
    File {
        path: std::path::PathBuf,
        size: u64,
        /// 上一次重读是什么时候。子代理一边跑一边写，文件每帧都在长，帧帧重读
        /// 重排（含正文的 markdown 渲染）把画面板的那一帧拖慢，转轮就一顿一顿。
        last_reload: Option<std::time::Instant>,
    },
}

/// 日志文件在长的时候最快多久重读一次。
const LOG_RELOAD_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

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
    let tail_width = miyu_hosts::render::visible_width(&tail);
    let label_room = width.saturating_sub(tail_width + 8);
    let label = miyu_hosts::render::clip_to_display_width(label, label_room.max(4));
    let label_width = miyu_hosts::render::visible_width(&label);
    let fill = width.saturating_sub(label_width + tail_width + 6).max(1);
    format!("{DIM}── {label} {}{tail}──{RESET}", "─".repeat(fill))
}

pub(in crate::cli) struct Overlay {
    source: Source,
    /// 跑的是哪条命令。标题栏那个 title 是短标签(模型给的话只有 16 字符),
    /// 看不出真正在跑什么,所以日志上面原样铺一行(用户 09-14)。
    command: String,
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
            command: String::new(),
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
        command: String,
        cols: usize,
    ) -> Self {
        let mut panel = Self {
            source: Source::File {
                path,
                size: 0,
                last_reload: None,
            },
            job_id,
            title,
            command,
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
        let Source::File {
            path,
            size,
            last_reload,
        } = &mut self.source
        else {
            return;
        };
        if !force && last_reload.is_some_and(|last| last.elapsed() < LOG_RELOAD_INTERVAL) {
            return;
        }
        let current = std::fs::metadata(&*path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if !force && current == *size {
            return;
        }
        *size = current;
        *last_reload = Some(std::time::Instant::now());
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
        let rail = miyu_hosts::render::timeline::panel_rail();
        // 抬头能占多宽：和前台那种面板同一把尺（`panel_step_line` 自己还会再
        // 裁一刀兜底）。再扣掉图标那一格和尾巴上的 ` · ok`。
        let head_width = miyu_hosts::render::timeline::panel_step_width_for_head().max(12);
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
            // 标题栏那个 title 是短标签,看不出在跑什么;日志上面原样铺一行。
            let mut head = Vec::new();
            let command = self.command.trim().to_string();
            if !command.is_empty() {
                head.extend(
                    miyu_hosts::render::wrap_display_text(&format!("$ {command}"), width)
                        .into_iter()
                        .map(|piece| format!("{indent}{piece}")),
                );
                head.push(String::new());
            }
            return head
                .into_iter()
                .chain(text.lines().flat_map(|line| {
                    if line.trim().is_empty() {
                        return vec![String::new()];
                    }
                    miyu_hosts::render::wrap_display_text(line, width)
                        .into_iter()
                        .map(|piece| format!("{indent}{piece}"))
                        .collect::<Vec<_>>()
                }))
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
                    miyu_hosts::render::timeline::render_speech_lines(
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
                format!(" · {}", miyu_base::i18n::text("running", "运行中"))
            } else {
                String::new()
            };
            // 收缩行合着的时候是 `›`，点开才翻成 `⌄`（和主线那条一样）。
            let glyph = if step.glyph == SUMMARY_GLYPH {
                miyu_hosts::render::timeline::fold_glyph_closed()
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
                    miyu_base::i18n::text("thought", "已思考"),
                    miyu_hosts::render::timeline::PEEK_SEP,
                    miyu_hosts::render::timeline::peek_tail(&step.head, head_width)
                )
            } else {
                step.head.clone()
            };
            let head = miyu_hosts::render::clip_to_display_width(&head, head_width);
            // 前台那种面板（走事件）和这种（读日志）用的是**同一份**排版代码：
            // 取数的地方不同，长相不该不同（用户原话：后台子代理和前台子代理
            // 应该是一回事啊，为什么感觉你做出来两个浮层）。
            // 正在跑／正在准备的那一行左边距上转着点阵，和主线一样。
            let line = if step.running || step.preparing {
                miyu_hosts::render::timeline::panel_live_step_line(
                    glyph,
                    &format!("{head}{status}"),
                )
            } else {
                miyu_hosts::render::timeline::panel_step_line(
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
        let head_width = miyu_hosts::render::timeline::panel_step_width_for_head().max(12);
        let mut rows: Vec<String> = Vec::new();
        for (inner_index, step) in fold.inner.iter().enumerate() {
            if inner_index > 0 {
                rows.push(miyu_hosts::render::timeline::panel_rail());
            }
            let head = step_head(step, head_width);
            let head = miyu_hosts::render::clip_to_display_width(&head, head_width);
            let inner_line = miyu_hosts::render::timeline::panel_step_line(
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
        detail.push(miyu_hosts::render::timeline::fold_line_open(line));
        detail.push(miyu_hosts::render::timeline::panel_rail());
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

#[cfg(test)]
mod test_support;
