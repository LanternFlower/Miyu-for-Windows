use super::subagent_runner::{ProgressMode, SubagentProgress, SubagentRunner, SubagentStats};
use super::{ToolRegistry, ToolSpec};
use crate::agent::AgentMode;
use crate::config::{AppConfig, ModelTier};
use crate::llm::OpenAiCompatibleClient;
use crate::paths::MiyuPaths;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

const SUBAGENT_SYSTEM_PROMPT: &str = include_str!("../prompts/subagent-general.md");

/// 前台子代理的原始进度标记流,按工具调用 id 暂存。回合收尾 derive_tool_flow 时取走
/// 挂到那次调用上落库,网页端刷新/回看时回放子过程时间线(#9:刷新丢内容)。
/// 进程内、封顶,取走即清;后台子代理走 jobs 的 trace,不走这。
fn subagent_traces() -> &'static Mutex<HashMap<String, Vec<String>>> {
    static TRACES: std::sync::OnceLock<Mutex<HashMap<String, Vec<String>>>> =
        std::sync::OnceLock::new();
    TRACES.get_or_init(|| Mutex::new(HashMap::new()))
}

const MAX_CALL_TRACE: usize = 4000;

/// 一条子代理进度标记(`__subagent_*` / `__subtool_*`)入某次调用的缓冲。
pub fn record_subagent_trace(call_id: &str, marker: &str) {
    if call_id.is_empty() {
        return;
    }
    let mut map = subagent_traces().lock().unwrap();
    let buf = map.entry(call_id.to_string()).or_default();
    buf.push(marker.to_string());
    if buf.len() > MAX_CALL_TRACE {
        let overflow = buf.len() - MAX_CALL_TRACE;
        buf.drain(0..overflow);
    }
}

/// 取走某次调用的标记流(回合最终落库时用,取完清掉,避免长会话堆积)。
pub fn take_subagent_trace(call_id: &str) -> Vec<String> {
    subagent_traces()
        .lock()
        .unwrap()
        .remove(call_id)
        .unwrap_or_default()
}

/// 只读某次调用的标记流,不清空。回合中途的检查点(`checkpoint_tool_flow`)用它:
/// 检查点在一个回合里会跑多次,若也用 `take` 会把标记流提前抽干,等回合收尾真正
/// 落库时(`stream.rs`)就只剩空的了(#5a:前台子代理刷新丢子过程的真因)。
pub fn peek_subagent_trace(call_id: &str) -> Vec<String> {
    subagent_traces()
        .lock()
        .unwrap()
        .get(call_id)
        .cloned()
        .unwrap_or_default()
}

/// 一条进度是不是子代理子过程标记(据此决定要不要留进 trace)。
pub fn is_subagent_marker(message: &str) -> bool {
    message.starts_with("__subagent") || message.starts_with("__subtool")
}

/// dev 子代理的系统提示词由三段拼成:用户的 dev 提示词(与 dev 会话同一份
/// 真相源)、主机环境块、这一句交付约定。三段都是同一会话内的常量,拼出的
/// 前缀字节稳定,多次 dev 子代理之间照样命中供应商缓存。
///
/// 约定只留一句:主体布置任务时会把目标写进 prompt,但「回话对象是主 agent
/// 而不是用户、没有第二轮」这件事它自己看不出来——dev 提示词里也没有。
const SUBAGENT_DEV_CONTRACT: &str = "Your reply goes back to the agent that delegated this task, not to a user, and there is no second round: finish the work yourself and end with what you did, what the result was, and anything the caller must know.";

/// 子代理不再分类(08-17):任务由主体布置,工具就沿用主体的目录。
/// 原来的 explore 是一份硬白名单(read_file/glob/grep/check_os_info/
/// read_clipboard/web_fetch/web_search),而 dev 目录根本不注册前五个——
/// dev 下的 explore 只剩 web 两件套,描述却还在承诺 7 个工具。分类本身
/// 就是这类漂移的来源,连同 275 字符的 subagent_type 参数一起退场。
///
/// 递归防护保留:这份排除表继续把 subagent/deep_research、技能创作、闹钟和
/// 娱乐类工具挡在子代理之外。
pub(in crate::tools) const SUBAGENT_EXCLUDED: &[&str] = &[
    "subagent",
    // 09-11 改名前的旧名,按名匹配的排除表留着不花钱。
    "task",
    "task_agent",
    "send_subagent_message",
    "deep_research",
    "load_skill",
    "manage_skill",
    "alarm",
    "use_meme",
    "manage_meme",
    "generate_image",
    "print_image",
    "search_web_images",
    "divine",
];

const SUBAGENT_TOOL_TIMEOUT: u64 = 120;

#[derive(Clone)]
struct SubagentContext {
    config: AppConfig,
    paths: MiyuPaths,
    tools: ToolRegistry,
}

