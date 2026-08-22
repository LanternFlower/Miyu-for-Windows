//! 用户输入的清洗。

use super::shared::*;
use crate::agent::*;

#[test]
fn strips_pasted_system_reminder_from_user_input() {
    let input = "继续<system-reminder>hidden</system-reminder> ok";

    assert_eq!(clean_user_visible_text(input), "继续 ok");
}

#[test]
fn strips_unclosed_system_reminder_from_user_input() {
    let input = "继续<system_reminder>hidden";

    assert_eq!(clean_user_visible_text(input), "继续");
}

#[test]
fn raw_reasoning_is_batched_before_filtered_display() {
    let temp = tempfile::tempdir().unwrap();
    let state = crate::state::StateStore::new(&test_paths(temp.path())).unwrap();
    state
        .start_turn("reasoning-turn", "long task", std::process::id())
        .unwrap();
    let mut sink = TurnJournalSink::new(state.clone(), "reasoning-turn".to_string(), 0);
    let mut displayed = Vec::new();
    {
        let mut on_event = |event| {
            if let AgentEvent::Chunk(chunk) = event {
                displayed.push(chunk.text);
            }
            Ok(())
        };
        sink.emit(
            AgentEvent::RawReasoning(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text: "raw reasoning".to_string(),
            }),
            &mut on_event,
        )
        .unwrap();
        sink.emit(
            AgentEvent::Chunk(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text: "filtered reasoning".to_string(),
            }),
            &mut on_event,
        )
        .unwrap();
    }
    assert!(displayed.is_empty());
    assert!(state.load_turns().unwrap()[0].journal_events.is_empty());

    {
        let mut on_event = |event| {
            if let AgentEvent::Chunk(chunk) = event {
                displayed.push(chunk.text);
            }
            Ok(())
        };
        sink.emit(AgentEvent::SpinnerTick, &mut on_event).unwrap();
    }

    assert_eq!(displayed, ["filtered reasoning"]);
    assert_eq!(state.load_turns().unwrap()[0].journal_events.len(), 1);
    assert_eq!(
        state.load_turns().unwrap()[0].journal_events[0]
            .text_payload
            .as_deref(),
        Some("raw reasoning")
    );
}
