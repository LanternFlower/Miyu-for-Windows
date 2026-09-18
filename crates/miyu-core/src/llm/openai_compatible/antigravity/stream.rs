//! agy 子进程的生命周期与 stream-json 事件泵。
//!
//! stdout 每行一个事件:`init`(首行,带会话 id 与代理名)/ `step_update`
//! (正文增量、工具步、错误步)/ `result`(终态)。与 claude 线的差异:
//! ①正文分片在 `agent_response` 步的 `text_delta` 里,同轮多次模型调用是多个
//! 步,步间补空行;②工具步 ACTIVE/DONE/ERROR 三态,DONE 的 `tool_info.output`
//! 只有命令与 MCP 结果有正文;③result 的 usage 是整轮累计,单次调用的用量在
//! 最后一个 `agent_response DONE` 里;④两种静默失败——续传目标丢失时静默
//! 新开会话、代理没挂上时静默退回默认提示词——都只能靠 init 首行判定,判定
//! 失败立刻杀进程(init 先于模型调用,不花额度)。
//!
//! 超时/出错路径必须显式杀进程组:drop future 只是弃 promise,不杀子进程。

use super::{ResumeTargetLost, MCP_SERVER_NAME};
use crate::llm::openai_compatible::cli_relay::{
    compact_line, hidden_remote_tool, process::RelayProcess, shape_remote_output, RelayOutcome,
    SessionPoisoned,
};
use crate::llm::openai_compatible::*;

/// 把登录态/额度类失败翻译成端点调度认识的分类。只看 result 级的错误文本:
/// stderr 每次启动都打一行 "not logged into Antigravity" 再静默鉴权成功,
/// 拿它判 401 会误杀。
fn classify_agy_failure(text: &str) -> Option<HttpStatusFailure> {
    let lower = text.to_ascii_lowercase();
    const RATE_LIMIT: &[&str] = &[
        "quota",
        "rate limit",
        "too many requests",
        "resource_exhausted",
        "resource exhausted",
        "usage limit",
        "out of credits",
        "credits exhausted",
    ];
    const AUTH: &[&str] = &[
        "not logged in",
        "not logged into",
        "sign in",
        "unauthenticated",
        "authentication",
        "login required",
    ];
    if google_policy_block(text) {
        return Some(HttpStatusFailure {
            status: 400,
            kind: HttpFailureKind::ContentPolicy,
        });
    }
    if RATE_LIMIT.iter().any(|needle| lower.contains(needle)) {
        return Some(HttpStatusFailure {
            status: 429,
            kind: HttpFailureKind::RateLimit,
        });
    }
    if AUTH.iter().any(|needle| lower.contains(needle)) {
        return Some(HttpStatusFailure {
            status: 401,
            kind: HttpFailureKind::Authentication,
        });
    }
    None
}

/// Google 侧的提示词安全拦截。agy 不把它当错误,而是当成一段普通回复正文
/// (agent_response 的 text_delta,result 也是 SUCCESS)流出来,不拦就会原样
/// 发到 QQ(09-04/09-06 用户实录各数条)。只认这两句的固定措辞。
pub(super) fn google_policy_block(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("the prompt could not be submitted") || lower.contains("prohibited use policy")
}

#[derive(Default)]
struct StreamState {
    /// 正文一开头就是 Google 的策略拦截文案:整段不当正文发,收尾判错。
    policy_block: Option<String>,
    content: String,
    content_emitted: usize,
    /// 当前正文步的 step_index:换步时补空行。
    response_step: Option<u64>,
    /// 最后一次模型调用的用量(上下文表读它)。
    per_call_usage: Option<Usage>,
    /// 本轮所有模型调用的用量累计(记账读它)。agy 的工具循环在一个进程里
    /// 可能调好几次模型,每次都真的计费,所以本轮花费是逐次相加——而
    /// `result.usage` 是**整条会话**的累计,拿它记账会把历史重复计进本轮
    /// (09-16 实测:本轮 16,399,result 报 2,800,349)。
    turn_usage: Option<Usage>,
    /// 已发过 started 的工具步。
    started_tools: HashSet<u64>,
    /// `error_message` 步的正文,静默失败时并进报错。
    error_text: String,
}

