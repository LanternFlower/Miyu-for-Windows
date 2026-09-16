//! 后台任务日志 → 时间线：流水账的解析与排版。从 `src/cli/repl/tail/screen/overlay.rs` 搬来（09-16 拆分），逻辑未改。

/// 读文件末尾 `budget` 字节。从中间切开的第一行丢掉，免得开头是半个字符。
pub(super) fn read_tail(path: &std::path::Path, budget: u64) -> String {
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
pub(super) struct LogStep {
    pub(super) glyph: String,
    pub(super) green: bool,
    pub(super) head: String,
    pub(super) status: Option<&'static str>,
    pub(super) body: Vec<String>,
    /// 这一步是不是"想"。连续的思考要并成一条，和主线一个规矩。
    pub(super) thinking: bool,
    /// 这一步是不是它说的正文。正文没有抬头，整段就是内容。
    pub(super) speech: bool,
    /// 这一步花了多久（`[结果]` 行上带的 `· 1.2s`）。收缩行的 Worked for 靠它加。
    elapsed: Option<std::time::Duration>,
    /// 收缩行：收起来的那几步。点开收缩行看到的是它们，每一步再点开才是正文。
    pub(super) inner: Vec<LogStep>,
    /// 日志末尾那个还没有结果的调用：它正在跑。
    pub(super) running: bool,
    /// 日志末尾的 `[准备]`：参数还在流。
    pub(super) preparing: bool,
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
            miyu_hosts::render::tool_glyph_for(name.trim()).to_string(),
            text.trim().to_string(),
        ),
        None => ("\u{f013}".to_string(), rest.trim().to_string()),
    }
}

/// 「交给它的差事」那一步的图标（文档）。
pub(super) const PROMPT_GLYPH: &str = "\u{f4a5}";

/// 收缩行的图标。和主线那条 `⌄ Worked for …` 一个样子。
pub(super) const SUMMARY_GLYPH: &str = "⌄";

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
    let summary = miyu_hosts::render::timeline::summary_line(
        elapsed,
        miyu_hosts::render::timeline::Counts {
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
pub(super) fn log_steps(text: &str) -> Vec<LogStep> {
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
                    miyu_base::i18n::text("prompt", "提示词"),
                    miyu_hosts::render::timeline::PEEK_SEP
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
            // 图标是那个工具自己的（`[准备] edit\t准备编辑` → 铅笔），老日志没带
            // 工具 id 的退回通用齿轮。
            let (glyph, phase) = split_tool_line(rest);
            steps.push(LogStep {
                glyph,
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
                // 正文段也一样：桥按自然段落盘，一段里的换行原样写着（标题、
                // 表格行、列表项都是这么来的），丢掉就是整段缺句子、表格只剩
                // 表头（用户实测截图）。
                Some(last) if last.thinking || last.speech => last.body.push(line.to_string()),
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
pub(super) fn step_head(step: &LogStep, head_width: usize) -> String {
    if !step.thinking {
        return step.head.clone();
    }
    let mut head = miyu_base::i18n::text("thought", "已思考").to_string();
    if let Some(secs) = step
        .elapsed
        .and_then(miyu_hosts::render::timeline::reported_seconds)
    {
        head.push_str(" · ");
        head.push_str(&secs);
    }
    head.push_str(miyu_hosts::render::timeline::PEEK_SEP);
    head.push_str(&miyu_hosts::render::timeline::peek_tail(
        &step.head, head_width,
    ));
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
    let Some(secs) = miyu_hosts::render::timeline::reported_seconds(elapsed) else {
        return head.to_string();
    };
    match head.split_once(" · ") {
        Some((name, rest)) => format!("{name} · {secs} · {rest}"),
        None => format!("{head} · {secs}"),
    }
}

/// 一步展开之后看到的东西：头行 + 空行 + 折好行的正文。和主线的 `step_detail`
/// 是同一个形状。
pub(super) fn log_step_detail(line: &str, step: &LogStep) -> Vec<String> {
    let inner = miyu_hosts::render::timeline::panel_detail_width();
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
        for piece in miyu_hosts::render::wrap_display_text(text, inner) {
            body.push(format!("\x1b[2m{color}{piece}\x1b[0m"));
        }
    }
    miyu_hosts::render::timeline::panel_step_detail(line, &body)
}