pub fn register(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    tools: ToolRegistry,
) {
    let context = SubagentContext {
        config,
        paths,
        tools,
    };
    registry.register(ToolSpec::new_with_progress(
        "subagent",
        "Launch a subagent to handle a complex task independently. The subagent has its own system prompt, tool set, and LLM loop, and returns its final text to the main agent. Set dev=true for coding work.",
        json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Short task description for progress display."
                },
                "prompt": {
                    "type": "string",
                    "description": "Detailed task prompt. Must include full context, goals, and output requirements since the subagent has no access to the main agent's conversation history."
                },
                "dev": {
                    "type": "boolean",
                    "description": "Run the subagent in development mode: the development system prompt plus a minimal coding tool set. Turn it on for every coding task."
                },
                "max_steps": {
                    "type": "integer",
                    "description": "Optional tool-call budget. Unlimited by default: the subagent ends when the task is done. Set a number only when you want a hard cap."
                },
                "background": {
                    "type": "boolean",
                    "description": "Run the subagent detached in the background: returns a job_id immediately; check with job(action=status) (its log holds live progress) and you are woken automatically on completion. Use for long research/tasks that should not block the conversation."
                },
                "resume_id": {
                    "type": "string",
                    "description": "Optional. When a previous task failed with a resume_id in its error, pass it here to continue that subagent from its last completed tool round instead of starting over (checkpoints persist on disk and survive a daemon restart, kept 2h)."
                },
                "tier": {
                    "type": "string",
                    "enum": ["lite", "cheap", "standard", "flagship"],
                    "description": "Optional model tier by task difficulty: lite for trivial lookups and formatting, cheap for simple tool-using work, standard for regular multi-step work (default), flagship for hard reasoning. Every tier has the full tool set; an unconfigured tier falls back to the main model."
                }
            },
            "required": ["description", "prompt"],
            "additionalProperties": false
        }),
        move |args, progress| {
            let context = context.clone();
            async move { run_subagent(args, context, progress).await }
        },
    ).writes());

    // 给正在运行的后台子代理发一条 follow-up 排队指令(像给主会话排队消息),
    // 子代理下一步开始前取走、并入对话——用于运行途中调整任务目标。
    registry.register(ToolSpec::new(
        "send_subagent_message",
        "Queue a follow-up instruction to a RUNNING background subagent (one you started with task(background=true)). It works like queuing a message to the main agent mid-run: the subagent picks it up before its next step, so you can steer or adjust its goal while it works. Pass the job_id from the background task's result. Only works while that subagent is still running.",
        json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The background subagent's job_id, from the task(background=true) result."
                },
                "message": {
                    "type": "string",
                    "description": "The follow-up instruction to inject into the running subagent."
                }
            },
            "required": ["job_id", "message"],
            "additionalProperties": false
        }),
        move |args| async move { send_subagent_message(args) },
    ));
}

fn send_subagent_message(args: Value) -> Result<String> {
    let job_id = args
        .get("job_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let message = args
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if job_id.is_empty() {
        bail!("job_id is required (the background subagent's id from the task result)");
    }
    if message.is_empty() {
        bail!("message is required");
    }
    if crate::tools::subagent_runner::deliver_to_subagent(&job_id, &message) {
        Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "job_id": job_id,
            "queued": message,
            "note": "The subagent will incorporate this before its next step."
        }))?)
    } else {
        let running = crate::tools::subagent_runner::running_subagent_ids();
        let hint = if running.is_empty() {
            "no background subagent is running right now".to_string()
        } else {
            format!("running background subagents: {}", running.join(", "))
        };
        bail!(
            "no running background subagent with job_id '{job_id}' (it may have already finished). {hint}"
        )
    }
}

#[derive(Clone)]
struct SubagentParams {
    description: String,
    prompt: String,
    resume_id: Option<String>,
    max_steps: usize,
    tier: ModelTier,
    dev: bool,
}

/// Session linkage captured while still inside the turn scope — a detached
/// background subagent loses the task-locals, so the audit anchor must be
/// resolved before spawning.
#[derive(Clone)]
struct AuditAnchor {
    parent: Option<String>,
    persona: String,
}

fn parse_params(args: &Value) -> Result<SubagentParams> {
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if description.is_empty() {
        bail!("description is required");
    }
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if prompt.is_empty() {
        bail!("prompt is required");
    }
    let resume_id = args
        .get("resume_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    // 0 = 不限步数(runner 语义):默认让子代理自然结束,预算仅在调用方
    // 显式给出 max_steps 时生效。
    let max_steps = args
        .get("max_steps")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(0);
    let tier = args
        .get("tier")
        .and_then(Value::as_str)
        .and_then(ModelTier::from_str)
        .unwrap_or(ModelTier::Standard);
    let dev = args.get("dev").and_then(Value::as_bool).unwrap_or(false);
    Ok(SubagentParams {
        description,
        prompt,
        resume_id,
        max_steps,
        tier,
        dev,
    })
}

async fn run_subagent(
    args: Value,
    context: SubagentContext,
    progress: crate::tools::ToolProgress,
) -> Result<String> {
    let params = parse_params(&args)?;
    let anchor = AuditAnchor {
        parent: crate::tools::workspace::try_session().map(|session| session.to_string()),
        persona: context.config.active_persona_scope(),
    };
    if args
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return spawn_background(context, params, anchor, progress).await;
    }
    // 前台子代理阻塞在本次调用里,主体无从中途插话,不开收件箱(None)。
    Ok(run_core(context, progress, params, anchor, None)
        .await?
        .output)
}

