//! IPC 事件解码表。
//!
//! 这张表以前抄在 `one_shot.rs` 和 `wake.rs` 两处，漏抄的变体就是 bug 现场。
//! 收敛成一份后，这里的「全集覆盖」用例负责让下一次漏抄当场报红。

use crate::cli::ipc_event::*;
use crate::cli::*;
use serde_json::json;

/// `web/event_map.rs` 里 `RunEventMapper::handle` 发出的全部 kind。
/// 加 IPC 事件时这张表要一起加，否则新事件在终端侧静默掉地上。
const ALL_KINDS: &[&str] = &[
    "turn.started",
    "assistant.delta",
    "reasoning.delta",
    "reasoning.start",
    "reasoning.reset",
    "reasoning.part_start",
    "reasoning.part_end",
    "reasoning.title",
    "tool.started",
    "tool.preparing",
    "tool.progress",
    "tool.output",
    "tool.finished",
    "tool.image",
    "tool.artifact",
    "question.requested",
    "queue.consumed",
    "queue.removed",
    "generation.superseded",
    "chat.round_usage",
    "context.compact_start",
    "context.compact_delta",
    "context.compact_end",
    "context.pop_start",
    "context.pop_end",
    "context.notice",
];

#[test]
fn every_published_kind_is_decoded() {
    for kind in ALL_KINDS {
        // `queue.removed` 要带上 prompt_id 才有内容可解。
        let data = json!({ "prompt_id": "p1" });
        assert!(
            !matches!(decode_ipc_event(kind, &data), DecodedIpc::Ignored),
            "IPC 事件 {kind} 没有解码分支——终端侧会静默丢掉它"
        );
    }
}

#[test]
fn run_completed_is_control_flow_not_an_event() {
    // 它终止事件循环，不能被当成「不认识」丢掉——漏了就永远等不到回合结束。
    assert!(matches!(
        decode_ipc_event("run.completed", &json!({})),
        DecodedIpc::RunCompleted
    ));
}

#[test]
fn unknown_kind_is_ignored() {
    assert!(matches!(
        decode_ipc_event("chat.made_up", &json!({})),
        DecodedIpc::Ignored
    ));
}

#[test]
fn async_side_effects_are_flagged_not_decoded() {
    let image = decode_ipc_event("tool.image", &json!({}));
    assert!(matches!(image, DecodedIpc::Async(AsyncIpc::ToolImage)));
    let artifact = decode_ipc_event("tool.artifact", &json!({}));
    assert!(matches!(
        artifact,
        DecodedIpc::Async(AsyncIpc::ToolArtifact)
    ));
    let question = decode_ipc_event("question.requested", &json!({}));
    assert!(matches!(question, DecodedIpc::Async(AsyncIpc::Question)));
}

#[test]
fn tool_finished_carries_identity_and_outcome() {
    let data = json!({
        "tool_id": "call-1",
        "name": "run_command",
        "ok": true,
        "output": "done",
    });
    let DecodedIpc::Event(AgentEvent::ToolResult {
        call_id,
        name,
        ok,
        output,
    }) = decode_ipc_event("tool.finished", &data)
    else {
        panic!("tool.finished 应当解码成 ToolResult");
    };
    assert_eq!(call_id, "call-1");
    assert_eq!(name, "run_command");
    assert!(ok);
    assert_eq!(output, "done");
}

#[test]
fn missing_ok_flag_means_failure() {
    // 编码侧永远带 ok，但解码不能因为字段缺失就把失败读成成功。
    let DecodedIpc::Event(AgentEvent::ToolResult { ok, .. }) =
        decode_ipc_event("tool.finished", &json!({ "tool_id": "c" }))
    else {
        panic!("tool.finished 应当解码成 ToolResult");
    };
    assert!(!ok);
}