/// 跑一轮:`process` 已经拉起并写好本轮载荷(新进程)或是借来的常驻进程(已写入
/// 下一段,`reused`)。`keep_alive` 为真且本轮正常收到终态帧,进程不收尾、原样交回
/// 调用方入池;其余情况(不复用、出错、进程自己退了)收尾/收掉,交回 `None`。
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_agy_turn<F>(
    mut process: RelayProcess,
    expected_agent: &str,
    expected_conversation: Option<&str>,
    reused: bool,
    keep_alive: bool,
    request_id: &str,
    on_chunk: &mut F,
) -> Result<(RelayOutcome, Option<RelayProcess>)>
where
    F: FnMut(ChatStreamChunk) -> Result<()>,
{
    let mut state = StreamState::default();
    // 借来的进程不会再发 init:会话 id 就是借的时候那条。
    let mut conversation_id: Option<String> = if reused {
        expected_conversation.map(str::to_string)
    } else {
        None
    };
    let mut final_frame: Option<Value> = None;
    while let Some(line) = process.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(request_id, %error, "agy emitted a non-JSON stdout line");
                continue;
            }
        };
        match value.get("event").and_then(Value::as_str) {
            Some("init") => {
                let actual = value
                    .get("conversation_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let agent = value.pointer("/init/agent").and_then(Value::as_str);
                if agent != Some(expected_agent) {
                    // 代理没挂上 = 静默跑在 agy 自己的默认提示词上,人格全丢。
                    process.kill();
                    bail!(
                        "agy did not load the `{expected_agent}` persona agent (init.agent = {agent:?}); {}",
                        process.stderr_tail()
                    );
                }
                if let Some(expected) = expected_conversation {
                    if actual != expected {
                        process.kill();
                        return Err(anyhow::Error::new(ResumeTargetLost {
                            requested: expected.to_string(),
                            actual,
                        }));
                    }
                }
                conversation_id = Some(actual);
            }
            Some("step_update") => {
                if let Some(step) = value.get("step_update") {
                    handle_step(step, &mut state, on_chunk)?;
                }
            }
            Some("result") => {
                final_frame = Some(value);
                break;
            }
            _ => {}
        }
    }
    let (exit_code, stderr_text, live) =
        if keep_alive && final_frame.is_some() && process.is_alive() {
            ("running".to_string(), process.stderr_tail(), Some(process))
        } else {
            let (code, tail) = process.finish().await;
            (code, tail, None)
        };
    match conclude(
        state,
        final_frame,
        conversation_id,
        &exit_code,
        &stderr_text,
        request_id,
        on_chunk,
    ) {
        Ok(outcome) => Ok((outcome, live)),
        Err(error) => {
            // 出错的进程不进池:SIGTERM 收掉(理由见 `RelayProcess::terminate`)。
            if let Some(process) = live {
                process.retire();
            }
            Err(error)
        }
    }
}