/// 一次子代理运行的结果。
///
/// `state` 以前是后台路径把 `output` 当 JSON 反解出来的——而 08-21 的
/// token-diet 把成功路径改成了纯文本,那次反解从此永远失败、悄悄退化成
/// "completed",`budget_reached` 被当成正常完成上报。现在直接带出来。
struct SubagentRun {
    output: String,
    state: &'static str,
}

/// 回合作用域(沙盒策略、工作区、会话身份)不跟着 `tokio::spawn` 走:后台
/// 子代理起在一条新任务上,task-local 到那边全是空的。后果不是显示问题
/// ——成员回合的后台子代理会跑在 Landlock 之外,工具的工作目录也退回
/// daemon 的 cwd。在还看得见的地方抓下来,进了后台原样套回去。
async fn with_turn_scope<F>(
    sandbox: Option<std::sync::Arc<crate::tools::sandbox::SandboxPolicy>>,
    workspace: Option<std::path::PathBuf>,
    session: Option<std::sync::Arc<str>>,
    future: F,
) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut future: std::pin::Pin<Box<dyn std::future::Future<Output = F::Output> + Send>> =
        Box::pin(future);
    if let Some(session) = session {
        future = Box::pin(crate::tools::workspace::with_session(session, future));
    }
    if let Some(workspace) = workspace {
        future = Box::pin(crate::tools::workspace::with_workspace(workspace, future));
    }
    // `None` 也照样套:显式「这一段没有策略」与回合里的语义一致。
    crate::tools::sandbox::with_sandbox(sandbox, future).await
}

/// Detach the subagent run behind the shared background-job registry: its
/// progress streams into the job log, and completion goes through the same
/// wake path as background commands.
async fn spawn_background(
    context: SubagentContext,
    params: SubagentParams,
    anchor: AuditAnchor,
    progress: crate::tools::ToolProgress,
) -> Result<String> {
    let description = params.description.clone();
    let prompt = params.prompt.clone();
    // 后台子代理起在 tokio::spawn 的新任务上,回合的 task-local(工作区/会话/
    // 沙盒)到那儿全空了:相对路径退回 daemon 的 cwd、Landlock 失效、且
    // mcp_bridge_config 因 try_session()=None 返回 None(claude-code 拿不到 Miyu 桥)。
    // 在还处于父回合作用域的此刻抓下来,由 with_turn_scope 在 spawn 里套回去。
    let sandbox = crate::tools::sandbox::current_sandbox();
    let workspace = crate::tools::workspace::try_workspace();
    let session = crate::tools::workspace::try_session();
    crate::tools::jobs::spawn_background_subagent(
        None,
        &description,
        params.dev,
        &progress,
        move |job_id, log_path| async move {
            write_subagent_prompt_header(&log_path, &prompt);
            let bridge = spawn_subagent_log_bridge(job_id.clone(), log_path.clone());
            // 后台子代理:用后台任务 id 作收件箱键,主体可用 send_subagent_message
            // 中途投递 follow-up;主体从后台返回里拿到这个 job_id。工作区/会话/沙盒
            // 由 with_turn_scope 套回(见上)。
            let run = with_turn_scope(
                sandbox,
                workspace,
                session,
                run_core(context, bridge, params, anchor, Some(job_id.clone())),
            )
            .await;
            let state_label = match &run {
                Ok(run) => run.state,
                Err(_) => "error",
            };
            let tail = match &run {
                Ok(run) => format!(
                    "\n{}\n{}\n",
                    crate::tools::jobs::SUBAGENT_RESULT_MARKER,
                    run.output
                ),
                Err(error) => format!("\n{}\n{error}\n", crate::tools::jobs::SUBAGENT_ERROR_MARKER),
            };
            let _ = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .and_then(|mut file| {
                    use std::io::Write as _;
                    file.write_all(tail.as_bytes())
                });
            tracing::debug!(job_id = %job_id, state = %state_label, "background subagent finished");
            match state_label {
                "completed" | "budget_reached" => {
                    crate::tools::jobs::JobState::Exited { code: Some(0) }
                }
                "timeout" => crate::tools::jobs::JobState::TimedOut,
                _ => crate::tools::jobs::JobState::Exited { code: None },
            }
        },
    )
    .await
}

/// 流水账开头写一条「差事」，面板里就是第一步，点开看全文。
///
/// 后台子代理跑起来之后，能看到的全是它自己的动作；它到底被要求干什么，只有
/// 派它出去的那一轮知道。隔十分钟回来看这个面板的人是没有那一轮的。
fn write_subagent_prompt_header(log_path: &std::path::Path, prompt: &str) {
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
fn spawn_subagent_log_bridge(
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
        let secs = crate::render::timeline::format_seconds(since.elapsed());
        *line = format!("[思考] {secs}\t{text}");
    }
}

