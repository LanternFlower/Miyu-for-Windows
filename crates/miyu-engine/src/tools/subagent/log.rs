use super::*;

/// 流水账开头写一条「差事」，面板里就是第一步，点开看全文。
///
/// 后台子代理跑起来之后，能看到的全是它自己的动作；它到底被要求干什么，只有
/// 派它出去的那一轮知道。隔十分钟回来看这个面板的人是没有那一轮的。
pub(super) fn write_subagent_prompt_header(log_path: &std::path::Path, prompt: &str) {
    let line = prompt_header_line(prompt);
    if line.is_empty() {
        return;
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut file| {
            use std::io::Write as _;
            writeln!(file, "{line}")
        });
}

/// prompt → 流水账里那一行。多行压成一行：流水账是按行读的，`\u{1}` 在正文里
/// 不会出现，面板那边照它拆回来。空 prompt 返回空串（不写）。
fn prompt_header_line(prompt: &str) -> String {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return String::new();
    }
    format!("[提示] {}", prompt.replace('\r', "").replace('\n', "\u{1}"))
}

/// Bridge a detached subagent's progress stream into its job log so
/// `job_status` reads live progress the same way it reads command output.
pub(super) fn spawn_subagent_log_bridge(
    job_id: String,
    log_path: std::path::PathBuf,
) -> crate::tools::ToolProgress {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        // 思考和正文都是**逐 delta** 来的。一条一行的话日志会变成每行一个词的
        // 字符梯，谁也读不下去（用户实测截图：整屏 `[正文] the` / `[正文] and`）。
        // 攒成段落，遇到别的事件或段落够长了才落盘。
        let mut thinking = String::new();
        let mut speech = String::new();
        // 上一次内层调用是什么时候发出的：结果回来时算耗时写进 `[结果]`。
        // 流水账里没有时间戳，面板那边"这一步花了多久""这一段 Worked for 多久"
        // 只能靠这个（用户实测：后台面板的收缩行没有 Worked for）。
        let mut last_call: Option<std::time::Instant> = None;
        // 这一段思考从什么时候开始的：落成 `[思考]` 行时把时长写在最前面。
        let mut thinking_since: Option<std::time::Instant> = None;
        while let Some(event) = receiver.recv().await {
            let crate::tools::ToolProgressEvent::Message(message) = event else {
                continue;
            };
            // 原始标记上 SSE(网页端据 job_id 渲染子过程流,与前台子代理工具行
            // 同款);人读的行落任务日志(job status 读它)。
            crate::tools::jobs::publish_job_progress(&job_id, &message);
            if let Some(text) = message.strip_prefix("__subagent_metric__") {
                // 制表符分隔：`<给人看的那串>\t<数字>\t<人话>`
                //（见 `SubagentRunner::report_metric`）。中途的量报只刷状态行上
                // 那串数，不落流水账——它一秒来好几次，落进去会把时间线撑满。
                let mut parts = text.split('\t');
                let display = parts.next().unwrap_or_default().trim().to_string();
                let raw = parts.next().and_then(|value| value.trim().parse().ok());
                crate::tools::jobs::set_metric(&job_id, &display, raw);
                continue;
            }
            let mut lines: Vec<String> = Vec::new();
            if let Some(text) = message.strip_prefix("__subagent_reasoning__") {
                flush_stream_buffer(&mut speech, "[正文]", &mut lines);
                if thinking.is_empty() && thinking_since.is_none() {
                    thinking_since = Some(std::time::Instant::now());
                }
                accumulate_stream(&mut thinking, text, "[思考]", &mut lines);
            } else if let Some(text) = message.strip_prefix("__subagent_content__") {
                flush_stream_buffer(&mut thinking, "[思考]", &mut lines);
                accumulate_stream(&mut speech, text, "[正文]", &mut lines);
            } else {
                flush_stream_buffer(&mut thinking, "[思考]", &mut lines);
                flush_stream_buffer(&mut speech, "[正文]", &mut lines);
                let elapsed = if message.starts_with("__subtool_call__") {
                    last_call = Some(std::time::Instant::now());
                    None
                } else if message.starts_with("__subtool_result__") {
                    last_call.take().map(|since| since.elapsed())
                } else {
                    None
                };
                let line = readable_subagent_log_line_timed(&message, elapsed);
                if !line.is_empty() {
                    lines.push(line);
                }
            }
            stamp_thought_lines(&mut lines, &mut thinking_since);
            if lines.is_empty() {
                continue;
            }
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .and_then(|mut file| {
                    use std::io::Write as _;
                    for line in &lines {
                        writeln!(file, "{line}")?;
                    }
                    Ok(())
                });
        }
        // 收尾：最后那段没等到分隔符的也要落盘。
        let mut lines: Vec<String> = Vec::new();
        flush_stream_buffer(&mut thinking, "[思考]", &mut lines);
        flush_stream_buffer(&mut speech, "[正文]", &mut lines);
        stamp_thought_lines(&mut lines, &mut thinking_since);
        if !lines.is_empty() {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .and_then(|mut file| {
                    use std::io::Write as _;
                    for line in &lines {
                        writeln!(file, "{line}")?;
                    }
                    Ok(())
                });
        }
    });
    crate::tools::ToolProgress::new(sender)
}

