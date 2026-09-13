//! IPC 事件解码：把 daemon 发来的 `(kind, data)` 还原成 [`AgentEvent`]。
//!
//! 编码侧的权威表是 `web/event_map.rs` 的 `RunEventMapper::handle`（唯一穷尽
//! 全部变体的那张），这里是它的逆。以前解码这件事在 `remote/one_shot.rs` 和
//! `remote/wake.rs` 各写了一份，谁漏抄一个变体，那条路径就少一个功能：
//!
//! - wake 收不到 `question.requested`，唤醒里模型提问会挂；
//! - wake 收不到 `context.compact_*` / `context.pop_*` / `context.notice`；
//! - 两边都丢 `tool.artifact`；
//! - `chat.round_usage` 在 one_shot 里没有分支，逐请求计量整轮掉地上。
//!
//! 收敛成一份之后，加 IPC 事件只要改这里 + 编码侧两处。
//!
//! **纯函数**：不碰终端、不发 IPC、不 await。需要异步副作用的两个事件
//! （图片要从库里取资产再画、问题要弹面板再回发命令）只返回一个标记，
//! 由调用方拿着原始 `data` 自己做——那部分状态各路径不同，强行收敛只会
//! 把 `Option<&mut ...>` 的分叉搬个地方。

use crate::cli::*;
use serde_json::Value;
use std::time::Instant;

/// 一条 IPC 事件解码后的去向。
pub(in crate::cli) enum DecodedIpc {
    /// 可以直接喂给渲染器 / UI 的事件。
    Event(AgentEvent),
    /// 需要调用方做异步副作用，原始 `data` 仍在调用方手里。
    Async(AsyncIpc),
    /// 队列里某条排队消息被 daemon 丢弃（从未进入对话）。没有对应的
    /// `AgentEvent` 变体——它不是回合里发生的事，是队列状态变更。
    QueueRemoved(Vec<String>),
    /// 回合跑完。这是控制流而不是 `AgentEvent`：它终止事件循环，
    /// `data` 里带着权威的用量数字（回合中逐请求的计量都是估的）。
    RunCompleted,
    /// 认识但这里不产生事件（调用方按需自己读 `data`），或者不认识。
    Ignored,
}

/// 需要调用方接手的异步副作用。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::cli) enum AsyncIpc {
    /// `tool.image`：`data` 里是落库后的资产而不是路径，要从库里取回来
    /// 再按本地终端的宽度画——daemon 量到的不是用户的终端。
    ToolImage,
    /// `tool.artifact`：同上，资产形态。
    ToolArtifact,
    /// `question.requested`：要弹面板，再按结果回发 `AnswerQuestion` /
    /// `CloseQuestion` / `Cancel` 三条命令之一。
    Question,
}

fn ipc_opt_text(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .map(std::string::ToString::to_string)
}

