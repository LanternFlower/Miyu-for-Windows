//! 推理标题的识别、过滤与流式切分。

use crate::agent::*;

#[test]
fn mode_reminder_does_not_inject_a_reasoning_title_protocol() {
    let prompt = with_mode_reminder("base".to_string(), AgentMode::Normal);
    assert_eq!(prompt, "base");
    assert!(!prompt.contains("<runtime"));

    // Dev 遵循极简原则:与 Normal 一样零模式提醒。
    let prompt = with_mode_reminder("base".to_string(), AgentMode::Dev);
    assert_eq!(prompt, "base");
}

#[test]
fn reasoning_title_filter_emits_completed_markdown_title_immediately() {
    let mut filter = ReasoningTitleFilter::default();
    assert_eq!(filter.push("**Preparing to"), (None, None));
    assert_eq!(
        filter.push(" call tools**"),
        (Some("Preparing to call tools".to_string()), None)
    );
    assert_eq!(filter.finish(), (None, None));
}

#[test]
fn reasoning_title_filter_strips_delayed_blank_line_before_body() {
    let mut filter = ReasoningTitleFilter::default();
    assert_eq!(
        filter.push("**Preparing to call tools**\n"),
        (Some("Preparing to call tools".to_string()), None)
    );
    assert_eq!(
        filter.push("\nInspect the arguments."),
        (None, Some("Inspect the arguments.".to_string()))
    );
}

#[test]
fn reasoning_title_filter_streams_plain_body_without_inventing_title() {
    let mut filter = ReasoningTitleFilter::default();
    assert_eq!(
        filter.push("The user is"),
        (None, Some("The user is".to_string()))
    );
    assert_eq!(
        filter.push(" asking what changed."),
        (None, Some(" asking what changed.".to_string()))
    );
    assert_eq!(
        filter.push(" Continue analysis."),
        (None, Some(" Continue analysis.".to_string()))
    );
    assert_eq!(filter.finish(), (None, None));
}

#[test]
fn reasoning_title_filter_keeps_long_markdown_heading_text() {
    let title = "heading ".repeat(12);
    let text = format!("# {title}\n\nBody reasoning.");
    let mut filter = ReasoningTitleFilter::default();
    let (parsed_title, body) = filter.push(&text);

    assert!(parsed_title.is_some());
    assert_eq!(body.as_deref(), Some("Body reasoning."));
    assert_eq!(filter.finish(), (None, None));
}

#[test]
fn reasoning_title_filter_extracts_markdown_action_heading() {
    assert_eq!(
        parse_reasoning_title(
            "**Planning response approach and title clipping**\n\nInspect the renderer."
        ),
        (
            Some("Planning response approach and title clipping".to_string()),
            "Inspect the renderer.".to_string()
        )
    );
}

#[test]
fn reasoning_title_filter_keeps_ordinary_bold_text_in_body() {
    assert_eq!(
        parse_reasoning_title("**Important:** keep this in the body."),
        (None, "**Important:** keep this in the body.".to_string())
    );
}

#[test]
fn reasoning_title_filter_matches_unsplit_input_at_every_character_boundary() {
    for text in [
        "**检查参数**\n\n\n继续分析。",
        "## 检查参数\n\n\n继续分析。",
        "**Checking arguments**\r\n\r\nContinue analysis.",
        "#include <stdio.h>",
    ] {
        let expected = parse_reasoning_title(text);
        for split in text
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(text.len()))
        {
            assert_eq!(
                parse_reasoning_title_chunks([&text[..split], &text[split..]]),
                expected,
                "different result when split at byte {split} in {text:?}"
            );
        }
    }
}

#[test]
fn reasoning_title_filter_does_not_show_incomplete_bold_title() {
    assert_eq!(
        parse_reasoning_title("**Incomplete title"),
        (None, "**Incomplete title".to_string())
    );
}

#[test]
fn reasoning_title_filter_does_not_use_first_sentence_as_title() {
    assert_eq!(
        parse_reasoning_title("Designing the clipping helper. Keep the rest."),
        (
            None,
            "Designing the clipping helper. Keep the rest.".to_string()
        )
    );
}