/// 刚落下来的 `[思考]` 行带上这段想了多久：`[思考] 1.2s\t正文`。面板那边按它
/// 报「已思考 · 1.2s」，收缩行的 Worked for 也把它算进去。
fn stamp_thought_lines(lines: &mut [String], thinking_since: &mut Option<std::time::Instant>) {
    for line in lines.iter_mut() {
        let Some(text) = line.strip_prefix("[思考] ") else {
            continue;
        };
        let Some(since) = thinking_since.take() else {
            break;
        };
        let secs = miyu_base::durations::format_seconds(since.elapsed());
        *line = format!("[思考] {secs}\t{text}");
    }
}

/// 把一小段流式文本攒进缓冲，攒够一个自然段（空行）或够长了就落一条。
pub(super) fn accumulate_stream(
    buffer: &mut String,
    text: &str,
    tag: &str,
    lines: &mut Vec<String>,
) {
    buffer.push_str(text);
    while let Some(index) = buffer.find("\n\n") {
        let chunk: String = buffer.drain(..index + 2).collect();
        if !chunk.trim().is_empty() {
            lines.push(format!("{tag} {}", chunk.trim()));
        }
    }
    // 一直不出现空行的话也不能无限攒下去。
    if buffer.chars().count() > 600 {
        lines.push(format!("{tag} {}", buffer.trim()));
        buffer.clear();
    }
}

/// 把缓冲里剩的那截落成一条（别的事件来了、或者收尾了）。
pub(super) fn flush_stream_buffer(buffer: &mut String, tag: &str, lines: &mut Vec<String>) {
    if buffer.trim().is_empty() {
        buffer.clear();
        return;
    }
    lines.push(format!("{tag} {}", buffer.trim()));
    buffer.clear();
}

/// 内层工具事件压成一句人话。原样贴 JSON 的话日志里全是转义引号。
fn subtool_summary(json: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json.trim()) else {
        return json.trim().to_string();
    };
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    // 前面带上工具 id（制表符分隔）：面板那边要按 id 挑图标，光有中文名挑不出来
    // ——所有工具就只能共用一个齿轮了。读日志的人看不到它（渲染时会切掉）。
    let mut out = format!("{name}\t{}", crate::tools::readable_tool_name(name));
    if let Some(ok) = value.get("ok").and_then(serde_json::Value::as_bool) {
        out.push_str(if ok { " ok" } else { " err" });
    }
    if let Some(args) = value.get("args").and_then(serde_json::Value::as_str) {
        let args = args.trim();
        if !args.is_empty() {
            // 先按工具自己的规矩摘一句主题（命令文本、检索词、路径……），摘不
            // 出来就把参数的值串起来，**不**原样甩 JSON——`{"action": "info",
            // "package_name": "zzq"}` 在面板里读起来是一团括号引号（用户实测：
            // 浮层的参数窥视是裸 JSON）。什么都摘不出来就不带主题。
            if let Some(subject) = crate::tools::tool_peek(name, args) {
                out.push_str(" · ");
                out.push_str(&miyu_base::terminal::clip_to_display_width(&subject, 200));
            }
        }
    }
    out
}