/// 终态帧到手之后的判定:策略拦截、会话级粘性 ERROR、零产出、正文与用量。
fn conclude<F>(
    state: StreamState,
    final_frame: Option<Value>,
    mut conversation_id: Option<String>,
    exit_code: &str,
    stderr_text: &str,
    request_id: &str,
    on_chunk: &mut F,
) -> Result<RelayOutcome>
where
    F: FnMut(ChatStreamChunk) -> Result<()>,
{
    let mut state = state;
    let Some(final_frame) = final_frame else {
        bail!(
            "agy exited (code {exit_code}) without a result event: {} {}",
            state.error_text.trim(),
            stderr_text
        );
    };
    let result_frame = final_frame.get("result").cloned().unwrap_or(Value::Null);
    if conversation_id.is_none() {
        conversation_id = result_frame
            .get("conversation_id")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    let status = result_frame
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let response = result_frame
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let error_field = match result_frame.get("error") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    // 策略拦截文案可能出现在正文步、result.response 或 error 字段里;不论
    // status 怎么写,一律按 ContentPolicy 判错,绝不当回复发出去。
    let policy_text = state
        .policy_block
        .clone()
        .or_else(|| google_policy_block(&response).then(|| response.clone()))
        .or_else(|| google_policy_block(&error_field).then(|| error_field.clone()));
    if let Some(text) = policy_text {
        tracing::warn!(
            request_id,
            "{}",
            t(
                "agy: Google content policy blocked the prompt; reply suppressed",
                "agy:Google 内容策略拦下了这条提示词,已拦截回复"
            )
        );
        return Err(anyhow::anyhow!(
            "{}: {}",
            t(
                "Google content policy blocked this prompt",
                "Google 内容策略拦下了这条提示词"
            ),
            compact_line(&text, 200)
        )
        .context(HttpStatusFailure {
            status: 400,
            kind: HttpFailureKind::ContentPolicy,
        }));
    }
    // `result.status` 是**会话级**粘性状态,不是本轮的成败:会话转录里但凡留下
    // 过一次失败(被硬杀的取消态步、上下文压缩序列化炸掉),之后每次续传这条
    // 会话,agy 都原样回吐当时那份 status/error,哪怕本轮从头到尾跑得好好的。
    // 09-16 实测钉死:同一条坏会话发 `ping` 照样回 `pong`、本轮用量 16,399,
    // status 仍是 ERROR、error 还是两小时前那句;对照的好会话同样一句 `ping`
    // 回 SUCCESS。按 status 一刀切会把已经生成好的正文整个丢掉——群聊里
    // 非管理员那一档连挂两小时正是这么来的。
    // 所以:本轮真跑出东西了就按成功交付,只把会话标记成不可再续传;本轮确实
    // 什么都没产出才是真失败。
    let turn_produced_output = state.turn_usage.is_some()
        && !(state.content.trim().is_empty() && response.trim().is_empty());
    let session_poisoned = status != "SUCCESS";
    if session_poisoned && !turn_produced_output {
        let detail = [
            error_field.as_str(),
            state.error_text.trim(),
            response.trim(),
        ]
        .into_iter()
        .find(|text| !text.is_empty())
        .unwrap_or(stderr_text)
        .to_string();
        let classified = classify_agy_failure(&format!("{error_field}\n{detail}"));
        let mut error = anyhow::anyhow!("agy turn failed ({status}): {}", detail.trim());
        if let Some(failure) = classified {
            error = error.context(failure);
        } else {
            // 归不到上游原因(额度/登录/策略)的失败,多半就是这条会话本身废了
            // ——上层作废它、重开一条重试(同 `ResumeTargetLost` 的待遇)。不这么
            // 做,这一档就一直续传同一条坏会话:09-16 实测一条坏会话粘了 1 小时
            // 45 分、连吃 37 次调用才自己缓过来(不是永久,但足够毁掉一下午)。
            // 反过来:能归类的**不能**重开重试。09-15 实测 agy 把配额耗尽
            // (`RESOURCE_EXHAUSTED`)也焊进会话状态,而换条新会话一样没额度,
            // 重试只是白烧一次调用;交给冷却/故障转移处理才对。
            error = error.context(SessionPoisoned);
        }
        return Err(error);
    }
    if session_poisoned {
        tracing::warn!(
            request_id,
            status = %status,
            detail = %compact_line(&error_field, 120),
            "agy replayed a stale session-level error although this turn produced output; \
             delivering the turn and retiring the conversation"
        );
    }

    flush_buffer(
        &state.content,
        &mut state.content_emitted,
        ChatStreamKind::Content,
        &mut *on_chunk,
        true,
    )?;
    let mut content = state.content;
    if content.trim().is_empty() && !response.trim().is_empty() {
        on_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: response.clone(),
        })?;
        content = response;
    }
    // `result.usage` 是整条会话的累计,只在本轮一次模型调用都没记到用量时
    // 当兜底(比如 agy 没给步级 usage),否则记账一律用本轮累计。
    let session_usage = result_frame.get("usage").and_then(usage_from_agy);
    let turn_usage = state.turn_usage.clone();
    let per_call_usage = state.per_call_usage.clone();
    if content.trim().is_empty()
        && per_call_usage.is_none()
        && session_usage
            .as_ref()
            .map(|usage| usage.effective_total_tokens() == 0)
            .unwrap_or(true)
    {
        // 「SUCCESS 但什么都没发生」:代理配置坏了、权限被拒、或模型一个字
        // 没吐。agy 对这几种都只在 stderr/error_message 步里留痕。
        bail!(
            "agy produced no output (status SUCCESS, zero usage): {} {}",
            state.error_text.trim(),
            stderr_text
        );
    }
    let usage = turn_usage.or(session_usage);
    let mut result = finalize_stream_result(content, String::new(), usage, Vec::new(), false)?;
    result.finish_reason = Some("stop".to_string());
    result.last_request_usage = per_call_usage;
    Ok(RelayOutcome {
        result,
        session_id: conversation_id,
        session_poisoned,
    })
}