fn ipc_bool(data: &Value, key: &str) -> bool {
    data.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn ipc_str_array(data: &Value, key: &str) -> Vec<String> {
    data.get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(std::string::ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn chunk(kind: crate::llm::ChatStreamKind, text: &str) -> ChatStreamChunk {
    ChatStreamChunk {
        kind,
        text: text.to_string(),
    }
}

/// `(kind, data)` → 事件。`received_at` 由调用方传入而不是这里取
/// `Instant::now()`，这样重放录制的事件流时时序不会被解码这一刻污染。
pub(in crate::cli) fn decode_ipc_event_at(
    kind: &str,
    data: &Value,
    received_at: Instant,
) -> DecodedIpc {
    use crate::llm::ChatStreamKind;

    let event = match kind {
        "turn.started" => AgentEvent::TurnStarted {
            turn_id: ipc_text(data, "turn_id").to_string(),
        },
        "assistant.delta" => {
            AgentEvent::Chunk(chunk(ChatStreamKind::Content, ipc_text(data, "delta")))
        }
        "reasoning.delta" => {
            AgentEvent::Chunk(chunk(ChatStreamKind::Reasoning, ipc_text(data, "delta")))
        }
        "reasoning.start" => AgentEvent::ReasoningStart { received_at },
        "reasoning.reset" => AgentEvent::ReasoningReset { received_at },
        "reasoning.part_start" => AgentEvent::ReasoningPartStart { received_at },
        "reasoning.part_end" => AgentEvent::ReasoningPartEnd { received_at },
        "reasoning.title" => AgentEvent::ReasoningTitle(ipc_text(data, "title").to_string()),
        "tool.preparing" => AgentEvent::ToolPreparing {
            name: ipc_text(data, "name").to_string(),
            batch: ipc_bool(data, "batch"),
        },
        "tool.started" => AgentEvent::ToolCall {
            call_id: ipc_text(data, "tool_id").to_string(),
            name: ipc_text(data, "name").to_string(),
            arguments: ipc_text(data, "arguments").to_string(),
        },
        "tool.progress" => AgentEvent::ToolProgress {
            call_id: ipc_text(data, "tool_id").to_string(),
            name: ipc_text(data, "name").to_string(),
            message: ipc_text(data, "message").to_string(),
        },
        "tool.output" => AgentEvent::CommandOutput {
            call_id: ipc_text(data, "tool_id").to_string(),
            name: ipc_text(data, "name").to_string(),
            stream: if ipc_text(data, "stream") == "stderr" {
                tools::CommandOutputStream::Stderr
            } else {
                tools::CommandOutputStream::Stdout
            },
            chunk: ipc_text(data, "output").as_bytes().to_vec(),
        },
        "tool.finished" => AgentEvent::ToolResult {
            call_id: ipc_text(data, "tool_id").to_string(),
            name: ipc_text(data, "name").to_string(),
            ok: ipc_bool(data, "ok"),
            output: ipc_text(data, "output").to_string(),
        },
        "tool.image" => return DecodedIpc::Async(AsyncIpc::ToolImage),
        "tool.artifact" => return DecodedIpc::Async(AsyncIpc::ToolArtifact),
        "question.requested" => return DecodedIpc::Async(AsyncIpc::Question),
        "queue.consumed" => AgentEvent::QueuedPromptsConsumed {
            prompt_ids: ipc_str_array(data, "prompt_ids"),
            mode: match ipc_text(data, "mode") {
                "dev" => AgentMode::Dev,
                _ => AgentMode::Normal,
            },
            provider_id: ipc_opt_text(data, "provider_id"),
            model: ipc_opt_text(data, "model"),
        },
        // 单数 `prompt_id`：编码侧一次只丢一条。
        "queue.removed" => {
            return match ipc_opt_text(data, "prompt_id") {
                Some(id) => DecodedIpc::QueueRemoved(vec![id]),
                None => DecodedIpc::Ignored,
            }
        }
        "generation.superseded" => AgentEvent::GenerationSuperseded {
            prompt_ids: ipc_str_array(data, "prompt_ids"),
        },
        "chat.round_usage" => {
            let round: Usage = data
                .get("usage")
                .cloned()
                .and_then(|usage| serde_json::from_value(usage).ok())
                .unwrap_or_default();
            AgentEvent::RoundUsage {
                round: Box::new(round),
                turn: TurnTokens {
                    total: ipc_u64(data, "turn_total"),
                    prompt: ipc_u64(data, "turn_prompt"),
                    cache_read: ipc_u64(data, "turn_cache_read"),
                },
                // 会话实时累计（已落库的各回合 + 子代理子会话 + 本回合至今）。
                // daemon 逐请求算好了送过来，终端这边不必自己叠。
                cumulative: TurnTokens {
                    total: ipc_u64(data, "cumulative_tokens"),
                    prompt: ipc_u64(data, "cumulative_prompt_tokens"),
                    cache_read: ipc_u64(data, "cumulative_cache_read_tokens"),
                },
                speed: GenerationSpeed {
                    tokens: ipc_u64(data, "turn_generation_tokens"),
                    millis: ipc_u64(data, "turn_generation_ms"),
                },
                estimated: ipc_bool(data, "estimated"),
                provider_id: ipc_opt_text(data, "provider_id"),
                model: ipc_opt_text(data, "model"),
            }
        }
        "context.compact_start" => AgentEvent::CompactStart,
        "context.compact_delta" => {
            AgentEvent::CompactChunk(chunk(ChatStreamKind::Content, ipc_text(data, "delta")))
        }
        "context.compact_end" => AgentEvent::CompactEnd,
        "context.pop_start" => AgentEvent::PopStart,
        "context.pop_end" => AgentEvent::PopEnd,
        "context.notice" => AgentEvent::Notice {
            text: ipc_text(data, "text").to_string(),
        },
        "run.completed" => return DecodedIpc::RunCompleted,
        _ => return DecodedIpc::Ignored,
    };
    DecodedIpc::Event(event)
}

/// `decode_ipc_event_at` 的常用形态：接收时刻即此刻。
pub(in crate::cli) fn decode_ipc_event(kind: &str, data: &Value) -> DecodedIpc {
    decode_ipc_event_at(kind, data, Instant::now())
}