/// 把一小段流式文本攒进缓冲，攒够一个自然段（空行）或够长了就落一条。
fn accumulate_stream(buffer: &mut String, text: &str, tag: &str, lines: &mut Vec<String>) {
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
fn flush_stream_buffer(buffer: &mut String, tag: &str, lines: &mut Vec<String>) {
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
    let mut out = format!("{name}\t{}", crate::render::readable_tool_name(name));
    if let Some(ok) = value.get("ok").and_then(serde_json::Value::as_bool) {
        out.push_str(if ok { " ok" } else { " err" });
    }
    if let Some(args) = value.get("args").and_then(serde_json::Value::as_str) {
        let args = args.trim();
        if !args.is_empty() {
            // 先按工具自己的规矩摘一句主题（命令文本、检索词、路径……），
            // 摘不出来才退回原文。直接贴 JSON 的话日志里全是
            // `{"command": "cd /very/long/path && …"}` 这种没法读的东西，
            // 面板里那条时间线也就跟着糊了。
            let subject =
                crate::render::tool_subject(name, args).unwrap_or_else(|| args.to_string());
            out.push_str(" · ");
            out.push_str(&crate::render::clip_to_display_width(&subject, 200));
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
        let secs = crate::render::timeline::format_seconds(elapsed);
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
        let line = crate::render::strip_ansi_text(line);
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
        out.push_str(&crate::render::clip_to_display_width(line, 400));
    }
    out
}

/// 一次工具结果最多往流水账里写几行输出。
const LOG_OUTPUT_LINES: usize = 24;

fn readable_subagent_log_line(message: &str) -> String {
    readable_subagent_log_line_timed(message, None)
}

/// 同上，`elapsed` 是这次内层调用从发出到结果回来花的时间（只有结果事件带）。
fn readable_subagent_log_line_timed(message: &str, elapsed: Option<Duration>) -> String {
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

/// dev 子代理的系统提示词。
///
/// 第一段是用户自己的 dev 提示词(`dev-prompt.md`,与 dev 会话读同一份),
/// 改它对子代理同时生效。第二段是主体也在用的主机环境块——子代理没有
/// 每轮瞬态尾巴,工作目录只能从这里知道,否则第一步永远浪费在 `pwd` 上。
/// 末尾是那句交付约定。
///
/// 三段在一个会话里都是常量(工作目录跟着会话工作区走),多次 dev 子代理
/// 之间前缀缓存照样命中。
fn build_dev_system_prompt(config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let mut prompt = config.dev_system_prompt(paths)?;
    prompt.push_str("\n\n");
    prompt.push_str(&crate::agent::prompt::host_environment_for(config, paths));
    prompt.push_str(&format!(
        "\n<runtime cwd=\"{}\"/>",
        crate::host_info::xml_attr_escape(
            &crate::tools::workspace::effective_workdir()
                .display()
                .to_string()
        )
    ));
    prompt.push_str("\n\n");
    prompt.push_str(SUBAGENT_DEV_CONTRACT);
    Ok(prompt)
}

async fn run_core(
    context: SubagentContext,
    progress: crate::tools::ToolProgress,
    params: SubagentParams,
    anchor: AuditAnchor,
    inbox_id: Option<String>,
) -> Result<SubagentRun> {
    let SubagentParams {
        description,
        prompt,
        resume_id,
        max_steps,
        tier,
        dev,
    } = params;
    let tool_timeout = SUBAGENT_TOOL_TIMEOUT;

    // WebUI 回合(既非终端、也非平台:没有 origin tty、没有平台 sender)一律用
    // Full 档发子过程标记(思考 + 结构化工具调用/结果),网页端据此把展开后的
    // 子过程时间线画成「思考+工具流」——和主智能体过程区同款(09-11 用户要求)。
    // 网页端默认收起这些,静息态不吵;终端/平台仍按 display.tool_calls 配置,
    // 免得 Summary 档的终端用户突然被子代理的全量嵌套刷屏。
    let is_webui_turn = crate::tools::workspace::current_origin_tty().is_none()
        && crate::tools::workspace::current_platform_sender().is_none();
    let mode = if is_webui_turn {
        ProgressMode::Full
    } else {
        ProgressMode::from_config(&context.config)
    };
    let enabled = context.config.plugins.deep_research.show_progress;
    let sa_progress = SubagentProgress::new(progress, mode, enabled);

    // 子过程展开区最上方的任务简介(09-12 #9:后台子代理展开后没有 prompt)。
    // 只在 Full 档(WebUI)发;前台子代理前端从工具参数直接建 brief、并置 sink.brief,
    // 收到这条 marker 会跳过不重复,后台没有参数就靠这条把 prompt 显示出来。
    if mode == ProgressMode::Full {
        sa_progress.phase(format!(
            "__subagent_brief__{}",
            serde_json::json!({ "description": &description, "prompt": &prompt })
        ));
    }

    // dev 子代理 = 开发模式的三件套,与 dev 会话同源:保留人格 "dev" 的
    // 作用域(记忆整套关)、那份 core_only 的工具面、以及中转线的 dev 工具
    // 作用域。少任何一件都会漂移成「名字叫 dev、其实是普通子代理」。
    let config = if dev {
        context.config.dev_scoped()
    } else {
        context.config.clone()
    };

    // Tier routing: the tier's pool gets its own load-balanced client;
    // an unconfigured pool silently uses the main model pool, and a
    // configured-but-unusable pool falls back with a notice returned to
    // the calling agent (not printed to the user). The fallback contract
    // lives in `from_tier` so auxiliary roles share it byte for byte.
    let routed = OpenAiCompatibleClient::from_tier(&config, &context.paths, tier)?;
    let tier_notice = routed.notice;
    let model_choice = routed.model_choice;
    let client = routed
        .client
        .with_request_scope("subagent")
        .with_claude_code_dev_mode(dev)
        .for_subagent_output(mode == ProgressMode::Full);
    // 普通子代理沿用主体目录:任务是主体布置的,分类只会让"承诺的工具"
    // 与"实际注册的工具"漂移(dev 下的旧 explore 就是这么坏掉的)。
    // dev 子代理反过来:它的任务与主体人格无关,拿的就是 dev 会话那张面,
    // 现造而不是注册时造——注册发生在 `compose_registry` 里,在那儿造 dev
    // 面会自己套自己。
    let tools = if dev {
        crate::tools::build_tool_registry(&config, &context.paths, AgentMode::Dev, false)?
    } else {
        context.tools.clone()
    };

    let system_prompt = if dev {
        build_dev_system_prompt(&config, &context.paths)?
    } else {
        SUBAGENT_SYSTEM_PROMPT.to_string()
    };

    // 审计会话**开跑之前**就建好：它的用量行是会话累计里子代理那一份的来源，
    // 跑完才写的话，中途被打断这一趟烧的词元就彻底没了（用户问到的正是这个）。
    let audit = SubagentAudit::open(&context, &anchor, &description, &prompt);
    let mut runner = SubagentRunner::new(client, system_prompt, tools, sa_progress)
        .max_steps(max_steps)
        .timeout_seconds(tool_timeout)
        .excluded_tools(SUBAGENT_EXCLUDED)
        .inbox_id(inbox_id.clone());
    if let Some(audit) = &audit {
        runner = runner.usage_sink(audit.usage_sink());
    }

    // 后台子代理开收件箱:主体可在运行途中投递 follow-up(见 subagent_runner)。
    // 用 drop guard 关箱,覆盖所有退出路径(正常返回 / `?` 早退 / panic)。
    struct InboxGuard(Option<String>);
    impl Drop for InboxGuard {
        fn drop(&mut self) {
            if let Some(id) = &self.0 {
                crate::tools::subagent_runner::close_subagent_inbox(id);
            }
        }
    }
    if let Some(id) = &inbox_id {
        crate::tools::subagent_runner::open_subagent_inbox(id);
    }
    let _inbox_guard = InboxGuard(inbox_id.clone());

    // 子代理不设总时长上限:它自然结束于任务完成或步数预算;逐工具超时
    // (tool_timeout)仍然兜底单步挂死。
    // 标记「在子代理里」:vision_analyze 据此走旁路转写而非 inline 寄存
    // (子代理循环不接力 inline 媒体,见 workspace::in_subagent)。
    let (result, stats) = match crate::tools::workspace::with_subagent(
        runner.run_with_resume(&prompt, resume_id.as_deref()),
    )
    .await
    {
        Ok((result, stats)) => (result, stats),
        Err(err) => {
            let output = serde_json::to_string_pretty(&json!({
                "ok": false,
                "kind": "subagent",
                "tier": tier.label(),
                "tier_notice": tier_notice,
                "description": description,
                "state": "error",
                "error": err.to_string(),
                "stats": SubagentStats::default().public(),
            }))?;
            match &audit {
                Some(audit) => audit.finish(&context, &output, None, &model_choice),
                None => record_subagent_audit(
                    &context,
                    &anchor,
                    &description,
                    &prompt,
                    &output,
                    None,
                    &model_choice,
                ),
            }
            return Ok(SubagentRun {
                output,
                state: "error",
            });
        }
    };

    let state = if stats.budget_reached {
        "budget_reached"
    } else {
        "completed"
    };

    let final_text = result.content.trim().to_string();

    // 08-21 token-diet:成功路径改文本形态——子代理结论不再被 JSON 转义
    // (换行/引号转义在长结论上是实打实的浪费)。result: 之后到结尾都是
    // 结论本体,tool_report.rs 的持久化提取按此约定解析;错误路径保留
    // ok:false JSON(成败判定的结构即功能)。
    let mut output = format!("subagent {state} (tier {}): {description}\n", tier.label());
    if let Some(notice) = &tier_notice {
        output.push_str(notice);
        output.push('\n');
    }
    output.push_str(&format!(
        "stats: {}\n",
        serde_json::to_string(&stats.public())?
    ));
    output.push_str("result:\n");
    output.push_str(&final_text);
    // Prefer the endpoint that actually produced the final reply (pools
    // load-balance, so the representative pool entry may differ).
    let model_choice = match (&result.provider_id, &result.model) {
        (Some(provider_id), Some(model)) => Some((provider_id.clone(), model.clone())),
        _ => model_choice,
    };
    match &audit {
        Some(audit) => audit.finish(&context, &output, Some(&stats), &model_choice),
        None => record_subagent_audit(
            &context,
            &anchor,
            &description,
            &prompt,
            &output,
            Some(&stats),
            &model_choice,
        ),
    }
    Ok(SubagentRun { output, state })
}

/// Persists an audit session for a subagent run: a hidden `kind='subagent'`
/// session linked to the parent turn's session, holding one turn (prompt →
/// result JSON) plus the model identity and token usage on the session row.
/// Best-effort: audit failures never fail the task itself.
/// 一趟子代理的审计会话：开跑之前就建好，边跑边记账，跑完写结果。
///
/// 原来是**跑完才写**的一锤子买卖——中途被打断（Ctrl+C、超时、daemon 重启）
/// 这一趟烧掉的词元就彻底没了，会话累计里查无此事（用户问：万一中断了不就
/// 丢失数据了吗）。现在开跑就有一行，量报每来一次就更新它。
struct SubagentAudit {
    store: crate::state::StateStore,
    session_id: String,
    turn_id: String,
    context_window: Option<i64>,
}

impl SubagentAudit {
    fn open(
        context: &SubagentContext,
        anchor: &AuditAnchor,
        description: &str,
        prompt: &str,
    ) -> Option<Self> {
        let outcome = (|| -> Result<Self> {
            let store = crate::state::StateStore::new(&context.paths)?;
            let name: String = description.chars().take(40).collect();
            let record = store.create_session(
                &anchor.persona,
                &name,
                "subagent",
                anchor.parent.as_deref(),
            )?;
            let turn_id = format!(
                "sat_{}_{:08x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis())
                    .unwrap_or(0),
                rand::random::<u32>()
            );
            store
                .pinned(&record.session_id)
                .start_turn(&turn_id, prompt, std::process::id())?;
            Ok(Self {
                store,
                session_id: record.session_id,
                turn_id,
                context_window: None,
            })
        })();
        match outcome {
            Ok(audit) => Some(audit),
            Err(error) => {
                tracing::warn!(error = %error, "{}", crate::i18n::text("failed to open the subagent audit session", "建立子代理审计会话失败"));
                None
            }
        }
    }

    /// 把此刻的账记上。写的是**累计值**不是增量，重复写不会算两遍。
    fn record(&self, stats: &SubagentStats) {
        let _ = self.store.record_subagent_usage(
            &self.session_id,
            None,
            None,
            self.context_window,
            stats.prompt_tokens as i64,
            stats.completion_tokens as i64,
            stats.total_tokens.max(stats.token_estimate) as i64,
            stats.cache_read_tokens as i64,
        );
    }

    fn usage_sink(&self) -> std::sync::Arc<dyn Fn(&SubagentStats) + Send + Sync> {
        let store = self.store.clone();
        let session_id = self.session_id.clone();
        let context_window = self.context_window;
        std::sync::Arc::new(move |stats: &SubagentStats| {
            let _ = store.record_subagent_usage(
                &session_id,
                None,
                None,
                context_window,
                stats.prompt_tokens as i64,
                stats.completion_tokens as i64,
                stats.total_tokens.max(stats.token_estimate) as i64,
                stats.cache_read_tokens as i64,
            );
        })
    }

    /// 收尾：写结果、补上端点与最终用量。
    fn finish(
        &self,
        context: &SubagentContext,
        output: &str,
        stats: Option<&SubagentStats>,
        model_choice: &Option<(String, String)>,
    ) {
        let outcome = (|| -> Result<()> {
            self.store
                .pinned(&self.session_id)
                .complete_turn(&self.turn_id, output, None)?;
            let (provider_id, model) = match model_choice.as_ref() {
                Some((provider_id, model)) => (Some(provider_id.as_str()), Some(model.as_str())),
                None => (None, None),
            };
            let context_window = match (provider_id, model) {
                (Some(provider), Some(model)) => context
                    .config
                    .context_window_for_provider_model(provider, model)
                    .ok()
                    .flatten()
                    .map(|window| window as i64),
                _ => None,
            };
            let (prompt_tokens, completion_tokens, total_tokens, cache_read_tokens) = match stats {
                Some(stats) => (
                    stats.prompt_tokens as i64,
                    stats.completion_tokens as i64,
                    stats.total_tokens.max(stats.token_estimate) as i64,
                    stats.cache_read_tokens as i64,
                ),
                None => (0, 0, 0, 0),
            };
            self.store.record_subagent_usage(
                &self.session_id,
                provider_id,
                model,
                context_window,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cache_read_tokens,
            )
        })();
        if let Err(error) = outcome {
            tracing::warn!(error = %error, "{}", crate::i18n::text("failed to record subagent audit session", "记录子代理审计会话失败"));
        }
    }
}

fn record_subagent_audit(
    context: &SubagentContext,
    anchor: &AuditAnchor,
    description: &str,
    prompt: &str,
    output: &str,
    stats: Option<&SubagentStats>,
    model_choice: &Option<(String, String)>,
) {
    let outcome = (|| -> Result<()> {
        let store = crate::state::StateStore::new(&context.paths)?;
        let parent = anchor.parent.clone();
        let persona = anchor.persona.clone();
        let name: String = description.chars().take(40).collect();
        let record = store.create_session(&persona, &name, "subagent", parent.as_deref())?;
        let pinned = store.pinned(&record.session_id);
        let turn_id = format!(
            "sat_{}_{:08x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0),
            rand::random::<u32>()
        );
        pinned.start_turn(&turn_id, prompt, std::process::id())?;
        pinned.complete_turn(&turn_id, output, None)?;
        let (provider_id, model) = match model_choice.as_ref() {
            Some((provider_id, model)) => (Some(provider_id.as_str()), Some(model.as_str())),
            None => (None, None),
        };
        let context_window = match (provider_id, model) {
            (Some(provider), Some(model)) => context
                .config
                .context_window_for_provider_model(provider, model)
                .ok()
                .flatten()
                .map(|window| window as i64),
            _ => None,
        };
        let (prompt_tokens, completion_tokens, total_tokens, cache_read_tokens) = match stats {
            Some(stats) => (
                stats.prompt_tokens as i64,
                stats.completion_tokens as i64,
                stats.total_tokens.max(stats.token_estimate) as i64,
                stats.cache_read_tokens as i64,
            ),
            None => (0, 0, 0, 0),
        };
        store.record_subagent_usage(
            &record.session_id,
            provider_id,
            model,
            context_window,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cache_read_tokens,
        )
    })();
    if let Err(error) = outcome {
        tracing::warn!(error = %error, "{}", crate::i18n::text("failed to record subagent audit session", "记录子代理审计会话失败"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 中途的量报只刷标题和状态行，**不进流水账**。
    ///
    /// 它一秒能来好几次（每调完一个工具报一次）。落进去的话，面板里那条时间线
    /// 会被「统计」节点撑满，真正在干什么反而看不见了。
    #[test]
    fn running_metric_never_lands_in_the_job_log() {
        assert_eq!(
            readable_subagent_log_line("__subagent_metric__1.2K\t工具调用 3 次"),
            ""
        );
        // 跑完那一次照旧留底。
        assert_eq!(
            readable_subagent_log_line("__subagent_stats__工具调用 3 次"),
            "[统计] 工具调用 3 次"
        );
    }

    /// 结果那一行要把工具真吐出来的东西带上。
    ///
    /// 只写一句「运行命令 ok · ls」的话，面板里那一步点开看到的还是同一句话
    /// ——等于点开是空的（用户实测：浮层里这些工具展开都没内容）。
    #[test]
    fn tool_result_carries_its_output_into_the_log() {
        let json = serde_json::json!({
            "name": "run_command",
            "args": r#"{"command":"ls"}"#,
            "ok": true,
            "output": "total 12\n\ndrwxr-xr-x 2 shorin\n",
        })
        .to_string();
        let line = readable_subagent_log_line(&format!("__subtool_result__{json}"));
        let mut lines = line.lines();
        assert!(
            lines
                .next()
                .unwrap_or_default()
                .starts_with("[结果] run_command\t"),
            "{line}"
        );
        assert_eq!(lines.next(), Some("[输出] total 12"), "{line}");
        // 空行不占一条记录。
        assert_eq!(lines.next(), Some("[输出] drwxr-xr-x 2 shorin"), "{line}");
        assert_eq!(lines.next(), None, "{line}");
    }

    /// 正文也是**逐 delta** 来的，得攒成段落再落盘。
    ///
    /// 一条一行的话日志会变成每行一个词的字符梯（用户实测截图：整屏
    /// `[正文] the` / `[正文] and`）。
    #[test]
    fn streamed_speech_is_batched_into_paragraphs() {
        let mut buffer = String::new();
        let mut lines = Vec::new();
        for chunk in ["Now ", "let ", "me ", "enumerate."] {
            accumulate_stream(&mut buffer, chunk, "[正文]", &mut lines);
        }
        assert!(lines.is_empty(), "还没到段落就落盘了: {lines:?}");
        flush_stream_buffer(&mut buffer, "[正文]", &mut lines);
        assert_eq!(lines, vec!["[正文] Now let me enumerate.".to_string()]);
        // 空行就是段落分隔，到了就落一条。
        let mut lines = Vec::new();
        accumulate_stream(&mut buffer, "第一段\n\n第二段", "[正文]", &mut lines);
        assert_eq!(lines, vec!["[正文] 第一段".to_string()]);
        flush_stream_buffer(&mut buffer, "[正文]", &mut lines);
        assert_eq!(lines[1], "[正文] 第二段");
    }

    /// 工具吐的原始输出要洗干净再进流水账。
    ///
    /// 转义序列、回车、制表符原样写进去的话，面板按纯文本算宽度，算出来的和
    /// 真实占宽对不上，右边那根竖线跟着参差不齐。
    #[test]
    fn tool_output_is_plain_text_in_the_log() {
        let json = serde_json::json!({
            "name": "run_command",
            "args": "{}",
            "ok": true,
            "output": "\u{1b}[31m红的\u{1b}[0m\ta\u{7}b\r\n干净一行\n",
        })
        .to_string();
        let line = readable_subagent_log_line(&format!("__subtool_result__{json}"));
        let outputs = line
            .lines()
            .filter_map(|line| line.strip_prefix("[输出] "))
            .collect::<Vec<_>>();
        assert_eq!(outputs, vec!["红的 ab", "干净一行"], "{line}");
        // `[结果]` 那一行自己带一个制表符（工具 id 的分隔），只看输出那几行。
        assert!(
            outputs
                .iter()
                .all(|line| !line.contains(|ch: char| ch.is_control())),
            "{line}"
        );
    }

    /// 差事写在流水账开头，换行折成 `\u{1}`（面板那边再拆回来）。
    #[test]
    fn prompt_header_folds_newlines() {
        let dir = std::env::temp_dir().join(format!("miyu-prompt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        write_subagent_prompt_header(&path, "  第一行\n第二行  ");
        let text = std::fs::read_to_string(&path).expect("读日志");
        assert_eq!(text, "[提示] 第一行\u{1}第二行\n");
        // 空差事不写。
        let empty = dir.join("empty.log");
        write_subagent_prompt_header(&empty, "   ");
        assert!(!empty.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn test_paths(root: &std::path::Path) -> MiyuPaths {
        crate::tools::tests::test_paths(root)
    }

    #[test]
    fn dev_flag_defaults_to_off_and_parses() {
        let base = json!({"description": "d", "prompt": "p"});
        assert!(!parse_params(&base).unwrap().dev);
        let mut with_dev = base.clone();
        with_dev["dev"] = json!(true);
        assert!(parse_params(&with_dev).unwrap().dev);
    }

    /// dev 子代理的系统提示词是三段拼起来的,少任何一段它都得先浪费一轮
    /// 去问「我在哪、说给谁听」。
    #[test]
    fn dev_system_prompt_carries_the_dev_prompt_host_block_and_contract() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let prompt = build_dev_system_prompt(&config, &paths).unwrap();
        assert!(
            prompt.starts_with(crate::config::DEFAULT_DEV_SYSTEM_PROMPT),
            "{prompt}"
        );
        assert!(prompt.contains("<host-environment"), "{prompt}");
        assert!(prompt.contains("<runtime cwd="), "{prompt}");
        assert!(prompt.ends_with(SUBAGENT_DEV_CONTRACT), "{prompt}");
    }

    /// 同一个会话里连开两个 dev 子代理,系统提示词必须逐字节相同——不然
    /// 每一个都是一次冷前缀。
    #[test]
    fn dev_system_prompt_is_byte_stable_within_a_session() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        assert_eq!(
            build_dev_system_prompt(&config, &paths).unwrap(),
            build_dev_system_prompt(&config, &paths).unwrap()
        );
    }

    /// 递归防护:dev 子代理拿的是 dev 会话那张面,而那张面里也注册着
    /// `subagent`——排除表必须认得新名,否则子代理能自己再开子代理。
    #[test]
    fn subagent_excludes_itself_by_its_current_name() {
        assert!(SUBAGENT_EXCLUDED.contains(&"subagent"));
    }

    /// 后台子代理起在 `tokio::spawn` 的新任务上,回合的 task-local 到那儿
    /// 全空了:成员的后台子代理会因此跑在 Landlock 之外,工具的工作目录
    /// 也退回 daemon 的 cwd。这条钉住「抓下来再套回去」。
    #[tokio::test]
    async fn background_scope_is_carried_across_the_spawn() {
        let workspace = std::path::PathBuf::from("/tmp/miyu-subagent-scope");
        let session: std::sync::Arc<str> = "sess_probe".into();
        let (bare, restored) = crate::tools::workspace::with_workspace(
            workspace.clone(),
            crate::tools::workspace::with_session(session.clone(), async {
                let carried_workspace = crate::tools::workspace::try_workspace();
                let carried_session = crate::tools::workspace::try_session();
                tokio::spawn(async move {
                    let bare = (
                        crate::tools::workspace::try_workspace(),
                        crate::tools::workspace::try_session(),
                    );
                    let restored =
                        with_turn_scope(None, carried_workspace, carried_session, async {
                            (
                                crate::tools::workspace::try_workspace(),
                                crate::tools::workspace::try_session(),
                            )
                        })
                        .await;
                    (bare, restored)
                })
                .await
                .unwrap()
            }),
        )
        .await;
        assert_eq!(bare, (None, None), "裸 spawn 本就看不见回合作用域");
        assert_eq!(restored, (Some(workspace), Some(session)));
    }
}