#[test]
fn command_output_splits_streams() {
    let stderr = decode_ipc_event(
        "tool.output",
        &json!({ "tool_id": "c", "stream": "stderr", "output": "boom" }),
    );
    let DecodedIpc::Event(AgentEvent::CommandOutput { stream, chunk, .. }) = stderr else {
        panic!("tool.output 应当解码成 CommandOutput");
    };
    assert!(matches!(stream, tools::CommandOutputStream::Stderr));
    assert_eq!(chunk, b"boom");

    let stdout = decode_ipc_event(
        "tool.output",
        &json!({ "tool_id": "c", "stream": "stdout", "output": "ok" }),
    );
    let DecodedIpc::Event(AgentEvent::CommandOutput { stream, .. }) = stdout else {
        panic!("tool.output 应当解码成 CommandOutput");
    };
    assert!(matches!(stream, tools::CommandOutputStream::Stdout));
}

#[test]
fn deltas_land_in_the_right_stream_kind() {
    let DecodedIpc::Event(AgentEvent::Chunk(content)) =
        decode_ipc_event("assistant.delta", &json!({ "delta": "hi" }))
    else {
        panic!("assistant.delta 应当解码成 Chunk");
    };
    assert_eq!(content.kind, crate::llm::ChatStreamKind::Content);
    assert_eq!(content.text, "hi");

    let DecodedIpc::Event(AgentEvent::Chunk(reasoning)) =
        decode_ipc_event("reasoning.delta", &json!({ "delta": "think" }))
    else {
        panic!("reasoning.delta 应当解码成 Chunk");
    };
    assert_eq!(reasoning.kind, crate::llm::ChatStreamKind::Reasoning);
}

