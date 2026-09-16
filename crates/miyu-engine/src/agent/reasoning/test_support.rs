//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/agent/reasoning.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

/// 测试助手：不关心批量提示的用例用这个，计数器每次从零开始。
pub fn emit_filtered_chunk<F>(
    chunk: ChatStreamChunk,
    filter: &mut ReasoningTitleFilter,
    on_event: &mut F,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    emit_filtered_chunk_at(chunk, Instant::now(), filter, &mut 0, on_event)
}

pub fn parse_reasoning_title(reasoning: &str) -> (Option<String>, String) {
    parse_reasoning_title_chunks([reasoning])
}

pub fn parse_reasoning_title_chunks<'a>(
    chunks: impl IntoIterator<Item = &'a str>,
) -> (Option<String>, String) {
    let mut filter = ReasoningTitleFilter::default();
    let mut title = None;
    let mut output = String::new();
    for chunk in chunks {
        let (chunk_title, text) = filter.push(chunk);
        title = title.or(chunk_title);
        if let Some(text) = text {
            output.push_str(&text);
        }
    }
    let (finished_title, pending) = filter.finish();
    let title = title.or(finished_title);
    if let Some(pending) = pending {
        output.push_str(&pending);
    }
    (title, output)
}