fn handle_step<F>(step: &Value, state: &mut StreamState, on_chunk: &mut F) -> Result<()>
where
    F: FnMut(ChatStreamChunk) -> Result<()>,
{
    let index = step.get("step_index").and_then(Value::as_u64).unwrap_or(0);
    let step_state = step.get("state").and_then(Value::as_str).unwrap_or("");
    match step.get("step_type").and_then(Value::as_str) {
        Some("agent_response") => {
            if state.response_step != Some(index) {
                if state.response_step.is_some()
                    && !state.content.is_empty()
                    && !state.content.ends_with("\n\n")
                {
                    push_buffered_chunk(
                        &mut state.content,
                        &mut state.content_emitted,
                        ChatStreamKind::Content,
                        "\n\n".to_string(),
                        on_chunk,
                    )?;
                }
                state.response_step = Some(index);
            }
            if let Some(text) = step.get("text_delta").and_then(Value::as_str) {
                if let Some(blocked) = state.policy_block.as_mut() {
                    // 已判定为拦截文案:后续增量只攒着进报错,不发正文。
                    blocked.push_str(text);
                } else if state.content.is_empty() && google_policy_block(text) {
                    state.policy_block = Some(text.to_string());
                } else {
                    push_buffered_chunk(
                        &mut state.content,
                        &mut state.content_emitted,
                        ChatStreamKind::Content,
                        text.to_string(),
                        on_chunk,
                    )?;
                }
            }
            if step_state == "DONE" {
                if let Some(usage) = step.get("usage").and_then(usage_from_agy) {
                    state.turn_usage = Some(match state.turn_usage.take() {
                        Some(running) => add_usage(running, &usage),
                        None => usage.clone(),
                    });
                    state.per_call_usage = Some(usage);
                }
            }
        }
        Some("tool") => {
            let info = step.get("tool_info").cloned().unwrap_or(Value::Null);
            let raw_name = step
                .get("tool_name")
                .and_then(Value::as_str)
                .or_else(|| info.get("name").and_then(Value::as_str))
                .unwrap_or("tool");
            let (name, input) = translate_tool(raw_name, info.get("parameters").cloned());
            if hidden_remote_tool(&name) {
                return Ok(());
            }
            let id = format!("agy-{index}");
            if state.started_tools.insert(index) {
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::RemoteToolStarted,
                    text: json!({ "id": id, "name": name, "input": input }).to_string(),
                })?;
            }
            if step_state == "DONE" || step_state == "ERROR" {
                let error = info
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let ok = step_state == "DONE" && error.is_none();
                let raw_output = match info.get("output") {
                    Some(Value::String(text)) => text.clone(),
                    Some(Value::Null) | None => String::new(),
                    Some(other) => other.to_string(),
                };
                let output = shape_remote_output(&name, &error.unwrap_or(raw_output));
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::RemoteToolFinished,
                    text: json!({ "id": id, "name": name, "ok": ok, "output": output }).to_string(),
                })?;
            }
        }
        Some("error_message") => {
            if let Some(text) = step.get("text_delta").and_then(Value::as_str) {
                state.error_text.push_str(text);
                state.error_text.push('\n');
            }
        }
        _ => {}
    }
    Ok(())
}