/// 结果事件摊成 `[结果]` + 若干 `[输出]`。
///
/// 只写 `[结果]` 的话，面板里那一步点开是空的——那行里已经有的东西再说一遍而已
/// （用户实测：浮层里这些工具展开都没内容）。真正值得看的是工具吐了什么，而
/// `__subtool_result__` 本来就带着（`clip_detail` 已经截过）。这儿再收一道，
/// 免得一条 8KB 的输出把流水账撑成日志本体。
fn subtool_result_lines(json: &str, elapsed: Option<Duration>) -> String {
    let mut out = format!("[结果] {}", subtool_summary(json));
    // 耗时紧跟在 ok/err 后面：`运行命令 ok · 1.2s · ls`。面板去掉 ok 之后就是
    // 主线那一行的样子（名字 · 秒数 · 窥视）。
    // 再短也写：一段里几个快工具加起来才够得上一个 Worked for。
    if let Some(elapsed) = elapsed {
        let secs = miyu_base::durations::format_seconds(elapsed);
        for status in [" ok", " err"] {
            if let Some(index) = out.find(&format!("{status} · ")) {
                out.insert_str(index + status.len(), &format!(" · {secs}"));
                break;
            }
            if out.ends_with(status) {
                out.push_str(&format!(" · {secs}"));
                break;
            }
        }
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json.trim()) else {
        return out;
    };
    let Some(output) = value.get("output").and_then(serde_json::Value::as_str) else {
        return out;
    };
    for line in output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(LOG_OUTPUT_LINES)
    {
        // 工具吐的是**原始输出**，里面有转义序列、回车、制表符。流水账是按行读
        // 的纯文本，面板把它当普通字符排版——原样写进去，一行的真实宽度和算出来
        // 的宽度就对不上，右边那根竖线跟着参差不齐。
        let line = miyu_base::terminal::strip_ansi_text(line);
        let line = line
            .chars()
            .map(|ch| if ch == '\t' { ' ' } else { ch })
            .filter(|ch| !ch.is_control())
            .collect::<String>();
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        out.push_str("\n[输出] ");
        out.push_str(&miyu_base::terminal::clip_to_display_width(line, 400));
    }
    out
}

/// 一次工具结果最多往流水账里写几行输出。
const LOG_OUTPUT_LINES: usize = 24;

/// 同上，`elapsed` 是这次内层调用从发出到结果回来花的时间（只有结果事件带）。
pub(super) fn readable_subagent_log_line_timed(message: &str, elapsed: Option<Duration>) -> String {
    if let Some(name) = message.strip_prefix("__subtool_preparing__") {
        // 参数还在流：面板把它当"正在准备"那一行。它不是一步，只有作为日志末尾
        // 那一行时才有意义，读日志的人看到它也只当"刚才准备过"。
        let name = name.trim();
        let phase = crate::tools::preparing_phase(name).unwrap_or("");
        return format!("[准备] {name}\t{phase}");
    }
    if let Some(text) = message.strip_prefix("__subagent_reasoning__") {
        let text = text.trim();
        if text.is_empty() {
            return String::new();
        }
        return format!("[思考] {text}");
    }
    if let Some(text) = message.strip_prefix("__subagent_content__") {
        let text = text.trim();
        if text.is_empty() {
            return String::new();
        }
        return format!("[正文] {text}");
    }
    if let Some(text) = message.strip_prefix("__subtool_call__") {
        return format!("[工具] {}", subtool_summary(text));
    }
    if let Some(text) = message.strip_prefix("__subtool_result__") {
        return subtool_result_lines(text, elapsed);
    }
    if let Some(text) = message.strip_prefix("__subagent_brief__") {
        // 任务简介（Full 档才发）里带着 prompt——正是面板第一步要的那份。
        // 认下来，免得它以无标签原文的身份漏进流水账。
        let prompt = serde_json::from_str::<serde_json::Value>(text.trim())
            .ok()
            .and_then(|value| {
                value
                    .get("prompt")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        return prompt_header_line(&prompt);
    }
    // 中途的量报只用来刷标题和状态行，不进流水账——每调一次工具记一条
    // 「统计」的话，面板里的时间线会被这些节点撑满。跑完那一次走
    // `__subagent_stats__`，那条是留底的。
    if message.starts_with("__subagent_metric__") {
        return String::new();
    }
    if let Some(text) = message.strip_prefix("__subagent_stats__") {
        return format!("[统计] {}", text.trim());
    }
    message.trim().to_string()
}
