//! 一轮里模型要的那批工具的执行:复读闸、输出截断拒执行、`ask_question` 的独占与轮次
//! 上限、并行 `task` 组、单个工具的进度与子过程检查点、内联媒体落库、工具结果回填。
//! 09-17 从 `chat_with_tools` 里抽出;每个调用各分支都以「压一条 tool 消息 + continue」收尾,
//! 下一次迭代开始时给上一批盖执行起止。

use super::round_state::RoundState;
use super::{stamp_tool_spans, unix_ms, REPEAT_FUSE_THRESHOLD, REPEAT_SKIP_THRESHOLD};
use crate::agent::*;

pub(super) enum ToolBatchOutcome {
    /// 这一条回复被输出上限截断,工具调用一个都没执行,已经把错误回灌;回到循环顶重发。
    Truncated,
    /// 整批执行完(含被跳过 / 被拒的);`question_round_allowed` 是这一批里那个合法的
    /// `ask_question` 是否算作提问轮(算的话不占工具轮数)。
    Executed { question_round_allowed: bool },
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_round_tool_calls<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        used_tools: &mut Vec<String>,
        persisted_tool_reports: &mut Vec<(String, String)>,
        result: &mut ChatResult,
        st: &mut RoundState,
        on_event: &mut F,
    ) -> Result<ToolBatchOutcome>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        // 同参复读闸(见 repeat_gate.rs):连续相同轮先跳过执行回灌错误,
        // 到保险丝阈值置 repeat_fused——下一轮请求不再带工具,逼模型用
        // 已有结果正常成文(硬截断+英文警告拼正文会把机器文本漏到 QQ,
        // 08-24 线上翻车实录)。
        let round_repeats = st.repeat_gate.observe(&result.tool_calls);
        if round_repeats >= REPEAT_FUSE_THRESHOLD && !st.repeat_fused {
            st.repeat_fused = true;
            tracing::warn!(
                repeats = round_repeats,
                "tool repeat fuse blown; withholding tools so the model answers with existing results"
            );
        }
        let repeat_skip = round_repeats >= REPEAT_SKIP_THRESHOLD;
        st.tool_round += 1;
        let next_responses_continuation = result.responses_continuation.clone();
        push_assistant_message_with_reasoning(
            messages,
            result.content.clone(),
            result.reasoning.as_deref(),
            result.thinking_signature.as_deref(),
            // 参数的合法性由 ChatMessage::assistant 统一收口(见那里的
            // 注释);执行侧仍拿原始参数,好让工具把 `EOF while parsing`
            // 这类解析错误如实回给模型。
            Some(result.tool_calls.clone()),
            true,
        );
        if result
            .finish_reason
            .as_deref()
            .is_some_and(|reason| reason.eq_ignore_ascii_case("length"))
            && !result.tool_calls.is_empty()
        {
            // 续传簿记与正常路径同步:跳过它会让下一轮带着上一轮的旧
            // response id 续传,服务端 400 后再走自愈,白费一次请求。
            // start 必须在 push tool 错误之前设定(续传输入=工具输出段)。
            if next_responses_continuation.is_some() {
                st.continuation_input_start = messages.len();
            }
            st.responses_continuation = next_responses_continuation;
            st.continuation_context = None;
            // A "length" stop means the output hit the token limit, so every
            // tool call in this message may carry silently truncated
            // arguments. Refuse to execute any of them and let the model
            // re-issue the calls with complete arguments.
            for call in &result.tool_calls {
                messages.push(ChatMessage::tool(
                    call.id.clone(),
                    "error: this reply was truncated by the output token limit, so the tool call arguments may be incomplete. Re-issue this tool call with complete arguments.",
                ));
            }
            return Ok(ToolBatchOutcome::Truncated);
        }
        if next_responses_continuation.is_some() {
            st.continuation_input_start = messages.len();
        }
        st.responses_continuation = next_responses_continuation;
        st.continuation_context = None;
        let ask_question_enabled = self
            .tools
            .lock()
            .unwrap()
            .tool_names()
            .iter()
            .any(|name| name == "ask_question");
        let question_call_count = result
            .tool_calls
            .iter()
            .filter(|call| ask_question_enabled && call.function.name == "ask_question")
            .count();
        if question_call_count == 1 {
            st.question_rounds += 1;
        }
        let question_round_allowed =
            question_call_count == 1 && st.question_rounds <= MAX_QUESTION_ROUNDS_PER_TURN;
        let defer_sibling_tools = question_call_count == 1 && result.tool_calls.len() > 1;
        // Multiple `task` calls in one batch run concurrently (subagents
        // are independent by design); everything else stays serial.
        let mut parallel_task_outputs = if defer_sibling_tools || repeat_skip {
            std::collections::HashMap::new()
        } else {
            self.execute_parallel_task_calls(&result.tool_calls, on_event)
                .await?
        };
        // 每个调用的执行起止:一次迭代压进 `messages` 的 tool 消息就是这个
        // 调用的结果(各分支都以 push + continue 收尾),下一次迭代开始时给
        // 上一批盖章。并行 task 组早在循环前跑完,这里量到的只是入队那一瞬,
        // 与其给一个假的 0 ms,不如让它没有耗时。
        let mut span_from = messages.len();
        let mut span_since = unix_ms();
        let mut span_skip = false;
        for (call_index, call) in std::mem::take(&mut result.tool_calls)
            .into_iter()
            .enumerate()
        {
            if !span_skip {
                stamp_tool_spans(&mut messages[span_from..], span_since, unix_ms());
            }
            span_from = messages.len();
            span_since = unix_ms();
            span_skip = parallel_task_outputs.contains_key(&call_index);
            if let Some(group_output) = parallel_task_outputs.remove(&call_index) {
                // Executed in the parallel group; events already emitted.
                used_tools.push(call.function.name.clone());
                if let Some(report) = group_output.report {
                    persisted_tool_reports.push((call.function.name.clone(), report));
                }
                let model_output = self
                    .spill_tool_output(
                        current_turn_id,
                        &call.id,
                        &call.function.name,
                        &group_output.output,
                    )
                    .unwrap_or(group_output.output);
                messages.push(ChatMessage::tool(call.id, model_output));
                continue;
            }
            let call_id = call.id.clone();
            let event_name = tool_event_name(&call.function.name, &call.function.arguments);
            on_event(AgentEvent::ToolCall {
                call_id: call_id.clone(),
                name: event_name.clone(),
                arguments: call.function.arguments.clone(),
            })?;
            if repeat_skip {
                // 同参复读:不再真执行,回灌上一轮的真实结果字节。不注入
                // 指令文本——故障态模型看不见输入增量,提示无用(08-24)。
                let output = st
                    .repeat_gate
                    .cached_output(&call.function.name, &call.function.arguments);
                on_event(AgentEvent::ToolResult {
                    call_id: call_id.clone(),
                    name: event_name.clone(),
                    ok: tool_output_succeeded(&output),
                    output: output.clone(),
                })?;
                messages.push(ChatMessage::tool(call.id, output));
                continue;
            }
            if question_call_count > 1 {
                let output = "tool error: only one ask_question call is allowed per tool batch; combine all questions into one call".to_string();
                on_event(AgentEvent::ToolResult {
                    call_id: call_id.clone(),
                    name: event_name.clone(),
                    ok: false,
                    output: output.clone(),
                })?;
                messages.push(ChatMessage::tool(call.id, output));
                continue;
            }
            if defer_sibling_tools && call.function.name != "ask_question" {
                let output = "tool error: deferred until the user answers ask_question; reissue this tool call after receiving the answer".to_string();
                on_event(AgentEvent::ToolResult {
                    call_id: call_id.clone(),
                    name: event_name.clone(),
                    ok: false,
                    output: output.clone(),
                })?;
                messages.push(ChatMessage::tool(call.id, output));
                continue;
            }
            if ask_question_enabled && call.function.name == "ask_question" {
                self.run_ask_question(
                    current_turn_id,
                    messages,
                    call,
                    call_id,
                    event_name,
                    question_round_allowed,
                    on_event,
                )
                .await?;
                continue;
            }
            self.run_tool_call(
                current_turn_id,
                messages,
                used_tools,
                persisted_tool_reports,
                call,
                call_id,
                event_name,
                st,
                on_event,
            )
            .await?;
        }
        if !span_skip {
            stamp_tool_spans(&mut messages[span_from..], span_since, unix_ms());
        }
        Ok(ToolBatchOutcome::Executed {
            question_round_allowed,
        })
    }
}