/// agy 的工具名/入参 → Miyu 卡片认识的名字与键。
/// - eager 注册的桥工具叫 `mcp_miyu_<name>`,剥前缀就是 Miyu 本名;
/// - 懒加载时是 `call_mcp_tool{ServerName,ToolName,Arguments}`,拆出来;
/// - 原生工具的入参键是 CamelCase(CommandLine/AbsolutePath…),归一成 Miyu 的
///   `command`/`path`/…,终端 `↳` 主题、WebUI 命令卡、平台日志三端都不用改。
fn translate_tool(raw_name: &str, parameters: Option<Value>) -> (String, Value) {
    let bridge_prefix = format!("mcp_{MCP_SERVER_NAME}_");
    if let Some(inner) = raw_name.strip_prefix(&bridge_prefix) {
        return (inner.to_string(), parameters.unwrap_or(json!({})));
    }
    if raw_name == "call_mcp_tool" {
        if let Some(params) = parameters.as_ref() {
            if params.get("ServerName").and_then(Value::as_str) == Some(MCP_SERVER_NAME) {
                let name = params
                    .get("ToolName")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                let input = params.get("Arguments").cloned().unwrap_or(json!({}));
                return (name, input);
            }
        }
    }
    (raw_name.to_string(), normalize_native_arguments(parameters))
}

fn normalize_native_arguments(parameters: Option<Value>) -> Value {
    let Some(Value::Object(map)) = parameters else {
        return parameters.unwrap_or(json!({}));
    };
    let mut out = Map::new();
    for (key, value) in map {
        let mapped = match key.as_str() {
            "CommandLine" => "command",
            "Cwd" => "cwd",
            "AbsolutePath" | "TargetFile" | "DirectoryPath" | "SearchDirectory" | "SearchPath" => {
                "path"
            }
            "Pattern" => "pattern",
            "Query" => "query",
            "Url" => "url",
            // agy 给 UI 用的摘要字段,不是工具入参。
            "toolAction" | "toolSummary" => continue,
            other => other,
        };
        out.insert(mapped.to_string(), value);
    }
    Value::Object(out)
}

/// agy 的 usage 对象 → Miyu 口径。`input_tokens` 已含缓存命中部分(cache_read
/// 是它的子集)。**agy 的 `cache_read_tokens` 不可全信**:09-03 真机六轮里五轮
/// 它等于整个 input(9355/9355、45486/45486…),而本轮新输入的用户消息不可能
/// 已在缓存里——整段命中在物理上不成立。按 usage.rs 的规矩(没有真实依据的
/// 比率不能渲染成确定数字),声称整段命中的按「未报告缓存」处理;部分命中
/// (24324/33736 这种)照实上报。
/// 本轮内逐次模型调用的用量相加。缓存口径跟着加总:本轮但凡有一次真报了
/// 缓存命中,本轮就算报了(每次调用的命中量已在 `usage_from_agy` 里各自
/// 过滤过「整段命中」那种不可信的数)。
fn add_usage(mut running: Usage, next: &Usage) -> Usage {
    running.prompt_tokens = running.prompt_tokens.saturating_add(next.prompt_tokens);
    running.completion_tokens = running
        .completion_tokens
        .saturating_add(next.completion_tokens);
    running.total_tokens = running.total_tokens.saturating_add(next.total_tokens);
    running.reasoning_tokens = running
        .reasoning_tokens
        .saturating_add(next.reasoning_tokens);
    running.cache_read_tokens = running
        .cache_read_tokens
        .saturating_add(next.cache_read_tokens);
    running.cache_reported = running.cache_reported || next.cache_reported;
    running
}