#[test]
fn queue_consumed_keeps_mode_and_endpoint() {
    let data = json!({
        "prompt_ids": ["a", "b"],
        "mode": "dev",
        "provider_id": "claude-code",
        "model": "opus",
    });
    let DecodedIpc::Event(AgentEvent::QueuedPromptsConsumed {
        prompt_ids,
        mode,
        provider_id,
        model,
    }) = decode_ipc_event("queue.consumed", &data)
    else {
        panic!("queue.consumed 应当解码成 QueuedPromptsConsumed");
    };
    assert_eq!(prompt_ids, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(mode, AgentMode::Dev);
    assert_eq!(provider_id.as_deref(), Some("claude-code"));
    assert_eq!(model.as_deref(), Some("opus"));
}

#[test]
fn unknown_mode_falls_back_to_normal() {
    let DecodedIpc::Event(AgentEvent::QueuedPromptsConsumed { mode, .. }) =
        decode_ipc_event("queue.consumed", &json!({ "mode": "moonshot" }))
    else {
        panic!("queue.consumed 应当解码成 QueuedPromptsConsumed");
    };
    assert_eq!(mode, AgentMode::Normal);
}

#[test]
fn queue_removed_is_a_queue_change_not_an_event() {
    let removed = decode_ipc_event("queue.removed", &json!({ "prompt_id": "p7" }));
    let DecodedIpc::QueueRemoved(ids) = removed else {
        panic!("queue.removed 应当解码成 QueueRemoved");
    };
    assert_eq!(ids, vec!["p7".to_string()]);
}

#[test]
fn round_usage_rebuilds_the_whole_snapshot() {
    // one_shot 以前只手解了 prompt/completion 两个字段，其余靠边；
    // `Usage` 本身可反序列化，整份还原才能让 footer 的口径和进程内那条路一致。
    let data = json!({
        "usage": {
            "prompt_tokens": 1000,
            "completion_tokens": 200,
            "total_tokens": 1200,
            "cache_read_tokens": 800,
        },
        "turn_total": 5000,
        "turn_prompt": 4000,
        "turn_cache_read": 3000,
        "cumulative_tokens": 9000,
        "cumulative_prompt_tokens": 7000,
        "cumulative_cache_read_tokens": 6000,
        "turn_generation_tokens": 200,
        "turn_generation_ms": 4000,
        "estimated": true,
        "provider_id": "codex",
        "model": "gpt",
    });
    let DecodedIpc::Event(AgentEvent::RoundUsage {
        round,
        turn,
        cumulative,
        speed,
        estimated,
        provider_id,
        model,
    }) = decode_ipc_event("chat.round_usage", &data)
    else {
        panic!("chat.round_usage 应当解码成 RoundUsage");
    };
    assert_eq!(round.prompt_tokens, 1000);
    assert_eq!(round.completion_tokens, 200);
    assert_eq!(round.cache_read_tokens, 800);
    // footer 的上下文占用口径：prompt + completion。
    assert_eq!(
        round.prompt_tokens.saturating_add(round.completion_tokens),
        1200
    );
    assert_eq!(turn.total, 5000);
    assert_eq!(turn.prompt, 4000);
    assert_eq!(turn.cache_read, 3000);
    // 会话实时累计由 daemon 算好送来（含已完成的子代理），终端不自己叠。
    assert_eq!(cumulative.total, 9000);
    assert_eq!(cumulative.prompt, 7000);
    assert_eq!(cumulative.cache_read, 6000);
    assert_eq!(speed.tokens, 200);
    assert_eq!(speed.millis, 4000);
    assert!(estimated);
    assert_eq!(provider_id.as_deref(), Some("codex"));
    assert_eq!(model.as_deref(), Some("gpt"));
}

#[test]
fn malformed_usage_does_not_lose_the_event() {
    // 计量字段坏了也得把事件交出去，否则 footer 卡在上一帧还找不到原因。
    let DecodedIpc::Event(AgentEvent::RoundUsage { round, turn, .. }) =
        decode_ipc_event("chat.round_usage", &json!({ "usage": "not an object" }))
    else {
        panic!("chat.round_usage 应当解码成 RoundUsage");
    };
    assert_eq!(round.prompt_tokens, 0);
    assert_eq!(turn.total, 0);
}

#[test]
fn superseded_carries_the_prompt_ids() {
    let DecodedIpc::Event(AgentEvent::GenerationSuperseded { prompt_ids }) =
        decode_ipc_event("generation.superseded", &json!({ "prompt_ids": ["x"] }))
    else {
        panic!("generation.superseded 应当解码成 GenerationSuperseded");
    };
    assert_eq!(prompt_ids, vec!["x".to_string()]);
}

#[test]
fn context_lifecycle_events_decode() {
    assert!(matches!(
        decode_ipc_event("context.compact_start", &json!({})),
        DecodedIpc::Event(AgentEvent::CompactStart)
    ));
    assert!(matches!(
        decode_ipc_event("context.compact_end", &json!({})),
        DecodedIpc::Event(AgentEvent::CompactEnd)
    ));
    assert!(matches!(
        decode_ipc_event("context.pop_start", &json!({})),
        DecodedIpc::Event(AgentEvent::PopStart)
    ));
    assert!(matches!(
        decode_ipc_event("context.pop_end", &json!({})),
        DecodedIpc::Event(AgentEvent::PopEnd)
    ));
    let DecodedIpc::Event(AgentEvent::Notice { text }) = decode_ipc_event(
        "context.notice",
        &json!({ "text": "窗口太小，自动压缩暂停" }),
    ) else {
        panic!("context.notice 应当解码成 Notice");
    };
    assert_eq!(text, "窗口太小，自动压缩暂停");
}

#[test]
fn reasoning_timestamps_come_from_the_caller() {
    // 重放录制的事件流时，时序不能被「解码这一刻」污染。
    let base = std::time::Instant::now();
    let DecodedIpc::Event(AgentEvent::ReasoningStart { received_at }) =
        decode_ipc_event_at("reasoning.start", &json!({}), base)
    else {
        panic!("reasoning.start 应当解码成 ReasoningStart");
    };
    assert_eq!(received_at, base);
}