#[test]
fn reasoning_part_start_reopens_title_detection() {
    let mut filter = ReasoningTitleFilter::default();
    let mut titles = Vec::new();
    let mut reasoning = Vec::new();
    let mut on_event = |event| {
        match event {
            AgentEvent::ReasoningTitle(title) => titles.push(title),
            AgentEvent::Chunk(chunk) if chunk.kind == ChatStreamKind::Reasoning => {
                reasoning.push(chunk.text);
            }
            _ => {}
        }
        Ok(())
    };

    emit_filtered_chunk(
        ChatStreamChunk {
            kind: ChatStreamKind::ReasoningPartStart,
            text: String::new(),
        },
        &mut filter,
        &mut on_event,
    )
    .unwrap();
    emit_filtered_chunk(
        ChatStreamChunk {
            kind: ChatStreamKind::Reasoning,
            text: "**First title**\n\nFirst body.".to_string(),
        },
        &mut filter,
        &mut on_event,
    )
    .unwrap();
    emit_filtered_chunk(
        ChatStreamChunk {
            kind: ChatStreamKind::ReasoningPartEnd,
            text: String::new(),
        },
        &mut filter,
        &mut on_event,
    )
    .unwrap();
    emit_filtered_chunk(
        ChatStreamChunk {
            kind: ChatStreamKind::ReasoningPartStart,
            text: String::new(),
        },
        &mut filter,
        &mut on_event,
    )
    .unwrap();
    emit_filtered_chunk(
        ChatStreamChunk {
            kind: ChatStreamKind::Reasoning,
            text: "**Second title**".to_string(),
        },
        &mut filter,
        &mut on_event,
    )
    .unwrap();

    assert_eq!(titles, vec!["First title", "Second title"]);
    assert_eq!(reasoning, vec!["First body."]);
}

#[test]
fn reasoning_summary_finishes_before_answer_content() {
    let mut filter = ReasoningTitleFilter::default();
    let mut events = Vec::new();
    let mut on_event = |event| {
        events.push(match event {
            AgentEvent::ReasoningPartStart { .. } => "part-start".to_string(),
            AgentEvent::ReasoningTitle(title) => format!("title:{title}"),
            AgentEvent::Chunk(chunk) => format!("{:?}:{}", chunk.kind, chunk.text),
            AgentEvent::ReasoningPartEnd { .. } => "part-end".to_string(),
            _ => "other".to_string(),
        });
        Ok(())
    };

    for chunk in [
        ChatStreamChunk {
            kind: ChatStreamKind::ReasoningPartStart,
            text: String::new(),
        },
        ChatStreamChunk {
            kind: ChatStreamKind::Reasoning,
            text: "**Checking event order**\n\nSummary body.".to_string(),
        },
        ChatStreamChunk {
            kind: ChatStreamKind::ReasoningPartEnd,
            text: String::new(),
        },
        ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: "Answer.".to_string(),
        },
    ] {
        emit_filtered_chunk(chunk, &mut filter, &mut on_event).unwrap();
    }

    assert_eq!(
        events,
        [
            "part-start",
            "title:Checking event order",
            "Reasoning:Summary body.",
            "part-end",
            "Content:Answer.",
        ]
    );
}

#[test]
fn reasoning_boundaries_preserve_chunk_receive_timestamps() {
    let mut filter = ReasoningTitleFilter::default();
    let started_at = Instant::now();
    let ended_at = started_at + Duration::from_millis(725);
    let mut boundaries = Vec::new();
    let mut on_event = |event| {
        match event {
            AgentEvent::ReasoningPartStart { received_at } => {
                boundaries.push(("start", received_at));
            }
            AgentEvent::ReasoningPartEnd { received_at } => {
                boundaries.push(("end", received_at));
            }
            _ => {}
        }
        Ok(())
    };

    emit_filtered_chunk_at(
        ChatStreamChunk {
            kind: ChatStreamKind::ReasoningPartStart,
            text: String::new(),
        },
        started_at,
        &mut filter,
        &mut 0,
        &mut on_event,
    )
    .unwrap();
    emit_filtered_chunk_at(
        ChatStreamChunk {
            kind: ChatStreamKind::ReasoningPartEnd,
            text: String::new(),
        },
        ended_at,
        &mut filter,
        &mut 0,
        &mut on_event,
    )
    .unwrap();

    assert_eq!(boundaries, [("start", started_at), ("end", ended_at)]);
}

#[test]
fn reasoning_title_filter_does_not_treat_hash_include_as_heading() {
    assert_eq!(
        parse_reasoning_title("#include <stdio.h>"),
        (None, "#include <stdio.h>".to_string())
    );
}