fn usage_from_agy(value: &Value) -> Option<Usage> {
    let input = value.get("input_tokens").and_then(Value::as_u64)?;
    let output = value
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let thinking = value
        .get("thinking_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read = value
        .get("cache_read_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = value
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(input.saturating_add(output));
    let cache_reported = cache_read > 0 && cache_read < input;
    Some(Usage {
        prompt_tokens: input,
        completion_tokens: output,
        total_tokens: total,
        cache_read_tokens: if cache_reported { cache_read } else { 0 },
        reasoning_tokens: thinking,
        cache_reported,
        ..Usage::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_arguments_are_normalized_to_miyu_keys() {
        let (name, input) = translate_tool(
            "run_command",
            Some(json!({ "CommandLine": "pwd", "Cwd": "/w", "toolAction": "x" })),
        );
        assert_eq!(name, "run_command");
        assert_eq!(input, json!({ "command": "pwd", "cwd": "/w" }));
        let (name, input) =
            translate_tool("view_file", Some(json!({ "AbsolutePath": "/tmp/a.txt" })));
        assert_eq!(name, "view_file");
        assert_eq!(input["path"], "/tmp/a.txt");
    }

    #[test]
    fn bridge_tools_are_unwrapped_in_both_shapes() {
        let (name, input) = translate_tool("mcp_miyu_use_meme", Some(json!({ "action": "show" })));
        assert_eq!(name, "use_meme");
        assert_eq!(input["action"], "show");
        let (name, input) = translate_tool(
            "call_mcp_tool",
            Some(
                json!({ "ServerName": "miyu", "ToolName": "alarm", "Arguments": { "at": "9:00" } }),
            ),
        );
        assert_eq!(name, "alarm");
        assert_eq!(input["at"], "9:00");
        // 别家 MCP 服务器不剥、不拆。
        let (name, _) = translate_tool("mcp_other_thing", None);
        assert_eq!(name, "mcp_other_thing");
    }

    #[test]
    fn failure_classification_reads_quota_and_auth_phrases() {
        assert_eq!(
            classify_agy_failure("Quota exceeded for model").map(|f| f.status),
            Some(429)
        );
        assert_eq!(
            classify_agy_failure("You are not logged into Antigravity").map(|f| f.status),
            Some(401)
        );
        assert!(classify_agy_failure("model output error").is_none());
    }

    #[test]
    fn usage_maps_agy_fields() {
        let usage = usage_from_agy(&json!({
            "input_tokens": 100, "output_tokens": 7, "thinking_tokens": 3,
            "cache_read_tokens": 40, "total_tokens": 110
        }))
        .unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(usage.reasoning_tokens, 3);
        assert_eq!(usage.cache_read_tokens, 40);
        assert_eq!(usage.total_tokens, 110);
        assert!(usage.cache_reported);
        // 声称整段命中 = 不可信,按未报告处理;零命中同样不算"报告了缓存"。
        let bogus =
            usage_from_agy(&json!({ "input_tokens": 100, "cache_read_tokens": 100 })).unwrap();
        assert_eq!(bogus.cache_read_tokens, 0);
        assert!(!bogus.cache_reported);
        let none = usage_from_agy(&json!({ "input_tokens": 100, "cache_read_tokens": 0 })).unwrap();
        assert!(!none.cache_reported);
    }
}
