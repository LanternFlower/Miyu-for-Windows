mod compact;
mod conversation;
pub(crate) mod overflow;

use crate::clipboard::{ClipboardImage, PastedImage};
use crate::config::{AppConfig, PromptAudience};
use crate::host_info::xml_attr_escape;
use crate::llm::{
    ChatContent, ChatContentPart, ChatMessage, ChatResult, ChatStreamChunk, ChatStreamKind,
    ImageUrlContent, OpenAiCompatibleClient, ToolCall, ToolCallFunction, TurnTokens, Usage,
};
use crate::memory::{EvictedTurn, MemoryAccess, MemoryOrganizerHandle, MemoryOrigin, MemoryStore};
use crate::paths::MiyuPaths;
use crate::platforms::{PlatformContextImageRef, PlatformTurnContext};
use crate::question::{
    answered_tool_output, closed_tool_output, unavailable_tool_output, QuestionCancelled,
    QuestionExchange, QuestionRequest, QuestionResponse,
};
use crate::render::wait_spinner::SPINNER_INTERVAL;
use crate::state::{
    QueuedPrompt, QueuedPromptAttachment, RedoCandidate, RedoInputKind, StateStore,
    TurnRedoCheckpointPayload,
};
use crate::tools::{self, memes, vision, ToolPermission, ToolRegistry};
use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::Local;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Notify};

const MAX_QUESTION_ROUNDS_PER_TURN: usize = 8;

pub struct PendingTurnGuard {
    state: StateStore,
    turn_id: String,
    completed: bool,
}

impl PendingTurnGuard {
    pub fn new(state: StateStore, turn_id: String) -> Self {
        Self {
            state,
            turn_id,
            completed: false,
        }
    }

    pub fn complete_with_model(
        mut self,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        self.state.complete_turn_with_usage_and_model(
            &self.turn_id,
            content,
            reasoning,
            provider_id,
            model,
            tokens,
            token_usage_estimated,
        )?;
        self.completed = true;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn interrupt(&mut self) -> Result<()> {
        if !self.completed {
            self.state.interrupt_turn(&self.turn_id)?;
            self.completed = true;
        }
        Ok(())
    }
}

impl Drop for PendingTurnGuard {
    fn drop(&mut self) {
        if !self.completed {
            if let Err(error) = self.state.interrupt_turn(&self.turn_id) {
                tracing::error!(
                    turn_id = %self.turn_id,
                    error = %error,
                    "failed to persist an interrupted turn"
                );
            }
        }
    }
}

struct PendingRedoGuard {
    state: StateStore,
    turn_id: String,
    revision: i64,
    completed: bool,
}

impl PendingRedoGuard {
    fn new(state: StateStore, turn_id: String, revision: i64) -> Self {
        Self {
            state,
            turn_id,
            revision,
            completed: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn complete_with_model(
        mut self,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        self.state.complete_turn_revision_with_usage_and_model(
            &self.turn_id,
            self.revision,
            content,
            reasoning,
            provider_id,
            model,
            tokens,
            token_usage_estimated,
        )?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for PendingRedoGuard {
    fn drop(&mut self) {
        if !self.completed {
            if let Err(error) = self
                .state
                .interrupt_turn_revision(&self.turn_id, self.revision)
            {
                tracing::error!(
                    turn_id = %self.turn_id,
                    revision = self.revision,
                    error = %error,
                    "failed to recover an interrupted redo generation"
                );
            }
        }
    }
}

pub struct RedoPromptInput {
    pub prompt_id: String,
    pub content: String,
    pub display_content: String,
    pub images: Vec<Option<PastedImage>>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AgentMode {
    Normal,
    Chat,
}

#[derive(Clone)]
pub struct AgentTurnControl {
    mode: Arc<Mutex<AgentMode>>,
    normal_tools: ToolRegistry,
    chat_tools: ToolRegistry,
    queue_ingress: Option<Arc<QueueIngressBarrier>>,
    supersede: Option<Arc<TurnSupersedeSignal>>,
    supersede_seen: Arc<AtomicU64>,
}

#[derive(Default)]
pub(crate) struct TurnSupersedeSignal {
    generation: AtomicU64,
    changed: Notify,
}

impl TurnSupersedeSignal {
    pub(crate) fn trigger(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.changed.notify_waiters();
        generation
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    async fn wait_after(&self, observed: u64) {
        loop {
            let changed = self.changed.notified();
            if self.generation() != observed {
                return;
            }
            changed.await;
        }
    }
}

#[derive(Default)]
pub(crate) struct QueueIngressBarrier {
    state: Mutex<QueueIngressState>,
    changed: Notify,
}

#[derive(Default)]
struct QueueIngressState {
    active_calls: HashSet<String>,
    reservations: usize,
    closed: bool,
}

pub(crate) struct QueueIngressReservation {
    barrier: Arc<QueueIngressBarrier>,
}

impl QueueIngressBarrier {
    pub(crate) fn tool_started(&self, call_id: &str) {
        let mut state = self.state.lock().unwrap();
        if !state.closed {
            state.active_calls.insert(call_id.to_string());
        }
    }

    pub(crate) fn tool_finished(&self, call_id: &str) {
        self.state.lock().unwrap().active_calls.remove(call_id);
        self.changed.notify_waiters();
    }

    pub(crate) fn try_reserve(self: &Arc<Self>) -> Option<QueueIngressReservation> {
        let mut state = self.state.lock().unwrap();
        if state.closed || state.active_calls.is_empty() {
            return None;
        }
        state.reservations = state.reservations.saturating_add(1);
        Some(QueueIngressReservation {
            barrier: self.clone(),
        })
    }

    pub(crate) fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.active_calls.clear();
        self.changed.notify_waiters();
    }

    async fn wait_for_reserved_ingress(&self) {
        loop {
            let changed = self.changed.notified();
            if self.state.lock().unwrap().reservations == 0 {
                return;
            }
            changed.await;
        }
    }
}

impl Drop for QueueIngressReservation {
    fn drop(&mut self) {
        let mut state = self.barrier.state.lock().unwrap();
        state.reservations = state.reservations.saturating_sub(1);
        self.barrier.changed.notify_waiters();
    }
}

impl AgentTurnControl {
    pub fn new(
        mode: AgentMode,
        normal_tools: ToolRegistry,
        chat_tools: ToolRegistry,
    ) -> Self {
        Self {
            mode: Arc::new(Mutex::new(mode)),
            normal_tools,
            chat_tools,
            queue_ingress: None,
            supersede: None,
            supersede_seen: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn set_queue_ingress(&mut self, ingress: Arc<QueueIngressBarrier>) {
        self.queue_ingress = Some(ingress);
    }

    pub(crate) fn set_supersede_signal(&mut self, signal: Arc<TurnSupersedeSignal>) {
        self.supersede = Some(signal);
    }

    fn pending_supersede_generation(&self) -> Option<u64> {
        let generation = self.supersede.as_ref()?.generation();
        (generation != self.supersede_seen.load(Ordering::Acquire)).then_some(generation)
    }

    fn mark_supersede_seen(&self, generation: u64) {
        self.supersede_seen.store(generation, Ordering::Release);
    }

    pub fn mode(&self) -> AgentMode {
        *self.mode.lock().unwrap()
    }

    pub fn set_mode(&self, mode: AgentMode) {
        *self.mode.lock().unwrap() = mode;
    }

    fn tools(&self, mode: AgentMode) -> ToolRegistry {
        match mode {
            AgentMode::Normal => self.normal_tools.clone(),
            AgentMode::Chat => self.chat_tools.clone(),
        }
    }
}

impl AgentMode {
    pub fn label(self) -> &'static str {
        if crate::i18n::is_zh() {
            match self {
                Self::Normal => "普通",
                Self::Chat => "闲聊",
            }
        } else {
            match self {
                Self::Normal => "NORMAL",
                Self::Chat => "CHAT",
            }
        }
    }

    fn reminder(self) -> Option<&'static str> {
        match self {
            Self::Normal => None,
            Self::Chat => Some(crate::prompts::CHAT_REMINDER),
        }
    }
}

#[derive(Debug)]
pub enum AgentEvent {
    TurnStarted {
        turn_id: String,
    },
    Chunk(ChatStreamChunk),
    /// Raw provider reasoning, persisted before the UI title/body filter.
    /// This event is consumed by `TurnJournalSink` and is never shown to a
    /// transport directly.
    RawReasoning(ChatStreamChunk),
    /// Internal durability barrier used before non-stream state mutations that
    /// create journal boundaries.
    FlushJournal,
    ReasoningStart {
        received_at: Instant,
    },
    ReasoningReset {
        received_at: Instant,
    },
    ReasoningPartStart {
        received_at: Instant,
    },
    ReasoningPartEnd {
        received_at: Instant,
    },
    ReasoningTitle(String),
    ToolCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    ToolPreparing {
        name: String,
    },
    ToolResult {
        call_id: String,
        name: String,
        ok: bool,
        output: String,
    },
    ToolProgress {
        call_id: String,
        name: String,
        message: String,
    },
    CommandOutput {
        call_id: String,
        name: String,
        stream: tools::CommandOutputStream,
        chunk: Vec<u8>,
    },
    PrepareForExternalOutput {
        ready: oneshot::Sender<bool>,
    },
    Image {
        call_id: String,
        name: String,
        path: PathBuf,
        alt: String,
    },
    Artifact {
        call_id: String,
        name: String,
        path: PathBuf,
        title: String,
    },
    AskQuestion {
        call_id: String,
        request: QuestionRequest,
        responder: oneshot::Sender<QuestionResponse>,
    },
    QueuedPromptsConsumed {
        prompt_ids: Vec<String>,
        mode: AgentMode,
        provider_id: Option<String>,
        model: Option<String>,
    },
    GenerationSuperseded {
        prompt_ids: Vec<String>,
    },
    SpinnerTick,
    CompactStart,
    CompactChunk(ChatStreamChunk),
    CompactEnd,
    PopStart,
    PopEnd,
    /// One-shot operational notice shown to the user (e.g. auto-compaction
    /// paused because the window is too small).
    Notice {
        text: String,
    },
}

const JOURNAL_FLUSH_BYTES: usize = 16 * 1024;
const JOURNAL_FLUSH_INTERVAL: Duration = Duration::from_millis(80);

struct PendingJournalChunk {
    kind: ChatStreamKind,
    text: String,
}

/// Persists semantic stream events before forwarding them to a transport.
/// Small adjacent deltas are coalesced so a long answer does not turn into a
/// SQLite transaction per provider token.
struct TurnJournalSink {
    state: StateStore,
    turn_id: String,
    revision: i64,
    segment_index: i64,
    pending: Option<PendingJournalChunk>,
    pending_reasoning_display: String,
    last_flush: Instant,
}

impl TurnJournalSink {
    fn new(state: StateStore, turn_id: String, revision: i64) -> Self {
        Self {
            state,
            turn_id,
            revision,
            segment_index: 0,
            pending: None,
            pending_reasoning_display: String::new(),
            last_flush: Instant::now(),
        }
    }

    fn emit<F>(&mut self, event: AgentEvent, on_event: &mut F) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        match event {
            AgentEvent::Chunk(chunk)
                if matches!(
                    chunk.kind,
                    ChatStreamKind::Content | ChatStreamKind::ToolCall
                ) =>
            {
                self.push_chunk(chunk, on_event)
            }
            AgentEvent::Chunk(chunk) if chunk.kind == ChatStreamKind::Reasoning => {
                self.pending_reasoning_display.push_str(&chunk.text);
                Ok(())
            }
            AgentEvent::RawReasoning(chunk) => {
                if chunk.kind == ChatStreamKind::Reasoning && !chunk.text.is_empty() {
                    self.push_chunk(chunk, on_event)
                } else {
                    Ok(())
                }
            }
            AgentEvent::FlushJournal => self.flush(on_event),
            AgentEvent::SpinnerTick => {
                self.flush(on_event)?;
                on_event(AgentEvent::SpinnerTick)
            }
            AgentEvent::ReasoningStart { received_at } => {
                self.flush(on_event)?;
                self.append("reasoning_start", None, None, None, None, None)?;
                on_event(AgentEvent::ReasoningStart { received_at })
            }
            AgentEvent::ReasoningReset { received_at } => {
                self.flush(on_event)?;
                self.append("reasoning_reset", None, None, None, None, None)?;
                on_event(AgentEvent::ReasoningReset { received_at })
            }
            AgentEvent::ReasoningPartStart { received_at } => {
                self.flush(on_event)?;
                self.append("reasoning_part_start", None, None, None, None, None)?;
                on_event(AgentEvent::ReasoningPartStart { received_at })
            }
            AgentEvent::ReasoningPartEnd { received_at } => {
                self.flush(on_event)?;
                self.append("reasoning_part_end", None, None, None, None, None)?;
                on_event(AgentEvent::ReasoningPartEnd { received_at })
            }
            AgentEvent::ReasoningTitle(title) => {
                self.flush(on_event)?;
                self.append("reasoning_title", None, None, Some(&title), None, None)?;
                on_event(AgentEvent::ReasoningTitle(title))
            }
            AgentEvent::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                self.flush(on_event)?;
                self.append(
                    "tool_call",
                    Some(&call_id),
                    Some(&name),
                    Some(&arguments),
                    None,
                    None,
                )?;
                on_event(AgentEvent::ToolCall {
                    call_id,
                    name,
                    arguments,
                })
            }
            AgentEvent::ToolPreparing { name } => {
                self.flush(on_event)?;
                self.append("tool_preparing", None, Some(&name), Some(&name), None, None)?;
                on_event(AgentEvent::ToolPreparing { name })
            }
            AgentEvent::ToolResult {
                call_id,
                name,
                ok,
                output,
            } => {
                self.flush(on_event)?;
                self.append(
                    "tool_result",
                    Some(&call_id),
                    Some(&name),
                    Some(&output),
                    None,
                    Some(ok),
                )?;
                on_event(AgentEvent::ToolResult {
                    call_id,
                    name,
                    ok,
                    output,
                })
            }
            AgentEvent::ToolProgress {
                call_id,
                name,
                message,
            } => {
                self.flush(on_event)?;
                self.append(
                    "tool_progress",
                    Some(&call_id),
                    Some(&name),
                    Some(&message),
                    None,
                    None,
                )?;
                on_event(AgentEvent::ToolProgress {
                    call_id,
                    name,
                    message,
                })
            }
            AgentEvent::CommandOutput {
                call_id,
                name,
                stream,
                chunk,
            } => {
                self.flush(on_event)?;
                let kind = match stream {
                    tools::CommandOutputStream::Stdout => "command_stdout",
                    tools::CommandOutputStream::Stderr => "command_stderr",
                };
                self.append(kind, Some(&call_id), Some(&name), None, Some(&chunk), None)?;
                on_event(AgentEvent::CommandOutput {
                    call_id,
                    name,
                    stream,
                    chunk,
                })
            }
            AgentEvent::Image {
                call_id,
                name,
                path,
                alt,
            } => {
                self.flush(on_event)?;
                let payload = serde_json::json!({
                    "path": path.display().to_string(),
                    "alt": alt,
                });
                let payload = serde_json::to_string(&payload)?;
                self.append(
                    "image",
                    Some(&call_id),
                    Some(&name),
                    Some(&payload),
                    None,
                    None,
                )?;
                on_event(AgentEvent::Image {
                    call_id,
                    name,
                    path,
                    alt,
                })
            }
            AgentEvent::Artifact {
                call_id,
                name,
                path,
                title,
            } => {
                self.flush(on_event)?;
                let payload = serde_json::json!({
                    "path": path.display().to_string(),
                    "title": title,
                });
                let payload = serde_json::to_string(&payload)?;
                self.append(
                    "artifact",
                    Some(&call_id),
                    Some(&name),
                    Some(&payload),
                    None,
                    None,
                )?;
                on_event(AgentEvent::Artifact {
                    call_id,
                    name,
                    path,
                    title,
                })
            }
            AgentEvent::AskQuestion {
                call_id,
                request,
                responder,
            } => {
                self.flush(on_event)?;
                let payload = serde_json::to_string(&request)?;
                self.append(
                    "question",
                    Some(&call_id),
                    Some("ask_question"),
                    Some(&payload),
                    None,
                    None,
                )?;
                on_event(AgentEvent::AskQuestion {
                    call_id,
                    request,
                    responder,
                })
            }
            AgentEvent::GenerationSuperseded { prompt_ids } => {
                self.flush(on_event)?;
                self.state.supersede_turn_journal_segment(
                    &self.turn_id,
                    self.revision,
                    self.segment_index,
                )?;
                on_event(AgentEvent::GenerationSuperseded { prompt_ids })
            }
            AgentEvent::QueuedPromptsConsumed {
                prompt_ids,
                mode,
                provider_id,
                model,
            } => {
                self.flush(on_event)?;
                self.segment_index = self.segment_index.saturating_add(1);
                on_event(AgentEvent::QueuedPromptsConsumed {
                    prompt_ids,
                    mode,
                    provider_id,
                    model,
                })
            }
            AgentEvent::CompactStart
            | AgentEvent::CompactChunk(_)
            | AgentEvent::CompactEnd
            | AgentEvent::PopStart
            | AgentEvent::PopEnd
            | AgentEvent::Notice { .. }
            | AgentEvent::TurnStarted { .. }
            | AgentEvent::PrepareForExternalOutput { .. } => on_event(event),
            AgentEvent::Chunk(chunk) => on_event(AgentEvent::Chunk(chunk)),
        }
    }

    fn push_chunk<F>(&mut self, chunk: ChatStreamChunk, on_event: &mut F) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        if self.pending.is_none() && !self.pending_reasoning_display.is_empty() {
            self.flush(on_event)?;
        }
        let should_flush = self.pending.as_ref().is_some_and(|pending| {
            pending.kind != chunk.kind
                || pending.text.len().saturating_add(chunk.text.len()) >= JOURNAL_FLUSH_BYTES
                || self.last_flush.elapsed() >= JOURNAL_FLUSH_INTERVAL
        });
        if should_flush {
            self.flush(on_event)?;
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.text.push_str(&chunk.text);
        } else {
            self.pending = Some(PendingJournalChunk {
                kind: chunk.kind,
                text: chunk.text,
            });
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.text.len() >= JOURNAL_FLUSH_BYTES)
        {
            self.flush(on_event)?;
        }
        Ok(())
    }

    fn flush<F>(&mut self, on_event: &mut F) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let Some(pending) = self.pending.take() else {
            if self.pending_reasoning_display.is_empty() {
                return Ok(());
            }
            let text = std::mem::take(&mut self.pending_reasoning_display);
            on_event(AgentEvent::Chunk(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text,
            }))?;
            self.last_flush = Instant::now();
            return Ok(());
        };
        let kind = match pending.kind {
            ChatStreamKind::Content => "assistant_content",
            ChatStreamKind::Reasoning => "assistant_reasoning",
            ChatStreamKind::ToolCall => "tool_call_delta",
            ChatStreamKind::ReasoningReset
            | ChatStreamKind::ReasoningPartStart
            | ChatStreamKind::ReasoningPartEnd => return Ok(()),
        };
        self.append(kind, None, None, Some(&pending.text), None, None)?;
        self.last_flush = Instant::now();
        if pending.kind == ChatStreamKind::Reasoning {
            let text = std::mem::take(&mut self.pending_reasoning_display);
            if text.is_empty() {
                return Ok(());
            }
            return on_event(AgentEvent::Chunk(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text,
            }));
        }
        on_event(AgentEvent::Chunk(ChatStreamChunk {
            kind: pending.kind,
            text: pending.text,
        }))
    }

    fn finish<F>(&mut self, on_event: &mut F) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.flush(on_event)
    }

    fn append(
        &self,
        kind: &str,
        call_id: Option<&str>,
        name: Option<&str>,
        text_payload: Option<&str>,
        blob_payload: Option<&[u8]>,
        ok: Option<bool>,
    ) -> Result<()> {
        self.state.append_turn_journal_event(
            &self.turn_id,
            self.revision,
            self.segment_index,
            kind,
            call_id,
            name,
            text_payload,
            blob_payload,
            ok,
        )
    }
}

fn emit_tool_progress<F>(
    on_event: &mut F,
    call_id: &str,
    name: &str,
    progress: tools::ToolProgressEvent,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    match progress {
        tools::ToolProgressEvent::Message(message) => on_event(AgentEvent::ToolProgress {
            call_id: call_id.to_string(),
            name: name.to_string(),
            message,
        }),
        tools::ToolProgressEvent::PrepareForExternalOutput { ready } => {
            on_event(AgentEvent::PrepareForExternalOutput { ready })
        }
        tools::ToolProgressEvent::Image { path, alt } => on_event(AgentEvent::Image {
            call_id: call_id.to_string(),
            name: name.to_string(),
            path,
            alt,
        }),
        tools::ToolProgressEvent::Artifact { path, title } => on_event(AgentEvent::Artifact {
            call_id: call_id.to_string(),
            name: name.to_string(),
            path,
            title,
        }),
        tools::ToolProgressEvent::CommandOutput { stream, chunk } => {
            on_event(AgentEvent::CommandOutput {
                call_id: call_id.to_string(),
                name: name.to_string(),
                stream,
                chunk,
            })
        }
    }
}

pub struct Agent {
    state: StateStore,
    client: OpenAiCompatibleClient,
    system_prompt: String,
    /// Per-run system additions supplied by a transport/plugin. They are
    /// intentionally excluded from prompt-change hashing and persistence.
    runtime_system_context: Vec<String>,
    /// Per-message transport context (sender identity JSON, message ids, …)
    /// rendered as a tail system message after the user turn. Kept out of the
    /// system prompt so the stable prefix stays byte-identical across turns.
    turn_system_context: Vec<String>,
    /// Raw user input snapshot taken before platform plugins wrapped the turn
    /// content (instruction boilerplate, group history, …). The memory diary
    /// records this instead of the wrapped prompt — the minimal C10 "记忆只读
    /// raw_content" separation. `None` on paths whose input is already raw
    /// (terminal, WebUI) and on redo replays.
    memory_content: Option<String>,
    suppress_session_history: bool,
    trim_at_ratio: f32,
    trim_batch_ratio: f32,
    tools_enabled: bool,
    max_tool_rounds: usize,
    tools: Arc<Mutex<ToolRegistry>>,
    memory: MemoryStore,
    memory_organizer: Option<MemoryOrganizerHandle>,
    memory_origin: MemoryOrigin,
    memory_database_id: String,
    memory_generation: i64,
    mode: AgentMode,
    prompt_audience: PromptAudience,
    config: AppConfig,
    paths: MiyuPaths,
    on_overflow: String,
    turn_display_content: Option<String>,
    attachment_run_id: Option<String>,
    image_platform: Option<String>,
    image_platform_label: Option<String>,
    platform_context: Option<Arc<PlatformTurnContext>>,
    context_images: Vec<PlatformContextImageRef>,
    /// Exact (messages, tools) of the most recent live request; feeds the
    /// idle cache-keepalive pings (v7 DeepSeek 高命中策略). Only populated
    /// while `cache.keepalive_seconds > 0`.
    last_request_snapshot: Option<(Vec<ChatMessage>, Vec<crate::llm::ToolDefinition>)>,
    /// Cancels the currently running keepalive loop, if any.
    keepalive_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Consecutive auto-compactions that failed to bring the context back
    /// under the trigger. A healthy compaction lands below the trigger; two
    /// in a row mean the verbatim floor alone exceeds it (window too small),
    /// so auto-compaction latches off until the context drops (`compact_stuck`).
    consecutive_compacts: std::sync::atomic::AtomicU32,
    compact_stuck: std::sync::atomic::AtomicBool,
    /// Max turn seq observed right after the previous auto-compaction (-1 =
    /// none yet). A new compaction firing within a few turns of the last one
    /// means some single item (a huge paste or tool output) refills the
    /// window instantly — compacting harder won't help ("thrashing").
    last_compact_max_seq: std::sync::atomic::AtomicI64,
    rapid_compacts: std::sync::atomic::AtomicU32,
    /// One-shot "context is getting large" notice at the soft watermark.
    soft_notice_sent: std::sync::atomic::AtomicBool,
}

struct PreparedUserInput {
    content: String,
    message: ChatMessage,
    hints: Vec<ChatMessage>,
}

/// Output of a `task` call executed in the parallel group.
struct GroupTaskOutput {
    output: String,
    /// Persistable tool report, extracted at completion.
    report: Option<String>,
}

impl Agent {
    pub fn new(
        config: AppConfig,
        paths: &MiyuPaths,
        state: StateStore,
        client: OpenAiCompatibleClient,
        tools: ToolRegistry,
        mode: AgentMode,
    ) -> Result<Self> {
        Self::new_for_audience(
            config,
            paths,
            state,
            client,
            tools,
            mode,
            PromptAudience::Owner,
        )
    }

    pub(crate) fn new_for_audience(
        config: AppConfig,
        paths: &MiyuPaths,
        state: StateStore,
        client: OpenAiCompatibleClient,
        tools: ToolRegistry,
        mode: AgentMode,
        prompt_audience: PromptAudience,
    ) -> Result<Self> {
        // Construction is side-effect free (aside from idempotent memory
        // init) so concurrent turns can each build their own Agent; startup
        // maintenance (prompt-change reset, stale-turn recovery) lives in
        // `prepare_for_turn`.
        let base_system_prompt = config.system_prompt_for(paths, prompt_audience)?;
        let system_prompt = with_host_environment(
            with_mode_reminder(base_system_prompt, mode),
            prompt_audience,
            paths,
        );
        let tools_enabled = config.tools.enabled;
        let max_tool_rounds = config.tools.max_rounds;
        let memory = MemoryStore::new(&config, paths);
        memory.init()?;
        let (memory_database_id, memory_generation) = memory.identity()?;
        let memory_origin = MemoryOrigin::local(state.session_id().to_string());
        let on_overflow = config.context.on_overflow.clone();
        Ok(Self {
            state,
            client,
            system_prompt,
            runtime_system_context: Vec::new(),
            turn_system_context: Vec::new(),
            memory_content: None,
            suppress_session_history: false,
            trim_at_ratio: config.context.trim_at_ratio,
            trim_batch_ratio: config.context.trim_batch_ratio,
            tools_enabled,
            max_tool_rounds,
            tools: Arc::new(Mutex::new(tools)),
            memory,
            memory_organizer: None,
            memory_origin,
            memory_database_id,
            memory_generation,
            mode,
            prompt_audience,
            config,
            paths: paths.clone(),
            on_overflow,
            turn_display_content: None,
            attachment_run_id: None,
            image_platform: None,
            image_platform_label: None,
            platform_context: None,
            context_images: Vec::new(),
            last_request_snapshot: None,
            keepalive_cancel: None,
            consecutive_compacts: std::sync::atomic::AtomicU32::new(0),
            compact_stuck: std::sync::atomic::AtomicBool::new(false),
            last_compact_max_seq: std::sync::atomic::AtomicI64::new(-1),
            rapid_compacts: std::sync::atomic::AtomicU32::new(0),
            soft_notice_sent: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Stops the idle cache-keepalive loop (called whenever a new request is
    /// about to change the context, and before dropping the agent).
    pub fn cancel_cache_keepalive(&mut self) {
        if let Some(cancel) = self.keepalive_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// Starts the idle keepalive loop for the last request prefix. No-op when
    /// disabled or when no snapshot exists.
    fn start_cache_keepalive(&mut self) {
        self.cancel_cache_keepalive();
        let interval = self.config.cache.keepalive_seconds;
        if interval == 0 {
            return;
        }
        let Some((messages, tools)) = self.last_request_snapshot.clone() else {
            return;
        };
        let max_pings = self.config.cache.keepalive_max_pings;
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.keepalive_cancel = Some(cancel.clone());
        let client = self.client.clone();
        let state = self.state.clone();
        let usage_source = self.usage_source().to_string();
        tokio::spawn(async move {
            for ping in 0..max_pings {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                if cancel.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                match client.cache_keepalive(messages.clone(), tools.clone()).await {
                    Ok(Some(usage)) => {
                        tracing::info!(
                            ping = ping + 1,
                            prompt_tokens = usage.prompt_tokens,
                            cache_read = usage.cache_read_tokens,
                            "cache keepalive ping"
                        );
                        let meta = crate::state::UsageMeta {
                            source: &usage_source,
                            provider: Some(client.provider_id()),
                            model: None,
                        };
                        let _ = state.add_auxiliary_usage(&usage, meta);
                    }
                    Ok(None) => return, // protocol without keepalive support
                    Err(error) => {
                        tracing::warn!(error = %error, "cache keepalive ping failed");
                        return;
                    }
                }
            }
        });
    }

    /// 用量历史的来源标签:平台回合记平台 id(如 "qq"),其余一律 "agent"。
    fn usage_source(&self) -> &str {
        self.platform_context
            .as_ref()
            .map(|context| context.conversation.platform.as_str())
            .unwrap_or("agent")
    }

    pub fn prepare_for_turn(&mut self) -> Result<()> {
        let effective_system_prompt = self
            .config
            .system_prompt_for(&self.paths, self.prompt_audience)?;
        if matches!(self.mode, AgentMode::Normal | AgentMode::Chat) {
            let fingerprint_prompt = self.config.base_system_prompt(&self.paths)?;
            let compatible_previous = matches!(self.prompt_audience, PromptAudience::Owner)
                .then_some(effective_system_prompt.as_str());
            self.state.reset_if_prompt_changed_with_compatible(
                &fingerprint_prompt,
                compatible_previous,
            )?;
            self.state.recover_stale_turns()?;
            self.maybe_cold_resume_prune()?;
        }
        self.system_prompt = with_host_environment(
            with_runtime_system_context(
                with_mode_reminder(effective_system_prompt, self.mode),
                &self.runtime_system_context,
            ),
            self.prompt_audience,
            &self.paths,
        );
        Ok(())
    }

    pub fn set_runtime_system_context(&mut self, context: Vec<String>) -> Result<()> {
        self.runtime_system_context = context
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
        self.refresh_system_prompt()
    }

    /// Per-message transport blocks that ride the turn tail (after the user
    /// message) instead of the system prompt. No prompt refresh needed: they
    /// are consumed at message-assembly time.
    /// Raw input for the memory diary; `None` falls back to the turn content.
    pub fn set_memory_content(&mut self, content: Option<String>) {
        self.memory_content = content.filter(|text| !text.trim().is_empty());
    }

    pub fn set_turn_system_context(&mut self, context: Vec<String>) {
        self.turn_system_context = context
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
    }

    pub(crate) fn set_memory_writes_enabled(&mut self, enabled: bool) {
        self.memory.set_writes_enabled(enabled);
    }

    pub(crate) fn set_memory_organizer(&mut self, organizer: MemoryOrganizerHandle) {
        self.memory_organizer = Some(organizer);
    }

    pub(crate) fn set_memory_origin(&mut self, origin: MemoryOrigin) {
        self.memory_origin = origin;
    }

    pub(crate) fn set_memory_request_context(
        &mut self,
        access: MemoryAccess,
        writer_principal: Option<String>,
        writer_display_name: impl Into<String>,
    ) {
        self.memory
            .set_request_context(access, writer_principal, writer_display_name);
    }

    pub(crate) fn set_image_platform(&mut self, platform: &str, display_name: &str) {
        let platform = platform
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
            .collect::<String>();
        self.image_platform = (!platform.is_empty()).then_some(platform);
        self.image_platform_label = self.image_platform.as_ref().and_then(|_| {
            (!display_name.trim().is_empty()).then(|| display_name.trim().to_string())
        });
    }

    pub(crate) fn set_platform_context_images(
        &mut self,
        context: Arc<PlatformTurnContext>,
        images: Vec<PlatformContextImageRef>,
    ) {
        self.platform_context = Some(context);
        self.context_images = images;
    }

    pub fn set_turn_persistence(
        &mut self,
        display_content: String,
        attachment_run_id: Option<String>,
    ) {
        self.turn_display_content = Some(display_content);
        self.attachment_run_id = attachment_run_id;
    }

    pub fn set_session_history_suppressed(&mut self, suppressed: bool) {
        self.suppress_session_history = suppressed;
    }

    /// Runs a batch's `task` tool calls concurrently, in waves bounded by
    /// `tools.subagent_concurrency`. Subagents are independent by design, so
    /// fanning them out preserves semantics while collapsing wall-clock time.
    /// Batches with fewer than two task calls — or a not-yet-loaded task tool
    /// (hybrid lazy loading) — return an empty map and take the serial path.
    async fn execute_parallel_task_calls<F>(
        &self,
        calls: &[crate::llm::ToolCall],
        loaded_tools: &std::collections::BTreeSet<String>,
        on_event: &mut F,
    ) -> Result<std::collections::HashMap<usize, GroupTaskOutput>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let mut outputs = std::collections::HashMap::new();
        let eligible: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(_, call)| call.function.name == "task")
            .map(|(index, _)| index)
            .collect();
        if eligible.len() < 2 {
            return Ok(outputs);
        }
        {
            let tools = self.tools.lock().unwrap();
            if tools::is_hybrid_loading_mode(&self.config.tools.loading_mode)
                && tools.requires_lazy_load("task", loaded_tools)
            {
                return Ok(outputs);
            }
        }

        struct Slot {
            call_index: usize,
            call_id: String,
            event_name: String,
            future: Option<tools::ToolFuture>,
            progress: mpsc::UnboundedReceiver<tools::ToolProgressEvent>,
        }
        enum WaveEvent {
            Done(usize, Result<String>),
            Progress(usize, tools::ToolProgressEvent),
            Spinner,
        }

        let limit = self.config.tools.subagent_concurrency.max(1);
        for wave in eligible.chunks(limit) {
            let mut slots: Vec<Slot> = Vec::new();
            {
                let tools = self.tools.lock().unwrap();
                for &call_index in wave {
                    let call = &calls[call_index];
                    let event_name = tool_event_name(&call.function.name, &call.function.arguments);
                    on_event(AgentEvent::ToolCall {
                        call_id: call.id.clone(),
                        name: event_name.clone(),
                        arguments: call.function.arguments.clone(),
                    })?;
                    let (progress_tx, progress_rx) = mpsc::unbounded_channel();
                    match tools.call_with_progress_future(
                        &call.function.name,
                        &call.function.arguments,
                        progress_tx,
                    ) {
                        Ok(future) => slots.push(Slot {
                            call_index,
                            call_id: call.id.clone(),
                            event_name,
                            future: Some(future),
                            progress: progress_rx,
                        }),
                        Err(err) => {
                            let output = format!("tool error: {err}");
                            on_event(AgentEvent::ToolResult {
                                call_id: call.id.clone(),
                                name: event_name,
                                ok: false,
                                output: output.clone(),
                            })?;
                            outputs.insert(
                                call_index,
                                GroupTaskOutput {
                                    output,
                                    report: None,
                                },
                            );
                        }
                    }
                }
            }
            let mut remaining = slots.iter().filter(|slot| slot.future.is_some()).count();
            let mut spinner_interval = tokio::time::interval(SPINNER_INTERVAL);
            spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            spinner_interval.tick().await;
            while remaining > 0 {
                let event = {
                    let poll_slots = std::future::poll_fn(|context| {
                        for (position, slot) in slots.iter_mut().enumerate() {
                            if let std::task::Poll::Ready(Some(progress)) =
                                slot.progress.poll_recv(context)
                            {
                                return std::task::Poll::Ready(WaveEvent::Progress(
                                    position, progress,
                                ));
                            }
                            if let Some(future) = slot.future.as_mut() {
                                if let std::task::Poll::Ready(result) =
                                    future.as_mut().poll(context)
                                {
                                    slot.future = None;
                                    return std::task::Poll::Ready(WaveEvent::Done(
                                        position, result,
                                    ));
                                }
                            }
                        }
                        std::task::Poll::Pending
                    });
                    tokio::select! {
                        event = poll_slots => event,
                        _ = spinner_interval.tick() => WaveEvent::Spinner,
                    }
                };
                match event {
                    WaveEvent::Spinner => on_event(AgentEvent::SpinnerTick)?,
                    WaveEvent::Progress(position, progress) => {
                        emit_tool_progress(
                            on_event,
                            &slots[position].call_id,
                            &slots[position].event_name,
                            progress,
                        )?;
                    }
                    WaveEvent::Done(position, result) => {
                        remaining -= 1;
                        while let Ok(progress) = slots[position].progress.try_recv() {
                            emit_tool_progress(
                                on_event,
                                &slots[position].call_id,
                                &slots[position].event_name,
                                progress,
                            )?;
                        }
                        let call_index = slots[position].call_index;
                        let call_id = slots[position].call_id.clone();
                        let event_name = slots[position].event_name.clone();
                        match result {
                            Ok(output) => {
                                on_event(AgentEvent::ToolResult {
                                    call_id,
                                    name: event_name,
                                    ok: true,
                                    output: output.clone(),
                                })?;
                                let report = extract_persistable_tool_report("task", &output);
                                outputs.insert(call_index, GroupTaskOutput { output, report });
                            }
                            Err(err) => {
                                let output = format!("tool error: {err}");
                                on_event(AgentEvent::ToolResult {
                                    call_id,
                                    name: event_name,
                                    ok: false,
                                    output: output.clone(),
                                })?;
                                outputs.insert(
                                    call_index,
                                    GroupTaskOutput {
                                        output,
                                        report: None,
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(outputs)
    }

    /// Rebuilds the system prompt for the current mode without running
    /// turn-entry maintenance. Used for mid-turn mode switches, where
    /// `reset_if_prompt_changed` must never fire (it would wipe the very
    /// turn that is running).
    fn refresh_system_prompt(&mut self) -> Result<()> {
        let base_system_prompt = self
            .config
            .system_prompt_for(&self.paths, self.prompt_audience)?;
        self.system_prompt = with_host_environment(
            with_runtime_system_context(
                with_mode_reminder(base_system_prompt, self.mode),
                &self.runtime_system_context,
            ),
            self.prompt_audience,
            &self.paths,
        );
        Ok(())
    }

    pub fn mode(&self) -> AgentMode {
        self.mode
    }

    pub fn context_window(&self) -> Option<usize> {
        self.client.context_window(&self.config).ok().flatten()
    }

    pub fn effective_context_tokens(&self) -> Result<u64> {
        let messages = self.chat_messages("", "")?;
        let mut tokens = overflow::estimate_messages_tokens(&messages) as u64;
        if self.tools_enabled {
            let loaded_tools = self.initial_loaded_tools(&messages)?;
            tokens = tokens.saturating_add(self.tool_definition_tokens(&loaded_tools) as u64);
        }
        Ok(tokens)
    }

    /// Session-scoped lifetime token total (Σ in the footer): keeps growing
    /// across compactions, resets to zero with the session history. The old
    /// global usage.json figure lives on in /usage as the global overview.
    pub fn conversation_usage_tokens(&self) -> Result<u64> {
        self.state.session_cumulative_tokens()
    }

    /// Same Σ with the prompt and cache-read halves its cache rate needs.
    pub fn conversation_usage_token_totals(&self) -> Result<TurnTokens> {
        self.state.session_cumulative_token_totals()
    }

    fn tool_definition_tokens(&self, loaded_tools: &BTreeSet<String>) -> usize {
        let tools = self.tools.lock().unwrap();
        let definitions = if tools::is_stub_loading_mode(&self.config.tools.loading_mode) {
            tools.stub_definitions()
        } else if tools::is_hybrid_loading_mode(&self.config.tools.loading_mode) {
            tools.lazy_definitions(loaded_tools)
        } else {
            tools.definitions()
        };
        estimate_tool_definition_tokens(&definitions)
    }

    async fn consume_queued_prompts<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        queued: Vec<QueuedPrompt>,
        preceding_assistant: (Option<&str>, Option<&str>, Option<&str>, Option<&str>),
        checkpoint: TurnRedoCheckpointPayload,
        control: &AgentTurnControl,
        on_event: &mut F,
    ) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        on_event(AgentEvent::FlushJournal)?;
        let mut prepared = Vec::with_capacity(queued.len());
        for prompt in queued {
            let images = self.queued_prompt_images(&prompt)?;
            let input = self.prepare_user_input(&prompt.content, &images).await?;
            prepared.push((prompt, input));
        }

        let mode = control.mode();
        if self.mode != mode {
            self.switch_mode(mode, control.tools(mode));
            self.refresh_system_prompt()?;
        }
        replace_request_mode_context(
            messages,
            &self.system_prompt,
            mode,
            self.platform_context.is_some(),
        );

        let consumed = prepared
            .iter()
            .map(|(prompt, input)| (prompt.prompt_id.clone(), input.content.clone()))
            .collect::<Vec<_>>();
        self.state.consume_queued_prompts_with_checkpoint(
            current_turn_id,
            &consumed,
            preceding_assistant
                .0
                .filter(|content| !content.trim().is_empty()),
            preceding_assistant
                .1
                .filter(|reasoning| !reasoning.trim().is_empty()),
            preceding_assistant
                .2
                .filter(|provider_id| !provider_id.trim().is_empty()),
            preceding_assistant
                .3
                .filter(|model| !model.trim().is_empty()),
            checkpoint,
        )?;
        on_event(AgentEvent::QueuedPromptsConsumed {
            prompt_ids: consumed.iter().map(|(id, _)| id.clone()).collect(),
            mode,
            provider_id: preceding_assistant.2.map(str::to_string),
            model: preceding_assistant.3.map(str::to_string),
        })?;

        for (_, input) in prepared {
            messages.push(input.message);
            messages.extend(input.hints);
        }
        Ok(())
    }

    fn trim_visible_context(&self) -> Result<Vec<crate::state::StoredConversationEntry>> {
        let Some(context_window) = self.context_window() else {
            return Ok(Vec::new());
        };
        let track_loaded_tool_sources = self.tools_enabled
            && self.config.tools.persist_loaded_tools
            && tools::is_hybrid_loading_mode(&self.config.tools.loading_mode);
        if track_loaded_tool_sources {
            self.effective_context_tokens()?;
        }
        let mut loaded_tool_sources = if track_loaded_tool_sources {
            Some(self.state.load_session_loaded_tools_with_sources()?)
        } else {
            None
        };
        let expected_loaded_tools = loaded_tool_sources.clone();
        let mut total = usize::try_from(self.effective_context_tokens()?).unwrap_or(usize::MAX);
        let trigger = (context_window as f32 * self.trim_at_ratio).max(1.0) as usize;
        if total < trigger {
            return Ok(Vec::new());
        }

        let target = (context_window as f32 * (1.0 - self.trim_batch_ratio)).max(1.0) as usize;
        let turns = self.state.load_visible_turns()?;
        let mut loaded_tool_tokens = loaded_tool_sources
            .as_ref()
            .map(|items| {
                self.tool_definition_tokens(
                    &items
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<BTreeSet<_>>(),
                )
            })
            .unwrap_or(0);
        let mut count = 0usize;
        for turn in turns
            .iter()
            .filter(|turn| !turn.is_summary && turn.status != crate::state::TurnStatus::Running)
        {
            if total <= target {
                break;
            }
            let turn_tokens = if turn.status == crate::state::TurnStatus::Interrupted
                && !turn.journal_events.is_empty()
            {
                let mut replay = vec![self.turn_user_message(turn)];
                replay.extend(interrupted_turn_replay_messages(self, turn));
                overflow::estimate_messages_tokens(&replay)
            } else {
                turn_context_tokens(turn)
            };
            total = total.saturating_sub(turn_tokens);
            if let Some(items) = loaded_tool_sources.as_mut() {
                items.retain(|(_, source)| source.as_deref() != Some(turn.turn_id.as_str()));
                let remaining = items
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<BTreeSet<_>>();
                let remaining_tokens = self.tool_definition_tokens(&remaining);
                if remaining_tokens <= loaded_tool_tokens {
                    total = total.saturating_sub(loaded_tool_tokens - remaining_tokens);
                } else {
                    total = total.saturating_add(remaining_tokens - loaded_tool_tokens);
                }
                loaded_tool_tokens = remaining_tokens;
            }
            count += 1;
        }
        let turns = self.state.oldest_evictable_visible_turns(count)?;
        archive_and_delete_visible_turns_checked(
            &self.state,
            &self.memory,
            &turns,
            expected_loaded_tools.as_deref(),
        )
    }

    pub fn switch_mode(&mut self, mode: AgentMode, tools: ToolRegistry) {
        self.mode = mode;
        self.tools = Arc::new(Mutex::new(tools));
    }

    pub fn replace_client(&mut self, client: OpenAiCompatibleClient) {
        self.client = client;
    }

    pub(crate) fn cloned_client(&self) -> OpenAiCompatibleClient {
        self.client.clone()
    }

    pub fn reload_config(
        &mut self,
        config: AppConfig,
        client: OpenAiCompatibleClient,
    ) -> Result<()> {
        self.config = config;
        self.client = client;
        self.tools_enabled = self.config.tools.enabled;
        self.max_tool_rounds = self.config.tools.max_rounds;
        self.trim_at_ratio = self.config.context.trim_at_ratio;
        self.trim_batch_ratio = self.config.context.trim_batch_ratio;
        self.on_overflow = self.config.context.on_overflow.clone();
        let (access, writer_principal, writer_display_name) = self.memory.request_context();
        self.memory = MemoryStore::new(&self.config, &self.paths).with_request_context(
            access,
            writer_principal,
            writer_display_name,
        );
        self.memory.init()?;
        (self.memory_database_id, self.memory_generation) = self.memory.identity()?;
        self.prepare_for_turn()
    }

    pub fn reset_memory(&mut self) -> Result<()> {
        let (access, writer_principal, writer_display_name) = self.memory.request_context();
        self.memory = MemoryStore::new(&self.config, &self.paths).with_request_context(
            access,
            writer_principal,
            writer_display_name,
        );
        self.memory.init()?;
        (self.memory_database_id, self.memory_generation) = self.memory.identity()?;
        Ok(())
    }

    pub async fn chat_stream<F>(&mut self, input: &str, on_event: F) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.chat_stream_with_images(input, &[], on_event).await
    }

    pub async fn chat_stream_with_images<F>(
        &mut self,
        input: &str,
        images: &[Option<PastedImage>],
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.chat_stream_with_images_inner(input, images, None, on_event)
            .await
    }

    pub async fn chat_stream_with_control<F>(
        &mut self,
        input: &str,
        images: &[Option<PastedImage>],
        control: &AgentTurnControl,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.chat_stream_with_images_inner(input, images, Some(control), on_event)
            .await
    }

    pub async fn redo_stream_with_control<F>(
        &mut self,
        candidate: &RedoCandidate,
        prompts: Vec<RedoPromptInput>,
        control: &AgentTurnControl,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let session = self.state.session_id();
        crate::tools::workspace::with_session(
            session,
            self.redo_stream_turn(candidate, prompts, control, on_event),
        )
        .await
    }

    async fn redo_stream_turn<F>(
        &mut self,
        candidate: &RedoCandidate,
        prompts: Vec<RedoPromptInput>,
        control: &AgentTurnControl,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.cancel_cache_keepalive();
        self.state.recover_stale_turns()?;
        self.trim_visible_context()?;
        if prompts.is_empty()
            || prompts.last().map(|prompt| prompt.prompt_id.as_str())
                != Some(candidate.input_id.as_str())
        {
            bail!("redo prompts no longer match the selected input");
        }
        let current_turn = self
            .state
            .load_turns()?
            .into_iter()
            .find(|turn| turn.turn_id == candidate.turn_id)
            .context("redo turn no longer exists")?;

        let mut prepared = Vec::with_capacity(prompts.len());
        for prompt in prompts {
            let input = self
                .prepare_user_input(&prompt.content, &prompt.images)
                .await?;
            prepared.push((prompt, input));
        }
        let (last_prompt, last_input) = prepared.last().context("redo input is empty")?;
        let last_content = last_input.content.clone();
        let last_display_content = last_prompt.display_content.clone();
        let diary_input = last_content.clone();
        let redo = self.state.begin_redo(
            &candidate.turn_id,
            &candidate.input_id,
            candidate.input_kind,
            candidate.revision,
            &last_content,
            &last_display_content,
            std::process::id(),
        )?;
        let guard =
            PendingRedoGuard::new(self.state.clone(), candidate.turn_id.clone(), redo.revision);
        let mut on_event = on_event;
        on_event(AgentEvent::TurnStarted {
            turn_id: candidate.turn_id.clone(),
        })?;

        let mut messages = self.chat_messages(&candidate.turn_id, "")?;
        // chat_messages ends with [.., user placeholder, runtime]; drop both
        // and re-append the runtime right after the real redo input so the
        // transient tail keeps sitting behind the user message.
        let runtime_message = messages.pop();
        let _ = messages.pop();
        let replay_start;
        let fossil_start;
        let base_tool_reports;
        let initial_tool_rounds;
        let initial_question_rounds;
        match candidate.input_kind {
            RedoInputKind::Initial => {
                let (_, input) = prepared.pop().context("redo input is empty")?;
                messages.push(input.message);
                fossil_start = messages.len();
                messages.extend(runtime_message);
                replay_start = messages.len();
                messages.extend(input.hints);
                base_tool_reports = Vec::new();
                initial_tool_rounds = 0;
                initial_question_rounds = 0;
            }
            RedoInputKind::Followup => {
                let checkpoint = redo.checkpoint.context("redo checkpoint is unavailable")?;
                messages.push(self.turn_user_message(&current_turn));
                fossil_start = messages.len();
                messages.extend(runtime_message);
                replay_start = messages.len();
                messages.extend(checkpoint.replay_messages);
                for (_, input) in prepared {
                    messages.push(input.message);
                    messages.extend(input.hints);
                }
                base_tool_reports = checkpoint.prefix_tool_reports;
                initial_tool_rounds = checkpoint.tool_rounds;
                initial_question_rounds = checkpoint.question_rounds;
            }
        }
        // Redo rewrites the turn, so refresh its fossilized tail to match what
        // this generation actually sends (new runtime stamp + replayed tail).
        self.state.set_turn_context_messages(
            &candidate.turn_id,
            &fossil_context_messages(&messages[fossil_start..]),
        )?;

        let mut used_tools = Vec::new();
        let mut persisted_tool_reports = Vec::new();
        let mut journal =
            TurnJournalSink::new(self.state.clone(), candidate.turn_id.clone(), redo.revision);
        let stream_result = {
            let mut journaled_event = |event| journal.emit(event, &mut on_event);
            self.chat_with_tools(
                &candidate.turn_id,
                &mut messages,
                &mut used_tools,
                &mut persisted_tool_reports,
                replay_start,
                &base_tool_reports,
                initial_tool_rounds,
                initial_question_rounds,
                Some(control),
                &mut journaled_event,
            )
            .await
        };
        journal.finish(&mut on_event)?;
        let result = stream_result?;
        let reports = persisted_tool_reports
            .into_iter()
            .map(|(_, report)| report)
            .collect::<Vec<_>>();
        self.state
            .append_persisted_contexts(&candidate.turn_id, &reports)?;
        let tokens = TurnTokens::from_usage(result.usage.as_ref());
        guard.complete_with_model(
            &result.content,
            result.reasoning.as_deref(),
            result.provider_id.as_deref(),
            result.model.as_deref(),
            tokens,
            result.usage_estimated,
        )?;
        if self.memory.process_after_turn(
            &diary_input,
            &result.content,
            &self.memory_origin,
            &self.memory_database_id,
            self.memory_generation,
        )? {
            self.wake_memory_organizer();
        }
        if let Some(usage) = result.usage.clone() {
            let meta = crate::state::UsageMeta {
                source: self.usage_source(),
                provider: result.provider_id.as_deref(),
                model: result.model.as_deref(),
            };
            self.state.add_usage(&usage, meta)?;
        }
        self.start_cache_keepalive();
        Ok(result)
    }

    /// Publishes the turn's session as the ambient scope before running it.
    /// Subagents launched inside read it to hang their audit sessions off this
    /// one, and those audits now count toward the session's Σ — without the
    /// scope a subagent bills to nobody. The daemon actor sets the same scope;
    /// re-scoping to the same id is harmless, and the direct/local paths had
    /// no scope at all.
    async fn chat_stream_with_images_inner<F>(
        &mut self,
        input: &str,
        images: &[Option<PastedImage>],
        control: Option<&AgentTurnControl>,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let session = self.state.session_id();
        crate::tools::workspace::with_session(
            session,
            self.chat_stream_turn(input, images, control, on_event),
        )
        .await
    }

    async fn chat_stream_turn<F>(
        &mut self,
        input: &str,
        images: &[Option<PastedImage>],
        control: Option<&AgentTurnControl>,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        // A new turn is about to mutate the context; stop pinging the stale
        // prefix (the turn's own requests refresh the cache anyway).
        self.cancel_cache_keepalive();
        self.state.recover_stale_turns()?;
        self.trim_visible_context()?;
        let prepared = self.prepare_user_input(input, images).await?;
        let input = prepared.content.clone();
        let turn_id = format!(
            "turn_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            rand::random::<u16>()
        );
        let display_content = self
            .turn_display_content
            .take()
            .unwrap_or_else(|| input.clone());
        let attachment_run_id = self.attachment_run_id.take();
        self.state.start_turn_with_display(
            &turn_id,
            &input,
            &display_content,
            std::process::id(),
            attachment_run_id.as_deref(),
        )?;
        let guard = PendingTurnGuard::new(self.state.clone(), turn_id.clone());
        let mut on_event = on_event;
        on_event(AgentEvent::TurnStarted {
            turn_id: turn_id.clone(),
        })?;
        let mut messages = self.chat_messages(&turn_id, &input)?;
        // chat_messages ends with [.., user, runtime]; swap in the prepared
        // user message (attachments/images) at its position before the
        // transient runtime tail.
        let user_index = messages.len().saturating_sub(2);
        if let Some(user) = messages.get_mut(user_index) {
            *user = prepared.message;
        }
        let replay_start = messages.len();
        if !self.turn_system_context.is_empty() {
            // Trusted transport/control tail (v7 §三): host-derived per-message
            // context lands after the user message, before untrusted blocks.
            // Standing advisories (the `[SystemInfo:` class, e.g. long-reply
            // conversion records) repeat identical text turn after turn; when
            // the exact bytes are already visible in a replayed fossil the
            // repeat adds nothing and is skipped — the associative-memory
            // dedup reasoning. Everything else ("this turn is system
            // triggered", identity warnings, moderation prechecks) refers to
            // the CURRENT turn, so an identical old fossil is no substitute
            // and those blocks are always sent.
            let fresh = self
                .turn_system_context
                .iter()
                .filter(|block| {
                    !(block.starts_with(STANDING_ADVISORY_PREFIX)
                        && turn_context_block_visible(&messages, block))
                })
                .cloned()
                .collect::<Vec<_>>();
            if !fresh.is_empty() {
                messages.push(ChatMessage::turn_context(fresh.join("\n\n")));
            }
        }
        messages.extend(prepared.hints);
        if self.mode != AgentMode::Chat {
            if let Some(mut association) = self.memory.association(&input)? {
                if association.organization_due {
                    self.wake_memory_organizer();
                }
                if self.memory.association_dedup_enabled() {
                    // Cross-turn dedup: fossils replay earlier associative
                    // blocks byte-for-byte, so a line already visible in this
                    // request adds nothing but tokens. Filtering only shrinks
                    // the block being built this turn; once a carrying turn is
                    // hidden by compact or trim, its lines leave the request
                    // and the memory becomes eligible for injection again.
                    let seen = visible_association_lines(&messages);
                    self.memory
                        .retain_unseen_association(&mut association, &seen);
                }
                if !association.facts.is_empty() || !association.episodes.is_empty() {
                    // v7 Phase 1.1: the associative-memory block rides the turn
                    // tail instead of `insert(1)`, so the stable history prefix
                    // in front stays byte-identical for provider prefix caches.
                    // It lands after `replay_start`, so redo checkpoints freeze
                    // the recalled snapshot (decision 6).
                    messages.push(ChatMessage::turn_context(
                        self.memory.format_association(&association),
                    ));
                }
            }
        }
        {
            if let Some(reminder) = memes::auto_meme_reminder(&self.config, &input) {
                messages.push(ChatMessage::turn_context(reminder));
            }
        }
        // v7 append-only fossilization ("注入了就别删"): archive the transient
        // system tail exactly as sent — runtime stamp, trusted transport
        // context, hints, associative memory, meme reminder — so future
        // history replay is a byte-exact extension of this request and the
        // provider prefix cache never sees a divergence at this turn.
        self.state.set_turn_context_messages(
            &turn_id,
            &fossil_context_messages(&messages[user_index + 1..]),
        )?;
        let mut used_tools = Vec::new();
        let mut persisted_tool_reports = Vec::new();
        let mut journal = TurnJournalSink::new(self.state.clone(), turn_id.clone(), 0);
        let stream_result = {
            let mut journaled_event = |event| journal.emit(event, &mut on_event);
            self.chat_with_tools(
                &turn_id,
                &mut messages,
                &mut used_tools,
                &mut persisted_tool_reports,
                replay_start,
                &[],
                0,
                0,
                control,
                &mut journaled_event,
            )
            .await
        };
        journal.finish(&mut on_event)?;
        let result = stream_result?;
        let reports = persisted_tool_reports
            .into_iter()
            .map(|(_, report)| report)
            .collect::<Vec<_>>();
        self.state.append_persisted_contexts(&turn_id, &reports)?;
        let tokens = TurnTokens::from_usage(result.usage.as_ref());
        guard.complete_with_model(
            &result.content,
            result.reasoning.as_deref(),
            result.provider_id.as_deref(),
            result.model.as_deref(),
            tokens,
            result.usage_estimated,
        )?;
        if self.memory.process_after_turn(
            // C10 三份内容分离(最小实现):日记读平台包装前的原文快照,
            // 而不是带指令样板和群聊记录块的完整 prompt 内容。
            self.memory_content.as_deref().unwrap_or(&input),
            &result.content,
            &self.memory_origin,
            &self.memory_database_id,
            self.memory_generation,
        )? {
            self.wake_memory_organizer();
        }
        if let Some(usage) = result.usage.clone() {
            let meta = crate::state::UsageMeta {
                source: self.usage_source(),
                provider: result.provider_id.as_deref(),
                model: result.model.as_deref(),
            };
            self.state.add_usage(&usage, meta)?;
        }
        self.start_cache_keepalive();
        Ok(result)
    }

    fn wake_memory_organizer(&self) {
        if let Some(organizer) = &self.memory_organizer {
            organizer.wake(self.config.clone(), self.paths.clone(), self.state.clone());
        }
    }

    async fn prepare_user_input(
        &self,
        input: &str,
        images: &[Option<PastedImage>],
    ) -> Result<PreparedUserInput> {
        let input = clean_user_visible_text(input);
        let binary_images = images
            .iter()
            .filter_map(|image| match image {
                Some(PastedImage::Binary(image)) => Some(image),
                _ => None,
            })
            .collect::<Vec<_>>();
        let path_images = images
            .iter()
            .filter_map(|image| match image {
                Some(PastedImage::Path(path)) => Some(path.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let absolute_image_paths =
            resolve_pasted_image_paths(images, &self.paths, self.image_platform.as_deref());
        let binary_paths = images
            .iter()
            .zip(&absolute_image_paths)
            .filter_map(|(image, path)| {
                matches!(image, Some(PastedImage::Binary(_)))
                    .then(|| path.clone())
                    .flatten()
            })
            .collect::<Vec<_>>();
        // v7 Phase 1.3-b: register the scoped vision tool whenever the platform
        // path is active, even with no images this turn. A conditional
        // registration made the tools array appear/disappear between turns,
        // invalidating the provider prefix cache from token 0; an empty scope
        // simply rejects analysis requests with a clear message instead.
        if self.tools_enabled
            && self.config.plugins.vision.enabled
            && self.image_platform.is_some()
        {
            let mut tools = self.tools.lock().unwrap();
            if let Some(platform_context) = self.platform_context.clone() {
                vision::register_scoped_platform(
                    &mut tools,
                    self.config.clone(),
                    self.paths.clone(),
                    binary_paths.iter().map(PathBuf::from).collect(),
                    self.context_images.clone(),
                    platform_context,
                );
            } else if !tools.contains("vision_analyze") {
                vision::register_scoped_local(
                    &mut tools,
                    self.config.clone(),
                    self.paths.clone(),
                    binary_paths.iter().map(PathBuf::from).collect(),
                );
            }
        }
        let vision_tool_available =
            self.tools_enabled && self.tools.lock().unwrap().contains("vision_analyze");
        let input = rewrite_image_placeholders_with_paths(&input, &absolute_image_paths);
        let current_model_supports_vision = self.current_model_supports_vision();
        let content = if !binary_images.is_empty() && !current_model_supports_vision {
            self.describe_images_with_vision_provider(&input, &binary_images)
                .await?
        } else {
            input
        };

        let message = if !binary_images.is_empty() && current_model_supports_vision {
            let mut parts = vec![ChatContentPart::Text {
                text: content.clone(),
            }];
            parts.extend(binary_images.iter().map(|image| ChatContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: image.data_url().to_string(),
                },
            }));
            ChatMessage::user_parts(parts)
        } else {
            ChatMessage::plain("user", &content)
        };

        let mut hints = Vec::new();
        if !binary_paths.is_empty() {
            let source = self
                .image_platform_label
                .as_deref()
                .or(self.image_platform.as_deref())
                .map(|platform| format!("通过 {platform} 发送"))
                .unwrap_or_else(|| "粘贴".to_string());
            let tool_hint = if vision_tool_available {
                "\n你可以使用 vision_analyze 工具对此图片进行更详细的分析。"
            } else {
                ""
            };
            let hint = if binary_paths.len() == 1 {
                format!(
                    "用户{source}了 1 张图片，已保存到临时文件：{}{}",
                    binary_paths[0], tool_hint
                )
            } else {
                let list = binary_paths
                    .iter()
                    .enumerate()
                    .map(|(index, path)| format!("  [Image {}] {}", index + 1, path))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "用户{source}了 {} 张图片，已保存到临时文件：\n{}{}",
                    binary_paths.len(),
                    list,
                    if vision_tool_available {
                        "\n你可以使用 vision_analyze 工具对这些图片进行更详细的分析。"
                    } else {
                        ""
                    }
                )
            };
            hints.push(ChatMessage::turn_context(hint));
        }
        if !path_images.is_empty() && vision_tool_available {
            let list = path_images
                .iter()
                .enumerate()
                .map(|(index, path)| format!("  [Image {}] {}", index + 1, path))
                .collect::<Vec<_>>()
                .join("\n");
            hints.push(ChatMessage::turn_context(format!(
                "用户粘贴了 {} 张本地图片路径：\n{}\n你可以使用 vision_analyze 工具读取并分析这些图片。",
                path_images.len(),
                list
            )));
        }
        if !self.context_images.is_empty() && vision_tool_available {
            let ids = self
                .context_images
                .iter()
                .map(|image| image.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            hints.push(ChatMessage::turn_context(format!(
                "此前群聊记录中有可按需查看的历史图片：{ids}。你尚未看到这些图片的实际内容；只有回答确实依赖图片时，才使用 vision_analyze，并把对应 ID 作为 image 参数。不得根据图片占位符猜测内容。"
            )));
        }

        Ok(PreparedUserInput {
            content,
            message,
            hints,
        })
    }

    pub async fn handle_overflow_after_turn<F>(
        &self,
        context_tokens: u64,
        on_event: F,
    ) -> Result<Option<ChatResult>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let mut on_event = on_event;
        let Some(compact) = self.handle_overflow(context_tokens, &mut on_event).await? else {
            return Ok(None);
        };
        self.state.add_auxiliary_usage(
            &compact.usage,
            crate::state::UsageMeta {
                source: self.usage_source(),
                provider: compact.provider_id.as_deref(),
                model: None,
            },
        )?;
        Ok(Some(ChatResult {
            content: String::new(),
            reasoning: None,
            usage: Some(compact.usage),
            usage_estimated: compact.usage_estimated,
            tool_calls: Vec::new(),
            provider_id: None,
            model: None,
            finish_reason: None,
            thinking_signature: None,
            last_request_usage: None,
            responses_continuation: None,
        }))
    }

    pub async fn compact_now<F>(&self, on_event: F) -> Result<Option<ChatResult>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let mut on_event = on_event;
        let context_window = self.context_window().or_else(|| {
            if crate::models_cache::is_loaded() {
                return None;
            }
            crate::models_cache::refresh_blocking(&self.paths).ok()?;
            self.context_window()
        });
        let Some(context_window) = context_window else {
            let missing = self.client.models_without_context_window(&self.config);
            if missing.is_empty() {
                bail!(
                    "{}",
                    crate::i18n::text(
                        "The current model's context window is not loaded or configured, so the context cannot be compacted",
                        "当前模型的上下文窗口尚未加载或未配置，无法压缩上下文"
                    )
                );
            }
            bail!(
                "{}{}",
                crate::i18n::text(
                    "The context windows for these active models are not loaded or configured, so the context cannot be compacted: ",
                    "以下活动模型的上下文窗口尚未加载或未配置，无法压缩上下文："
                ),
                missing.join(", ")
            );
        };
        let visible_count = self.state.load_visible_turns()?.len();
        if visible_count == 0 {
            return Ok(None);
        }
        let check = overflow::OverflowCheck::new(Some(context_window), self.trim_at_ratio, None);
        on_event(AgentEvent::CompactStart)?;
        let compactor = compact::Compactor::new(
            self.client.clone(),
            self.state.clone(),
            context_window,
            check.reserved_tokens,
            self.compact_tail_budget(context_window),
            matches!(self.mode, AgentMode::Chat),
        );
        let mut on_chunk = |chunk: ChatStreamChunk| on_event(AgentEvent::CompactChunk(chunk));
        let fork_builder = |fold_ids: &[String]| -> Result<compact::CompactForkParts> {
            Ok((
                self.compact_fork_prefix(fold_ids)?,
                self.live_tool_definitions()?,
            ))
        };
        let fork_builder: Option<compact::CompactForkBuilder<'_>> = self
            .config
            .context
            .compact_cache_reuse
            .then_some(&fork_builder);
        // Manual /compact is an explicit user request: bypass the
        // fold-economics gate (but tail retention still applies).
        let compact = match compactor
            .perform_compact(true, false, fork_builder, &mut on_chunk)
            .await
        {
            Ok(result) => {
                on_event(AgentEvent::CompactEnd)?;
                result
            }
            Err(err) => {
                on_event(AgentEvent::CompactEnd)?;
                return Err(err);
            }
        };
        let Some(compact) = compact else {
            return Ok(None);
        };
        self.state.add_auxiliary_usage(
            &compact.usage,
            crate::state::UsageMeta {
                source: self.usage_source(),
                provider: compact.provider_id.as_deref(),
                model: None,
            },
        )?;
        Ok(Some(ChatResult {
            content: String::new(),
            reasoning: None,
            usage: Some(compact.usage),
            usage_estimated: compact.usage_estimated,
            tool_calls: Vec::new(),
            provider_id: None,
            model: None,
            finish_reason: None,
            thinking_signature: None,
            last_request_usage: None,
            responses_continuation: None,
        }))
    }

    /// Cold-resume prune: after idling past the provider cache TTL the next
    /// request is a full-price cold start anyway, so a history rewrite right
    /// now is free cache-wise and only shrinks that first request. Uses a
    /// minimal harvest gate for the same reason.
    fn maybe_cold_resume_prune(&self) -> Result<()> {
        if !self.config.context.prune_stale_tool_reports {
            return Ok(());
        }
        let minutes = self.config.context.cold_prune_after_minutes;
        if minutes == 0 {
            return Ok(());
        }
        let Some(last) = self.state.session_last_request_at()? else {
            return Ok(());
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if now.saturating_sub(last) < (minutes as i64).saturating_mul(60) {
            return Ok(());
        }
        let stats = self.state.prune_stale_tool_reports(2, 1024)?;
        if stats.turns > 0 {
            tracing::info!(
                turns = stats.turns,
                saved_chars = stats.saved_chars,
                idle_minutes = now.saturating_sub(last) / 60,
                "context_rewrite reason=cold_resume_prune"
            );
        }
        Ok(())
    }

    /// Mechanical prune behind the harvest gate: rewriting history is a
    /// prefix-cache reset, so the batch must save at least ~window/64 tokens
    /// (~window/16 chars) to pay for it. Protects the newest 2 turns.
    fn prune_stale_history(&self, context_window: usize) -> Result<crate::state::PruneStats> {
        let min_saved_chars = (context_window / 16).max(8192);
        let stats = self.state.prune_stale_tool_reports(2, min_saved_chars)?;
        if stats.turns > 0 {
            tracing::info!(
                turns = stats.turns,
                saved_chars = stats.saved_chars,
                "context_rewrite reason=prune"
            );
        }
        Ok(stats)
    }

    /// Derives the verbatim tail budget for compaction. Fixed token count by
    /// design (the trigger scales with the window, the tail does not — that
    /// geometry is what stops the re-compaction loop); chat sessions default
    /// smaller because casual history has less verbatim value.
    fn compact_tail_budget(&self, context_window: usize) -> usize {
        self.config.context.compact_tail_tokens.unwrap_or({
            if matches!(self.mode, AgentMode::Chat) {
                8192
            } else {
                16384.min(context_window / 4)
            }
        })
    }

    async fn handle_overflow<F>(
        &self,
        context_tokens: u64,
        on_event: &mut F,
    ) -> Result<Option<compact::CompactResult>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        use std::sync::atomic::Ordering;
        let context_window = self.context_window();
        let check = overflow::OverflowCheck::new(context_window, self.trim_at_ratio, None);
        let context_tokens = usize::try_from(context_tokens).unwrap_or(usize::MAX);
        if !check.is_enabled() {
            return Ok(None);
        }
        if !check.check_tokens(context_tokens) {
            // Breathing room below the trigger is what a healthy compaction
            // buys; clear the stuck latch and the run counters here, before
            // any other branch can return, so a compaction that settles the
            // context anywhere under the trigger fully re-arms
            // auto-compaction (a stale count would latch the next one off).
            self.consecutive_compacts.store(0, Ordering::Relaxed);
            self.rapid_compacts.store(0, Ordering::Relaxed);
            self.compact_stuck.store(false, Ordering::Relaxed);
            // Below-trigger watermarks: each tier does only the cheapest
            // thing that helps. snip prunes stale tool reports mechanically
            // (no LLM call); soft just says the context is growing, once.
            if let Some(window) = context_window {
                let snip_threshold = (window as f32
                    * self.config.context.compact_snip_ratio)
                    .max(1.0) as usize;
                let soft_threshold = (window as f32
                    * self.config.context.compact_soft_ratio)
                    .max(1.0) as usize;
                if context_tokens >= snip_threshold {
                    if self.config.context.prune_stale_tool_reports {
                        let stats = self.prune_stale_history(window)?;
                        if stats.turns > 0 {
                            on_event(AgentEvent::Notice {
                                text: format!(
                                    "{} {} · ~{} chars",
                                    crate::i18n::text(
                                        "Folded stale tool records from turns:",
                                        "已机械折叠旧轮次的工具记录："
                                    ),
                                    stats.turns,
                                    stats.saved_chars,
                                ),
                            })?;
                        }
                    }
                } else if context_tokens >= soft_threshold
                    && !self.soft_notice_sent.swap(true, Ordering::Relaxed)
                {
                    on_event(AgentEvent::Notice {
                        text: crate::i18n::text(
                            "Context is getting large; older tool records will fold first, then the history will be compacted automatically.",
                            "上下文渐大；将先机械折叠旧工具记录，随后才会自动压缩历史。",
                        )
                        .to_string(),
                    })?;
                }
            }
            return Ok(None);
        }
        if self.compact_stuck.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let compact_result = match self.on_overflow.as_str() {
            "compact" => {
                let visible_count = self.state.load_visible_turns()?.len();
                if visible_count == 0 {
                    return Ok(None);
                }
                let window = context_window.unwrap();
                let force_threshold = (window as f32
                    * self.config.context.compact_force_ratio)
                    .max(1.0) as usize;
                let force = context_tokens >= force_threshold;
                // Prune first: it is free, and when it alone lands the
                // context back under the trigger the paid summary call (and
                // its cache reset) is skipped entirely.
                if self.config.context.prune_stale_tool_reports {
                    let stats = self.prune_stale_history(window)?;
                    if stats.turns > 0 && !force {
                        let post_tokens = usize::try_from(self.effective_context_tokens()?)
                            .unwrap_or(usize::MAX);
                        if !check.check_tokens(post_tokens) {
                            on_event(AgentEvent::Notice {
                                text: crate::i18n::text(
                                    "Folded stale tool records; context is back under the compaction threshold.",
                                    "已机械折叠旧工具记录；上下文已回落到压缩阈值之下。",
                                )
                                .to_string(),
                            })?;
                            return Ok(None);
                        }
                    }
                }
                on_event(AgentEvent::CompactStart)?;
                let compactor = compact::Compactor::new(
                    self.client.clone(),
                    self.state.clone(),
                    window,
                    check.reserved_tokens,
                    self.compact_tail_budget(window),
                    matches!(self.mode, AgentMode::Chat),
                );
                let mut on_chunk =
                    |chunk: ChatStreamChunk| on_event(AgentEvent::CompactChunk(chunk));
                let fork_builder = |fold_ids: &[String]| -> Result<compact::CompactForkParts> {
                    Ok((
                        self.compact_fork_prefix(fold_ids)?,
                        self.live_tool_definitions()?,
                    ))
                };
                let fork_builder: Option<compact::CompactForkBuilder<'_>> = self
                    .config
                    .context
                    .compact_cache_reuse
                    .then_some(&fork_builder);
                let result = match compactor
                    .perform_compact(force, true, fork_builder, &mut on_chunk)
                    .await
                {
                    Ok(result) => {
                        on_event(AgentEvent::CompactEnd)?;
                        result
                    }
                    Err(e) => {
                        on_event(AgentEvent::CompactEnd)?;
                        return Err(e);
                    }
                };
                if let Some(result) = result.as_ref() {
                    on_event(AgentEvent::Notice {
                        text: format!(
                            "{} {} → {} {}",
                            crate::i18n::text("Compacted: folded turns", "压缩完成：折叠轮次"),
                            result.folded_turns,
                            crate::i18n::text("kept verbatim", "逐字保留最近轮次"),
                            result.kept_turns,
                        ),
                    })?;
                }
                if result.is_some() {
                    // Post-compaction check: still over the trigger means the
                    // verbatim floor plus system prompt alone exceed it.
                    // Twice in a row would re-fire every turn (cratering the
                    // prefix cache each time), so latch auto-compaction off
                    // and say why, once.
                    let post_tokens =
                        usize::try_from(self.effective_context_tokens()?).unwrap_or(usize::MAX);
                    if check.check_tokens(post_tokens) {
                        let runs = self.consecutive_compacts.fetch_add(1, Ordering::Relaxed) + 1;
                        if runs >= 2 && !self.compact_stuck.swap(true, Ordering::Relaxed) {
                            on_event(AgentEvent::Notice {
                                text: crate::i18n::text(
                                    "Automatic context compaction paused: the context window is too small for compaction to help (the system prompt plus the verbatim tail already exceed the trigger). Raise context window or reduce tool output; compaction resumes once the context drops.",
                                    "自动上下文压缩已暂停：上下文窗口太小，压缩无法奏效（system prompt 加逐字尾巴已超过触发线）。请调大上下文窗口或减小工具输出；上下文回落后自动恢复。",
                                )
                                .to_string(),
                            })?;
                        }
                    } else {
                        self.consecutive_compacts.store(0, Ordering::Relaxed);
                    }
                    // Thrashing check: a healthy compaction buys many turns
                    // of breathing room. Refilling within ~3 turns, three
                    // times in a row, means a single oversized item refills
                    // the window and each compaction only craters the cache.
                    let max_seq = self
                        .state
                        .load_visible_turns()?
                        .last()
                        .map(|turn| turn.seq)
                        .unwrap_or(-1);
                    let previous = self.last_compact_max_seq.swap(max_seq, Ordering::Relaxed);
                    // Each turn advances seq by 1 and the compaction summary
                    // itself takes one, so "within 3 turns" is a delta <= 4.
                    if previous >= 0 && max_seq.saturating_sub(previous) <= 4 {
                        let rapid = self.rapid_compacts.fetch_add(1, Ordering::Relaxed) + 1;
                        if rapid >= 3 && !self.compact_stuck.swap(true, Ordering::Relaxed) {
                            on_event(AgentEvent::Notice {
                                text: crate::i18n::text(
                                    "Automatic context compaction paused: the context refills within a few turns of each compaction. A single message or tool output is likely too large for the window — read in smaller chunks, or /clear to start fresh.",
                                    "自动上下文压缩已暂停：每次压缩后几轮内上下文就再次填满。可能有单条消息或工具输出对窗口而言过大——请分块读取，或使用 /clear 重新开始。",
                                )
                                .to_string(),
                            })?;
                        }
                    } else {
                        self.rapid_compacts.store(0, Ordering::Relaxed);
                    }
                }
                result
            }
            "pop" => {
                on_event(AgentEvent::PopStart)?;
                self.trim_visible_context()?;
                on_event(AgentEvent::PopEnd)?;
                None
            }
            _ => None,
        };
        Ok(compact_result)
    }

    fn current_model_supports_vision(&self) -> bool {
        should_use_active_text_pool_for_images(&self.config)
    }

    async fn describe_images_with_vision_provider(
        &self,
        input: &str,
        images: &[&ClipboardImage],
    ) -> Result<String> {
        let vision_cfg = &self.config.plugins.vision;
        if !vision_cfg.enabled {
            bail!(
                "{}",
                crate::i18n::text(
                    "the active text model cannot read images and the vision plugin is disabled",
                    "当前文本模型无法读取图片，并且视觉插件已禁用"
                )
            );
        }
        let strict_pool = self
            .config
            .active_multimodal_provider_models
            .as_ref()
            .is_some_and(|pool| !pool.is_empty());
        let mut descriptions = Vec::new();
        for (i, img) in images.iter().enumerate() {
            let prompt = if input.trim().is_empty() {
                "请简洁描述这张图片，并指出重要细节。".to_string()
            } else {
                format!("用户消息：{input}\n\n请基于图片内容回答或描述图片，不要编造看不见的信息。")
            };
            match vision::analyze_image_url_with_prompt(
                &self.config,
                &self.paths,
                img.data_url(),
                &prompt,
            )
            .await
            {
                Ok(desc) => {
                    descriptions.push(format!("[Image {} 的描述]\n{}", i + 1, desc.trim()));
                }
                Err(error) if strict_pool => {
                    return Err(error).with_context(|| {
                        format!(
                            "configured multimodal model pool failed for image {}",
                            i + 1
                        )
                    });
                }
                Err(error) => {
                    descriptions.push(format!("[Image {} 识图失败: {}]", i + 1, error));
                }
            }
        }
        let combined = descriptions.join("\n\n");
        if input.trim().is_empty() {
            Ok(combined)
        } else {
            Ok(format!("{input}\n\n{combined}"))
        }
    }

    async fn chat_with_tools<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        used_tools: &mut Vec<String>,
        persisted_tool_reports: &mut Vec<(String, String)>,
        replay_start: usize,
        base_tool_reports: &[String],
        initial_tool_rounds: usize,
        initial_question_rounds: usize,
        control: Option<&AgentTurnControl>,
        on_event: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let mut tool_round = initial_tool_rounds;
        let mut question_rounds = initial_question_rounds;
        let mut replay_start = replay_start;
        // Passive overflow recovery is a one-shot barrier per turn: the
        // post-compaction retry must not recover another overflow (pi /
        // opencode / Claude Code all converge on exactly one attempt).
        let mut overflow_recovery_attempted = false;
        let mut loaded_tools = self.initial_loaded_tools(messages)?;
        let mut usage_accumulator = UsageAccumulator::default();
        // v7 cache write-grace: provider prefix-cache writes are async, so a
        // follow-up fired within ~2s can miss the prefix the previous round
        // just computed (measured on DeepSeek). Track round completion time.
        let mut last_round_completed_at: Option<Instant> = None;
        let mut responses_continuation = None;
        let mut continuation_input_start = messages.len();
        let mut continuation_context: Option<(usize, Vec<ChatMessage>)> = None;
        let artifact_auto_publish = self.mode == AgentMode::Normal
            && self.prompt_audience == PromptAudience::External
            && artifact_delivery_requested(messages)
            && self
                .tools
                .lock()
                .unwrap()
                .tool_names()
                .iter()
                .any(|name| name == "create_artifact");
        let mut artifact_candidates = Vec::<AutoArtifactCandidate>::new();
        let mut artifact_published = false;
        loop {
            let tool_limit_reached = self.max_tool_rounds > 0 && tool_round >= self.max_tool_rounds;

            if self.mode != AgentMode::Chat && self.config.skills.enabled {
                if self.mode == AgentMode::Normal {
                    let mut registry = self.tools.lock().unwrap();
                    tools::rescan_scripts(&mut registry, &self.paths);
                    tools::register_script_display_names(&registry);
                }
                let current_fingerprint = {
                    let registry = self.tools.lock().unwrap();
                    registry
                        .contains("load_skill")
                        .then(|| registry.skill_catalog_fingerprint())
                };
                if let Some(current_fingerprint) = current_fingerprint {
                    let config = self.config.clone();
                    let paths = self.paths.clone();
                    let refresh = tokio::task::spawn_blocking(move || {
                        tools::prepare_skill_refresh(current_fingerprint, &config, &paths)
                            .map(|snapshot| (snapshot, config, paths))
                    })
                    .await;
                    match refresh {
                        Ok(Ok((Some(snapshot), config, paths))) => {
                            let mut registry = self.tools.lock().unwrap();
                            tools::apply_skill_refresh(&mut registry, &config, &paths, snapshot);
                        }
                        Ok(Ok((None, _, _))) => {}
                        Ok(Err(error)) => {
                            tracing::warn!(error = %error, "failed to refresh Miyu skill catalog")
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "Miyu skill catalog worker stopped")
                        }
                    }
                }
            }

            let definitions = if self.tools_enabled && !tool_limit_reached {
                let tools = self.tools.lock().unwrap();
                if tools::is_stub_loading_mode(&self.config.tools.loading_mode) {
                    tools.stub_definitions()
                } else if tools::is_hybrid_loading_mode(&self.config.tools.loading_mode) {
                    tools.lazy_definitions(&loaded_tools)
                } else {
                    tools.definitions()
                }
            } else {
                Vec::new()
            };

            on_event(AgentEvent::ReasoningStart {
                received_at: Instant::now(),
            })?;
            let (chunk_tx, mut chunk_rx) =
                tokio::sync::mpsc::unbounded_channel::<(ChatStreamChunk, Instant)>();
            let mut request_messages = if responses_continuation.is_some() {
                messages
                    .get(continuation_input_start..)
                    .context("Responses continuation input cursor is out of bounds")?
                    .to_vec()
            } else {
                messages.clone()
            };
            if let Some((context_index, context_messages)) = continuation_context.as_ref() {
                let offset = context_index
                    .checked_sub(continuation_input_start)
                    .context("Responses continuation context cursor is out of bounds")?;
                if offset > request_messages.len() {
                    bail!("Responses continuation context cursor is out of bounds");
                }
                request_messages.splice(offset..offset, context_messages.clone());
            }
            let mut reasoning_filter = ReasoningTitleFilter::default();
            if self.config.cache.write_grace_ms > 0 {
                if let Some(previous) = last_round_completed_at {
                    let grace = std::time::Duration::from_millis(self.config.cache.write_grace_ms);
                    let elapsed = previous.elapsed();
                    if elapsed < grace {
                        tokio::time::sleep(grace - elapsed).await;
                    }
                }
            }
            if self.config.cache.keepalive_seconds > 0 && responses_continuation.is_none() {
                self.last_request_snapshot =
                    Some((request_messages.clone(), definitions.clone()));
            }
            let round_streamed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let round = {
                let streamed_flag = round_streamed.clone();
                let llm_future = self.client.chat_stream_with_continuation(
                    request_messages.clone(),
                    definitions,
                    responses_continuation.as_deref(),
                    move |chunk| {
                        streamed_flag.store(true, Ordering::Relaxed);
                        let _ = chunk_tx.send((chunk, Instant::now()));
                        Ok(())
                    },
                );
                tokio::pin!(llm_future);
                let mut spinner_interval = tokio::time::interval(SPINNER_INTERVAL);
                spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                spinner_interval.tick().await;
                let supersede = control.and_then(|control| control.supersede.as_deref());
                let supersede_generation = control.and_then(|control| {
                    supersede.map(|_| control.supersede_seen.load(Ordering::Acquire))
                });
                loop {
                    tokio::select! {
                        biased;
                        _ = async {
                            match (supersede, supersede_generation) {
                                (Some(signal), Some(generation)) => signal.wait_after(generation).await,
                                _ => std::future::pending::<()>().await,
                            }
                        } => {
                            break None;
                        }
                        result = &mut llm_future => {
                            break Some(result);
                        }
                        Some((chunk, received_at)) = chunk_rx.recv() => {
                            emit_model_chunk_at(
                                chunk,
                                received_at,
                                &mut reasoning_filter,
                                on_event,
                            )?;
                        }
                        _ = spinner_interval.tick() => {
                            on_event(AgentEvent::SpinnerTick)?;
                        }
                    }
                }
            };
            let round = match round {
                Some(Err(error)) => {
                    // Passive overflow trigger (compact-and-retry). Only at
                    // the turn's initial request, before any assistant output
                    // was streamed: mid-loop the live tool exchange is not
                    // rebuildable from the DB, and a partially shown answer
                    // must not be silently retried (opencode's
                    // hasAssistantStarted guard).
                    let initial_request = tool_round == initial_tool_rounds
                        && question_rounds == initial_question_rounds
                        && responses_continuation.is_none()
                        && !round_streamed.load(Ordering::Relaxed);
                    let window = self.context_window();
                    if initial_request
                        && !overflow_recovery_attempted
                        && window.is_some()
                        && crate::llm::is_context_overflow_error(&error)
                    {
                        overflow_recovery_attempted = true;
                        let window = window.unwrap();
                        let check = overflow::OverflowCheck::new(
                            Some(window),
                            self.trim_at_ratio,
                            None,
                        );
                        on_event(AgentEvent::CompactStart)?;
                        let compactor = compact::Compactor::new(
                            self.client.clone(),
                            self.state.clone(),
                            window,
                            check.reserved_tokens,
                            self.compact_tail_budget(window),
                            matches!(self.mode, AgentMode::Chat),
                        );
                        let mut on_compact_chunk =
                            |chunk: ChatStreamChunk| on_event(AgentEvent::CompactChunk(chunk));
                        // No fork here: a fork of an overflowing conversation
                        // overflows identically — recovery must use the
                        // isolated serialized path.
                        let compacted = compactor
                            .perform_compact(true, true, None, &mut on_compact_chunk)
                            .await;
                        on_event(AgentEvent::CompactEnd)?;
                        if let Ok(Some(compact_result)) = compacted {
                            self.state.add_auxiliary_usage(
                                &compact_result.usage,
                                crate::state::UsageMeta {
                                    source: self.usage_source(),
                                    provider: compact_result.provider_id.as_deref(),
                                    model: None,
                                },
                            )?;
                            // Splice the rebuilt (compacted) history prefix in
                            // front of the current turn's user message; the
                            // live tail (user input, runtime stamp, hints)
                            // is preserved byte-for-byte.
                            let user_index = replay_start.saturating_sub(2).min(messages.len());
                            let rebuilt = self.chat_messages(current_turn_id, "")?;
                            let prefix_len = rebuilt.len().saturating_sub(2);
                            let tail = messages.split_off(user_index);
                            messages.clear();
                            messages.extend(rebuilt.into_iter().take(prefix_len));
                            messages.extend(tail);
                            replay_start = prefix_len + 2;
                            continuation_input_start = messages.len();
                            tracing::info!(
                                folded = compact_result.folded_turns,
                                kept = compact_result.kept_turns,
                                "context overflow recovered by compact-and-retry"
                            );
                            continue;
                        }
                        if let Err(compact_error) = compacted {
                            tracing::warn!(
                                error = %compact_error,
                                "compact-and-retry failed; surfacing the original overflow"
                            );
                        }
                    }
                    return Err(error);
                }
                Some(Ok(result)) => Some(result),
                None => None,
            };
            let Some(result) = round else {
                if let Some(control) = control {
                    if let Some(generation) = control.pending_supersede_generation() {
                        control.mark_supersede_seen(generation);
                    }
                }
                let queued = self.state.load_queued_prompts()?;
                if queued.is_empty() {
                    continue;
                }
                let prompt_ids = queued
                    .iter()
                    .map(|prompt| prompt.prompt_id.clone())
                    .collect::<Vec<_>>();
                on_event(AgentEvent::GenerationSuperseded { prompt_ids })?;
                let checkpoint = redo_checkpoint_payload(
                    messages,
                    replay_start,
                    base_tool_reports,
                    persisted_tool_reports,
                    tool_round,
                    question_rounds,
                );
                let continuation_context_index = responses_continuation.as_ref().map(|_| {
                    continuation_context
                        .as_ref()
                        .map(|(index, _)| *index)
                        .unwrap_or(messages.len())
                });
                self.consume_queued_prompts(
                    current_turn_id,
                    messages,
                    queued,
                    (None, None, None, None),
                    checkpoint,
                    control.expect("supersede requires turn control"),
                    on_event,
                )
                .await?;
                if let Some(index) = continuation_context_index {
                    continuation_context = Some((
                        index,
                        vec![
                            ChatMessage::turn_context(continuation_system_prompt(
                                &self.system_prompt,
                                self.mode,
                            )),
                            ChatMessage::turn_context(runtime_context(
                                self.mode,
                                self.platform_context.is_some(),
                            )),
                        ],
                    ));
                }
                continue;
            };
            while let Ok((chunk, received_at)) = chunk_rx.try_recv() {
                emit_model_chunk_at(chunk, received_at, &mut reasoning_filter, on_event)?;
            }
            let (title, text) = reasoning_filter.finish();
            if let Some(title) = title {
                on_event(AgentEvent::ReasoningTitle(title))?;
            }
            if let Some(text) = text {
                on_event(AgentEvent::Chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Reasoning,
                    text,
                }))?;
            }
            usage_accumulator.add_result(&result, messages);
            last_round_completed_at = Some(Instant::now());
            if result.tool_calls.is_empty() || !self.tools_enabled {
                responses_continuation = None;
                continuation_input_start = messages.len();
                continuation_context = None;
                if let Some(control) = control {
                    let queued = self.state.load_queued_prompts()?;
                    if !queued.is_empty() {
                        if let Some(generation) = control.pending_supersede_generation() {
                            let prompt_ids = queued
                                .iter()
                                .map(|prompt| prompt.prompt_id.clone())
                                .collect();
                            on_event(AgentEvent::GenerationSuperseded { prompt_ids })?;
                            let checkpoint = redo_checkpoint_payload(
                                messages,
                                replay_start,
                                base_tool_reports,
                                persisted_tool_reports,
                                tool_round,
                                question_rounds,
                            );
                            self.consume_queued_prompts(
                                current_turn_id,
                                messages,
                                queued,
                                (None, None, None, None),
                                checkpoint,
                                control,
                                on_event,
                            )
                            .await?;
                            control.mark_supersede_seen(generation);
                            continue;
                        }
                        push_assistant_context_messages(
                            messages,
                            &result.content,
                            result.reasoning.as_deref(),
                            true,
                        );
                        let checkpoint = redo_checkpoint_payload(
                            messages,
                            replay_start,
                            base_tool_reports,
                            persisted_tool_reports,
                            tool_round,
                            question_rounds,
                        );
                        self.consume_queued_prompts(
                            current_turn_id,
                            messages,
                            queued,
                            (
                                Some(&result.content),
                                result.reasoning.as_deref(),
                                result.provider_id.as_deref(),
                                result.model.as_deref(),
                            ),
                            checkpoint,
                            control,
                            on_event,
                        )
                        .await?;
                        continue;
                    }
                }
                let mut result = result;
                if artifact_auto_publish && !artifact_published {
                    publish_auto_artifact_candidates(&artifact_candidates, on_event)?;
                }
                if let Some(usage) = usage_accumulator.usage() {
                    result.last_request_usage = result.usage.take();
                    result.usage = Some(usage);
                    result.usage_estimated = usage_accumulator.estimated;
                }
                return Ok(result);
            }
            if tool_limit_reached {
                let mut result = result;
                let warning = format!(
                    "工具调用已达到上限 {} 轮，未执行后续工具调用。可将 `tools.max_rounds` 设为 0 以允许无限工具调用。",
                    self.max_tool_rounds
                );
                let warning_chunk = if result.content.trim().is_empty() {
                    warning.clone()
                } else {
                    format!("\n\n{warning}")
                };
                result.content.push_str(&warning_chunk);
                on_event(AgentEvent::Chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Content,
                    text: warning_chunk,
                }))?;
                result.tool_calls.clear();
                if let Some(usage) = usage_accumulator.usage() {
                    result.last_request_usage = result.usage.take();
                    result.usage = Some(usage);
                    result.usage_estimated = usage_accumulator.estimated;
                }
                return Ok(result);
            }
            tool_round += 1;
            let next_responses_continuation = result.responses_continuation.clone();
            push_assistant_message_with_reasoning(
                messages,
                result.content.clone(),
                result.reasoning.as_deref(),
                result.thinking_signature.as_deref(),
                Some(result.tool_calls.clone()),
                true,
            );
            if result
                .finish_reason
                .as_deref()
                .is_some_and(|reason| reason.eq_ignore_ascii_case("length"))
                && !result.tool_calls.is_empty()
            {
                // A "length" stop means the output hit the token limit, so every
                // tool call in this message may carry silently truncated
                // arguments. Refuse to execute any of them and let the model
                // re-issue the calls with complete arguments.
                for call in &result.tool_calls {
                    messages.push(ChatMessage::tool(
                        call.id.clone(),
                        "error: 本次回复因输出 token 上限被截断，工具调用参数可能不完整。请重新发起该工具调用并给出完整参数。",
                    ));
                }
                continue;
            }
            if next_responses_continuation.is_some() {
                continuation_input_start = messages.len();
            }
            responses_continuation = next_responses_continuation;
            continuation_context = None;
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
                question_rounds += 1;
            }
            let question_round_allowed =
                question_call_count == 1 && question_rounds <= MAX_QUESTION_ROUNDS_PER_TURN;
            let defer_sibling_tools = question_call_count == 1 && result.tool_calls.len() > 1;
            // Multiple `task` calls in one batch run concurrently (subagents
            // are independent by design); everything else stays serial.
            let mut parallel_task_outputs =
                if defer_sibling_tools || self.mode == AgentMode::Chat {
                    std::collections::HashMap::new()
                } else {
                    self.execute_parallel_task_calls(&result.tool_calls, &loaded_tools, on_event)
                        .await?
                };
            for (call_index, call) in result.tool_calls.into_iter().enumerate() {
                if let Some(group_output) = parallel_task_outputs.remove(&call_index) {
                    // Executed in the parallel group; events already emitted.
                    used_tools.push(call.function.name.clone());
                    if let Some(report) = group_output.report {
                        persisted_tool_reports.push((call.function.name.clone(), report));
                    }
                    messages.push(ChatMessage::tool(call.id, group_output.output));
                    continue;
                }
                let call_id = call.id.clone();
                let event_name = tool_event_name(&call.function.name, &call.function.arguments);
                on_event(AgentEvent::ToolCall {
                    call_id: call_id.clone(),
                    name: event_name.clone(),
                    arguments: call.function.arguments.clone(),
                })?;
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
                    if !question_round_allowed {
                        let output = format!(
                            "tool error: ask_question exceeded the per-turn limit of {MAX_QUESTION_ROUNDS_PER_TURN}"
                        );
                        on_event(AgentEvent::ToolResult {
                            call_id: call_id.clone(),
                            name: event_name.clone(),
                            ok: false,
                            output: output.clone(),
                        })?;
                        messages.push(ChatMessage::tool(call.id, output));
                        continue;
                    }
                    let request = match QuestionRequest::parse(&call.function.arguments) {
                        Ok(request) => request,
                        Err(err) => {
                            let output = format!("tool error: invalid ask_question request: {err}");
                            on_event(AgentEvent::ToolResult {
                                call_id: call_id.clone(),
                                name: event_name.clone(),
                                ok: false,
                                output: output.clone(),
                            })?;
                            messages.push(ChatMessage::tool(call.id, output));
                            continue;
                        }
                    };
                    let (response_tx, response_rx) = oneshot::channel();
                    on_event(AgentEvent::AskQuestion {
                        call_id: call_id.clone(),
                        request: request.clone(),
                        responder: response_tx,
                    })?;
                    let response = response_rx.await.unwrap_or(QuestionResponse::Cancelled);
                    let output = match response {
                        QuestionResponse::Answered(answers) => {
                            let exchange = QuestionExchange::new(request, answers)?;
                            self.state
                                .append_question_exchange(current_turn_id, &exchange)?;
                            answered_tool_output(&exchange)
                        }
                        QuestionResponse::Closed => closed_tool_output(),
                        QuestionResponse::Cancelled => return Err(QuestionCancelled.into()),
                        QuestionResponse::Unavailable(reason) => unavailable_tool_output(&reason),
                    };
                    messages.push(ChatMessage::tool(call.id, output.clone()));
                    on_event(AgentEvent::ToolResult {
                        call_id: call_id.clone(),
                        name: event_name,
                        ok: true,
                        output,
                    })?;
                    continue;
                }
                used_tools.push(call.function.name.clone());
                {
                    let tools = self.tools.lock().unwrap();
                    let permission = tools.permission(&call.function.name)?;
                    if !mode_allows_tool_permission(self.mode, permission) {
                        bail!(
                            "{} mode blocked non-read-only tool: {}",
                            self.mode.label(),
                            call.function.name
                        );
                    }
                    if tools::is_hybrid_loading_mode(&self.config.tools.loading_mode)
                        && call.function.name != "load_tools"
                        && tools.requires_lazy_load(&call.function.name, &loaded_tools)
                    {
                        if tools.can_auto_load_direct_call(&call.function.name) {
                            loaded_tools.insert(call.function.name.clone());
                            if self.config.tools.persist_loaded_tools {
                                self.state.add_session_loaded_tools(
                                    &[call.function.name.clone()],
                                    Some(current_turn_id),
                                )?;
                            }
                        } else {
                            let output = format!(
                                "tool error: 工具 `{}` 尚未加载。请先调用 load_tools，参数为 {{\"names\":[\"{}\"]}}。",
                                call.function.name,
                                call.function.name,
                            );
                            on_event(AgentEvent::ToolResult {
                                call_id: call_id.clone(),
                                name: event_name.clone(),
                                ok: false,
                                output: output.clone(),
                            })?;
                            messages.push(ChatMessage::tool(call.id, output));
                            continue;
                        }
                    }
                }
                if call.function.name == "install_aur_package"
                    && used_tools.iter().any(|name| name == "review_aur_package")
                {
                    let output = "tool error: install_aur_package cannot run in the same turn as review_aur_package; ask the user to confirm installation first".to_string();
                    on_event(AgentEvent::ToolResult {
                        call_id: call_id.clone(),
                        name: event_name.clone(),
                        ok: false,
                        output: output.clone(),
                    })?;
                    messages.push(ChatMessage::tool(call.id, output));
                    continue;
                }
                let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
                let tool_future = {
                    let tools = self.tools.lock().unwrap();
                    tools.call_with_progress_future(
                        &call.function.name,
                        &call.function.arguments,
                        progress_tx,
                    )
                };
                let tool_future = match tool_future {
                    Ok(f) => f,
                    Err(err) => {
                        let output = format!("tool error: {err}");
                        on_event(AgentEvent::ToolResult {
                            call_id: call_id.clone(),
                            name: event_name.clone(),
                            ok: false,
                            output: output.clone(),
                        })?;
                        messages.push(ChatMessage::tool(call.id, output));
                        continue;
                    }
                };
                tokio::pin!(tool_future);
                let mut spinner_interval = tokio::time::interval(SPINNER_INTERVAL);
                spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                spinner_interval.tick().await;
                let (output, tool_succeeded) = loop {
                    tokio::select! {
                        result = &mut tool_future => {
                            break match result {
                                Ok(output) => {
                                    while let Ok(progress) = progress_rx.try_recv() {
                                        emit_tool_progress(on_event, &call_id, &event_name, progress)?;
                                    }
                                    (output, true)
                                }
                                Err(err) => {
                                    while let Ok(progress) = progress_rx.try_recv() {
                                        emit_tool_progress(on_event, &call_id, &event_name, progress)?;
                                    }
                                    on_event(AgentEvent::ToolResult {
                                        call_id: call_id.clone(),
                                        name: event_name.clone(),
                                        ok: false,
                                        output: format!("tool error: {err}"),
                                    })?;
                                    (format!("tool error: {err}"), false)
                                }
                            };
                        }
                        Some(progress) = progress_rx.recv() => {
                            emit_tool_progress(on_event, &call_id, &event_name, progress)?;
                        }
                        _ = spinner_interval.tick() => {
                            on_event(AgentEvent::SpinnerTick)?;
                        }
                    }
                };
                let clipboard_image = if tool_succeeded {
                    clipboard_binary_image_from_tool_result(&call.function.name, &output)
                } else {
                    None
                };
                messages.push(ChatMessage::tool(call.id, output.clone()));
                if tool_succeeded && call.function.name == "load_tools" {
                    let loaded = loaded_items_from_output(&output);
                    for name in &loaded.tools {
                        loaded_tools.insert(name.clone());
                    }
                    if self.config.tools.persist_loaded_tools {
                        self.state
                            .add_session_loaded_tools(&loaded.tools, Some(current_turn_id))?;
                        self.state
                            .add_session_loaded_targets(&loaded.targets, Some(current_turn_id))?;
                    }
                }
                if let Some(img) = clipboard_image {
                    let supports_vision = self.current_model_supports_vision();
                    let uses_vision_fallback =
                        !supports_vision && self.config.plugins.vision.enabled;
                    if !supports_vision {
                        let message = if self.config.plugins.vision.enabled {
                            if crate::i18n::is_zh() {
                                "视觉分析."
                            } else {
                                "Vision analysis."
                            }
                        } else if crate::i18n::is_zh() {
                            "当前模型不支持图片，且未启用视觉模型，无法分析剪贴板图片。"
                        } else {
                            "The current model does not support images and the vision plugin is disabled, so the clipboard image cannot be analyzed."
                        };
                        on_event(AgentEvent::ToolProgress {
                            call_id: call_id.clone(),
                            name: event_name.clone(),
                            message: message.to_string(),
                        })?;
                    }
                    let image_message = if uses_vision_fallback {
                        let image_future = self.clipboard_image_message(img);
                        tokio::pin!(image_future);
                        let mut spinner_interval = tokio::time::interval(SPINNER_INTERVAL);
                        spinner_interval
                            .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        spinner_interval.tick().await;
                        let mut progress_interval =
                            tokio::time::interval(Duration::from_millis(900));
                        progress_interval
                            .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        progress_interval.tick().await;
                        let mut progress_tick = 0usize;
                        loop {
                            tokio::select! {
                                result = &mut image_future => {
                                    break result?;
                                }
                                _ = progress_interval.tick() => {
                                    progress_tick = progress_tick.wrapping_add(1);
                                    on_event(AgentEvent::ToolProgress {
                                        call_id: call_id.clone(),
                                        name: event_name.clone(),
                                        message: vision_analysis_progress(progress_tick),
                                    })?;
                                }
                                _ = spinner_interval.tick() => {
                                    on_event(AgentEvent::SpinnerTick)?;
                                }
                            }
                        }
                    } else {
                        self.clipboard_image_message(img).await?
                    };
                    if let Some(message) = image_message {
                        messages.push(message);
                    }
                }
                if tool_succeeded {
                    let result_ok = tool_output_succeeded(&output);
                    if result_ok {
                        if let Some(delta) =
                            tool_call_footprint(&call.function.name, &call.function.arguments)
                        {
                            self.state.merge_turn_footprint(current_turn_id, &delta)?;
                        }
                        if matches!(
                            call.function.name.as_str(),
                            "create_artifact" | "apply_artifact_patch" | "present_artifact"
                        ) {
                            artifact_published = true;
                        } else if artifact_auto_publish {
                            for path in artifact_candidate_paths(&call.function.name, &output) {
                                artifact_candidates.push(AutoArtifactCandidate {
                                    call_id: call_id.clone(),
                                    tool_name: event_name.clone(),
                                    path,
                                });
                            }
                        }
                    }
                    on_event(AgentEvent::ToolResult {
                        call_id,
                        name: event_name.clone(),
                        ok: result_ok,
                        output: output.clone(),
                    })?;
                    if let Some(report) =
                        extract_persistable_tool_report(&call.function.name, &output)
                    {
                        persisted_tool_reports.push((call.function.name.clone(), report));
                    }
                }
            }
            if question_round_allowed {
                tool_round = tool_round.saturating_sub(1);
            }
            if let Some(control) = control {
                if let Some(queue_ingress) = control.queue_ingress.as_ref() {
                    queue_ingress.wait_for_reserved_ingress().await;
                }
                let queued = self.state.load_queued_prompts()?;
                if !queued.is_empty() {
                    let supersede_generation = control.pending_supersede_generation();
                    if supersede_generation.is_some() {
                        let prompt_ids = queued
                            .iter()
                            .map(|prompt| prompt.prompt_id.clone())
                            .collect();
                        on_event(AgentEvent::GenerationSuperseded { prompt_ids })?;
                    }
                    let checkpoint = redo_checkpoint_payload(
                        messages,
                        replay_start,
                        base_tool_reports,
                        persisted_tool_reports,
                        tool_round,
                        question_rounds,
                    );
                    let preceding_assistant = if supersede_generation.is_some() {
                        (None, None, None, None)
                    } else {
                        (
                            Some(result.content.as_str()),
                            result.reasoning.as_deref(),
                            result.provider_id.as_deref(),
                            result.model.as_deref(),
                        )
                    };
                    let continuation_context_index = responses_continuation.as_ref().map(|_| {
                        continuation_context
                            .as_ref()
                            .map(|(index, _)| *index)
                            .unwrap_or(messages.len())
                    });
                    self.consume_queued_prompts(
                        current_turn_id,
                        messages,
                        queued,
                        preceding_assistant,
                        checkpoint,
                        control,
                        on_event,
                    )
                    .await?;
                    if let Some(index) = continuation_context_index {
                        continuation_context = Some((
                            index,
                            vec![
                                ChatMessage::turn_context(continuation_system_prompt(
                                    &self.system_prompt,
                                    self.mode,
                                )),
                                ChatMessage::turn_context(runtime_context(
                                    self.mode,
                                    self.platform_context.is_some(),
                                )),
                            ],
                        ));
                    }
                    if let Some(generation) = supersede_generation {
                        control.mark_supersede_seen(generation);
                    }
                }
            }
        }
    }

    fn initial_loaded_tools(&self, messages: &[ChatMessage]) -> Result<BTreeSet<String>> {
        if !self.config.tools.persist_loaded_tools {
            return Ok(BTreeSet::new());
        }
        let mut loaded = self.state.load_session_loaded_tools()?;
        if loaded.is_empty() {
            loaded = loaded_tools_from_messages(messages);
            if !loaded.is_empty() {
                let names = loaded.iter().cloned().collect::<Vec<_>>();
                self.state.add_session_loaded_tools(&names, None)?;
            }
        }
        if !loaded.is_empty() {
            let tools = self.tools.lock().unwrap();
            let available = tools.tool_names().into_iter().collect::<BTreeSet<_>>();
            loaded.retain(|name| available.contains(name));
        }
        Ok(loaded)
    }

    async fn clipboard_image_message(&self, img: ClipboardImage) -> Result<Option<ChatMessage>> {
        if self.current_model_supports_vision() {
            return Ok(Some(ChatMessage::user_parts(vec![
                ChatContentPart::ImageUrl {
                    image_url: ImageUrlContent {
                        url: img.data_url().to_string(),
                    },
                },
            ])));
        }

        let images = vec![&img];
        let description = self
            .describe_images_with_vision_provider("", &images)
            .await?;
        if description.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(ChatMessage::plain("user", description)))
    }

    fn chat_messages(
        &self,
        current_turn_id: &str,
        current_input: &str,
    ) -> Result<Vec<ChatMessage>> {
        let mut messages = vec![ChatMessage::system(self.system_prompt.clone())];
        if !self.suppress_session_history {
            if let Some(summary) = self.state.load_last_summary()? {
                messages.push(summary_checkpoint_message(&summary.assistant_content));
            }
            let turns = self.state.load_visible_turns_excluding(current_turn_id)?;
            for turn in &turns {
                if turn.is_summary {
                    continue;
                }
                // A turn still running holds a placeholder that gets overwritten
                // with the real reply once it finishes, so replaying it would
                // put two different byte sequences at the same position and
                // drop the prefix cache for everyone after it. Roughly a fifth
                // of this group's turns overlap. The placeholder only ever said
                // "ignore me" anyway.
                if turn.status == crate::state::TurnStatus::Running {
                    continue;
                }
                self.push_history_turn(&mut messages, turn);
            }
        }
        // v7 §三: the minute-level runtime stamp is transient tail and must sit
        // AFTER the current user message. When it preceded the user message,
        // every next turn's replayed history diverged from the provider's
        // cached prefix exactly at this position, capping cross-turn prefix
        // cache reuse at the end of the stored history (verified byte-level
        // against DeepSeek prefix caching).
        messages.push(ChatMessage::plain("user", current_input));
        messages.push(ChatMessage::turn_context(runtime_context(
            self.mode,
            self.platform_context.is_some(),
        )));
        Ok(messages)
    }

    /// Renders one stored turn exactly as the live request rendered it
    /// (byte-identical replay incl. the fossilized transient tail), shared by
    /// the main request path and the compaction fork prefix.
    fn push_history_turn(&self, messages: &mut Vec<ChatMessage>, turn: &crate::state::Turn) {
        messages.push(self.turn_user_message(turn));
        // Fossilized transient tail (v7 append-only): replay the
        // system messages that followed the user message in the live
        // request, byte-identical and in order, so this turn renders
        // as a pure extension of what the provider already cached.
        messages.extend(turn.context_messages.iter().map(replay_fossil));
        if turn.status == crate::state::TurnStatus::Interrupted && !turn.journal_events.is_empty() {
            messages.extend(interrupted_turn_replay_messages(self, turn));
        } else {
            for exchange in &turn.question_exchanges {
                messages.push(ChatMessage::plain(
                    "assistant",
                    crate::question::assistant_exchange_text(exchange),
                ));
                messages.push(ChatMessage::plain(
                    "user",
                    crate::question::user_exchange_text(exchange),
                ));
            }
            for followup in &turn.followups {
                push_assistant_context_messages(
                    messages,
                    followup
                        .preceding_assistant_content
                        .as_deref()
                        .unwrap_or_default(),
                    followup.preceding_assistant_reasoning.as_deref(),
                    false,
                );
                messages.push(self.followup_user_message(followup));
            }
            push_assistant_context_messages(
                messages,
                &turn.assistant_content,
                turn.assistant_reasoning.as_deref(),
                true,
            );
            if !turn.tool_reports.is_empty() {
                messages.push(ChatMessage::turn_context(private_tool_memory(
                    &turn.tool_reports,
                )));
            }
        }
    }

    /// Byte-identical prefix of the live conversation covering exactly the
    /// turns about to fold: `[system][checkpoint][fold turns...]`. A fork
    /// summarization request built on this prefix re-reads the history at
    /// cached price instead of full price (the serialized fallback shares no
    /// bytes with the provider's cache).
    fn compact_fork_prefix(&self, fold_turn_ids: &[String]) -> Result<Vec<ChatMessage>> {
        let fold: std::collections::HashSet<&str> =
            fold_turn_ids.iter().map(|id| id.as_str()).collect();
        let mut messages = vec![ChatMessage::system(self.system_prompt.clone())];
        if let Some(summary) = self.state.load_last_summary()? {
            messages.push(summary_checkpoint_message(&summary.assistant_content));
        }
        for turn in self.state.load_visible_turns()? {
            if turn.is_summary || !fold.contains(turn.turn_id.as_str()) {
                continue;
            }
            self.push_history_turn(&mut messages, &turn);
        }
        Ok(messages)
    }

    fn live_tool_definitions(&self) -> Result<Vec<crate::llm::ToolDefinition>> {
        if !self.tools_enabled {
            return Ok(Vec::new());
        }
        let loaded = self.initial_loaded_tools(&[])?;
        let tools = self.tools.lock().unwrap();
        Ok(
            if tools::is_stub_loading_mode(&self.config.tools.loading_mode) {
                tools.stub_definitions()
            } else if tools::is_hybrid_loading_mode(&self.config.tools.loading_mode) {
                tools.lazy_definitions(&loaded)
            } else {
                tools.definitions()
            },
        )
    }

    fn followup_user_message(&self, followup: &crate::state::TurnFollowup) -> ChatMessage {
        if !self.current_model_supports_vision() {
            return ChatMessage::plain("user", &followup.content);
        }
        let mut images = followup
            .attachments
            .iter()
            .filter_map(|attachment| match attachment {
                QueuedPromptAttachment::Binary { mime, data_base64 } => {
                    Some(ChatContentPart::ImageUrl {
                        image_url: ImageUrlContent {
                            url: format!("data:{mime};base64,{data_base64}"),
                        },
                    })
                }
                QueuedPromptAttachment::Path { .. } => None,
            })
            .collect::<Vec<_>>();
        images.extend(self.uploaded_attachment_image_parts(&followup.uploaded_attachments));
        if images.is_empty() {
            return ChatMessage::plain("user", &followup.content);
        }
        let mut parts = vec![ChatContentPart::Text {
            text: followup.content.clone(),
        }];
        parts.extend(images);
        ChatMessage::user_parts(parts)
    }

    fn turn_user_message(&self, turn: &crate::state::Turn) -> ChatMessage {
        if !self.current_model_supports_vision() {
            return ChatMessage::plain("user", &turn.user_content);
        }
        let images = self.uploaded_attachment_image_parts(&turn.attachments);
        if images.is_empty() {
            return ChatMessage::plain("user", &turn.user_content);
        }
        let mut parts = vec![ChatContentPart::Text {
            text: turn.user_content.clone(),
        }];
        parts.extend(images);
        ChatMessage::user_parts(parts)
    }

    fn uploaded_attachment_image_parts(
        &self,
        attachments: &[crate::state::UserAttachment],
    ) -> Vec<ChatContentPart> {
        attachments
            .iter()
            .filter(|attachment| attachment.kind == "image")
            .filter_map(|attachment| {
                self.state
                    .load_user_attachment(&attachment.attachment_id)
                    .ok()
                    .flatten()
            })
            .map(|attachment| ChatContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: ClipboardImage::new(attachment.attachment.mime, attachment.bytes)
                        .data_url()
                        .to_string(),
                },
            })
            .collect()
    }

    fn queued_prompt_images(&self, prompt: &QueuedPrompt) -> Result<Vec<Option<PastedImage>>> {
        let mut images = queued_prompt_images(prompt)?;
        for attachment in &prompt.uploaded_attachments {
            if attachment.kind != "image" {
                continue;
            }
            if let Some(data) = self.state.load_user_attachment(&attachment.attachment_id)? {
                images.push(Some(PastedImage::Binary(ClipboardImage::new(
                    data.attachment.mime,
                    data.bytes,
                ))));
            }
        }
        Ok(images)
    }
}

fn tool_output_succeeded(output: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .and_then(|value| {
            value
                .get("success")
                .and_then(serde_json::Value::as_bool)
                .or_else(|| value.get("ok").and_then(serde_json::Value::as_bool))
        })
        .unwrap_or(true)
}

fn mode_allows_tool_permission(mode: AgentMode, permission: ToolPermission) -> bool {
    match mode {
        AgentMode::Normal => true,
        AgentMode::Chat => permission == ToolPermission::ReadOnly,
    }
}

#[derive(Debug)]
struct AutoArtifactCandidate {
    call_id: String,
    tool_name: String,
    path: PathBuf,
}

fn artifact_delivery_requested(messages: &[ChatMessage]) -> bool {
    let text = messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(chat_message_text)
        .unwrap_or_default()
        .to_lowercase();
    let zh_action = ["生成", "创建", "制作", "导出", "保存为", "写一", "写个"]
        .iter()
        .any(|word| text.contains(word));
    let zh_deliverable = [
        "报告",
        "文档",
        "文件",
        "网页",
        "页面",
        "表格",
        "清单",
        "markdown",
        "md",
        "html",
        "json",
        "csv",
        "pdf",
        "代码文件",
        "独立脚本",
        "示例程序",
    ]
    .iter()
    .any(|word| text.contains(word));
    let en_action = ["create", "generate", "write", "make", "export", "save"]
        .iter()
        .any(|word| text.split_whitespace().any(|part| part == *word));
    let en_deliverable = [
        "report",
        "document",
        "file",
        "webpage",
        "page",
        "table",
        "spreadsheet",
        "markdown",
        "html",
        "json",
        "csv",
        "pdf",
        "script",
        "standalone program",
    ]
    .iter()
    .any(|word| text.contains(word));
    (zh_action && zh_deliverable) || (en_action && en_deliverable)
}

fn chat_message_text(message: &ChatMessage) -> Option<String> {
    match message.content.as_ref()? {
        ChatContent::Text(text) => Some(text.clone()),
        ChatContent::Parts(parts) => Some(
            parts
                .iter()
                .filter_map(|part| match part {
                    ChatContentPart::Text { text } => Some(text.as_str()),
                    ChatContentPart::ImageUrl { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    }
}

fn artifact_candidate_paths(tool_name: &str, output: &str) -> Vec<PathBuf> {
    let Ok(payload) = serde_json::from_str::<Value>(output) else {
        return Vec::new();
    };
    let raw_paths = match tool_name {
        "write_file" if payload.get("created").and_then(Value::as_bool) == Some(true) => payload
            .get("path")
            .and_then(Value::as_str)
            .into_iter()
            .collect::<Vec<_>>(),
        "apply_patch" => payload
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|file| file.get("operation").and_then(Value::as_str) == Some("add"))
            .filter_map(|file| file.get("path").and_then(Value::as_str))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    raw_paths
        .into_iter()
        .map(resolve_tool_output_path)
        .filter(|path| artifact_candidate_extension(path))
        .collect()
}

fn resolve_tool_output_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        tools::workspace::effective_workdir().join(path)
    }
}

fn artifact_candidate_extension(path: &std::path::Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "md" | "markdown"
            | "html"
            | "htm"
            | "pdf"
            | "json"
            | "jsonl"
            | "csv"
            | "tsv"
            | "txt"
            | "log"
            | "css"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "rs"
            | "py"
            | "sh"
            | "toml"
            | "yaml"
            | "yml"
            | "xml"
            | "sql"
    )
}

fn publish_auto_artifact_candidates<F>(
    candidates: &[AutoArtifactCandidate],
    on_event: &mut F,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    let mut published = HashSet::new();
    for candidate in candidates {
        let key = candidate
            .path
            .canonicalize()
            .unwrap_or_else(|_| candidate.path.clone());
        if !published.insert(key) || !candidate.path.is_file() {
            continue;
        }
        on_event(AgentEvent::Artifact {
            call_id: candidate.call_id.clone(),
            name: candidate.tool_name.clone(),
            path: candidate.path.clone(),
            title: String::new(),
        })?;
    }
    Ok(())
}

#[derive(Default)]
struct UsageAccumulator {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    cache_reported: bool,
    has_usage: bool,
    estimated: bool,
}

impl UsageAccumulator {
    fn add_result(&mut self, result: &ChatResult, request_messages: &[ChatMessage]) {
        if let Some(usage) = &result.usage {
            self.add_usage(usage, false);
            return;
        }

        let prompt_tokens = overflow::estimate_messages_tokens(request_messages) as u64;
        let completion_tokens = estimate_result_tokens(result) as u64;
        self.add_usage(
            &Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens.saturating_add(completion_tokens),
                ..Usage::default()
            },
            true,
        );
    }

    fn add_usage(&mut self, usage: &Usage, estimated: bool) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(usage.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(usage.completion_tokens);
        let total = usage.effective_total_tokens();
        self.total_tokens = self.total_tokens.saturating_add(total);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(usage.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(usage.cache_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(usage.reasoning_tokens);
        self.cache_reported |= usage.cache_reported;
        self.has_usage = true;
        self.estimated |= estimated;
    }

    fn usage(&self) -> Option<Usage> {
        self.has_usage.then_some(Usage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: self.total_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
            reasoning_tokens: self.reasoning_tokens,
            cache_reported: self.cache_reported,
            ..Usage::default()
        })
    }
}

fn queued_prompt_images(prompt: &QueuedPrompt) -> Result<Vec<Option<PastedImage>>> {
    prompt
        .attachments
        .iter()
        .map(|attachment| match attachment {
            QueuedPromptAttachment::Binary { mime, data_base64 } => {
                let data = base64::engine::general_purpose::STANDARD
                    .decode(data_base64)
                    .map_err(|error| anyhow::anyhow!("invalid queued image data: {error}"))?;
                Ok(Some(PastedImage::Binary(ClipboardImage::new(
                    mime.clone(),
                    data,
                ))))
            }
            QueuedPromptAttachment::Path { path } => Ok(Some(PastedImage::Path(path.clone()))),
        })
        .collect()
}

/// The fossilizable prefix of a transient tail: the contiguous run of
/// system-role text messages. Stops at the first non-system or non-text
/// message so redo checkpoints (which append loop messages) never leak
/// assistant/tool content into the fossil record.
/// Marks turn-context blocks that are standing advisories about recent state
/// (not about the current message): only these may be skipped when an
/// identical copy is already visible in a replayed fossil. Producers opt in
/// by using this prefix (reply_processor's long-reply conversion notice).
const STANDING_ADVISORY_PREFIX: &str = "[SystemInfo:";

/// True when `block`'s exact text already appears inside a user-role message
/// of the request being built (a fossilized turn tail replayed from an earlier
/// turn). Stops standing notices from re-fossilizing identical bytes every
/// turn; a block whose content changed no longer matches and is sent again.
fn turn_context_block_visible(messages: &[ChatMessage], block: &str) -> bool {
    messages.iter().any(|message| {
        message.role == "user"
            && matches!(
                message.content.as_ref(),
                Some(ChatContent::Text(text)) if text.contains(block)
            )
    })
}

/// Collects the associative-memory entry lines already visible in the request
/// being built. Fossilized blocks replay as `user` messages whose text starts
/// with the block tag, so matching on that prefix picks up exactly the earlier
/// injections (legacy `system` fossils are re-roled to `user` before this
/// point). Matching whole rendered lines means an updated memory — new content
/// or date — no longer matches and gets injected again.
fn visible_association_lines(messages: &[ChatMessage]) -> HashSet<&str> {
    let mut seen = HashSet::new();
    for message in messages {
        if message.role != "user" {
            continue;
        }
        let Some(ChatContent::Text(text)) = message.content.as_ref() else {
            continue;
        };
        if !text.starts_with("<associative-memory") {
            continue;
        }
        for line in text.lines() {
            if line.starts_with("- [") {
                seen.insert(line.trim_end());
            }
        }
    }
    seen
}

fn fossil_context_messages(tail: &[ChatMessage]) -> Vec<ChatMessage> {
    // Keyed on the explicit marker rather than the role: these blocks now ride
    // as `user` messages (see `ChatMessage::turn_context`), which is
    // indistinguishable by role from a real user turn.
    tail.iter()
        .take_while(|message| {
            message.transient_context
                && matches!(message.content.as_ref(), Some(ChatContent::Text(_)))
        })
        .cloned()
        .collect()
}

/// Fossils written before the role change are stored as `system`. Replaying
/// them verbatim would keep re-poisoning the prefix for the rest of the
/// session, so they are re-roled on the way out: one cold start at the upgrade
/// boundary, byte-stable forever after.
fn replay_fossil(message: &ChatMessage) -> ChatMessage {
    if message.role != "system" {
        return message.clone();
    }
    let mut message = message.clone();
    message.role = "user".to_string();
    message.transient_context = true;
    message
}

fn replace_request_mode_context(
    messages: &mut [ChatMessage],
    system_prompt: &str,
    mode: AgentMode,
    platform: bool,
) {
    if let Some(system) = messages.first_mut() {
        *system = ChatMessage::system(system_prompt);
    }
    // Role-agnostic on purpose: the live block is a `user` message now, while
    // fossils written before the change are still `system`.
    if let Some(runtime) = messages.iter_mut().rev().find(|message| {
        matches!(
            message.content.as_ref(),
            Some(ChatContent::Text(content)) if content.starts_with("<runtime now=")
        )
    }) {
        *runtime = ChatMessage::turn_context(runtime_context(mode, platform));
    }
}

fn continuation_system_prompt(system_prompt: &str, mode: AgentMode) -> String {
    let mode = match mode {
        AgentMode::Normal => "normal",
        AgentMode::Chat => "chat",
    };
    format!(
        "<mode-update active=\"{mode}\">This supersedes all earlier mode-specific instructions.</mode-update>\n\n{system_prompt}"
    )
}

fn estimate_result_tokens(result: &ChatResult) -> usize {
    let mut tokens = crate::token_estimate::estimate_tokens(&result.content);
    if let Some(reasoning) = &result.reasoning {
        tokens = tokens.saturating_add(crate::token_estimate::estimate_tokens(reasoning));
    }
    for call in &result.tool_calls {
        tokens = tokens.saturating_add(crate::token_estimate::estimate_tokens(&call.function.name));
        tokens = tokens.saturating_add(crate::token_estimate::estimate_tokens(
            &call.function.arguments,
        ));
    }
    tokens.max(1)
}

fn estimate_tool_definition_tokens(definitions: &[crate::llm::ToolDefinition]) -> usize {
    definitions
        .iter()
        .filter_map(|definition| serde_json::to_string(definition).ok())
        .map(|text| crate::token_estimate::estimate_tokens(&text))
        .sum()
}

/// Deterministic footprint extraction at tool-execution time: the only point
/// where tool arguments still exist (completed turns don't persist them).
/// Stub-mode lazy tools wrap real args in an `arguments` shell — unwrap it.
fn tool_call_footprint(name: &str, arguments: &str) -> Option<crate::state::ToolFootprint> {
    let mut args: serde_json::Value = serde_json::from_str(arguments).ok()?;
    if let Some(inner) = args.get("arguments") {
        if inner.is_object() {
            args = inner.clone();
        }
    }
    let mut footprint = crate::state::ToolFootprint::default();
    match name {
        "read_file" => {
            footprint
                .read
                .insert(args.get("path")?.as_str()?.trim().to_string());
        }
        "write_file" | "apply_patch" | "edit_string" => {
            footprint
                .modified
                .insert(args.get("path")?.as_str()?.trim().to_string());
        }
        "remember_fact" => {
            let content = args.get("content")?.as_str()?.trim();
            if content.is_empty() {
                return None;
            }
            let mut label: String = content.chars().take(80).collect();
            if content.chars().count() > 80 {
                label.push('…');
            }
            footprint.memories.insert(label);
        }
        _ => return None,
    }
    Some(footprint)
}

fn extract_persistable_tool_report(tool_name: &str, output: &str) -> Option<String> {
    let field = match tool_name {
        "create_artifact" | "apply_artifact_patch" | "present_artifact" => {
            return compact_artifact_tool_report(tool_name, output)
                .map(|report| wrap_previous_tool_report(tool_name, &report))
        }
        "load_tools" => {
            return compact_loaded_tools_report(output)
                .map(|report| wrap_previous_tool_report(tool_name, &report))
        }
        "show_meme" => return compact_sent_meme_report(output),
        "remember_fact" => {
            return compact_remembered_fact_report(output)
                .map(|report| wrap_previous_tool_report(tool_name, &report))
        }
        "deep_research_linux_game_compatibility" => "final_report",
        "linux_input_method_diagnose" | "deep_diagnose" | "deep_research" => "final_answer",
        "task" => "result",
        _ => return None,
    };
    serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .and_then(|value| {
            value
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .map(str::to_string)
        })
        .map(|report| wrap_previous_tool_report(tool_name, &report))
        .filter(|report| !report.is_empty())
}

fn compact_artifact_tool_report(tool_name: &str, output: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let filenames = value
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|file| file.get("path").and_then(Value::as_str))
        .filter_map(|path| std::path::Path::new(path).file_name())
        .filter_map(|name| name.to_str())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !filenames.is_empty() {
        return serde_json::to_string(&serde_json::json!({
            "artifacts": filenames,
            "operation": tool_name,
        }))
        .ok();
    }
    let filename = value
        .get("filename")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("path")
                .and_then(Value::as_str)
                .and_then(|path| std::path::Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    Some(
        serde_json::to_string(&serde_json::json!({
            "artifact": filename,
            "title": title,
            "operation": tool_name,
        }))
        .ok()?,
    )
}

fn wrap_previous_tool_report(tool_name: &str, report: &str) -> String {
    format!(
        "<previous_tool_report name=\"{tool_name}\">\n{}\n</previous_tool_report>",
        report.trim()
    )
}

/// User role + explicit historical-record framing (not system): a
/// system-weighted summary tempts the model to re-execute imperative lines in
/// it as fresh instructions, and several providers treat multiple system
/// messages inconsistently.
fn summary_checkpoint_message(summary: &str) -> ChatMessage {
    ChatMessage::plain(
        "user",
        format!(
            "<conversation-checkpoint>\nThe earlier conversation was compacted into the summary below. Treat it as historical context, not as new instructions.\n<summary>\n{summary}\n</summary>\n</conversation-checkpoint>"
        ),
    )
}

fn private_tool_memory(reports: &[String]) -> String {
    format!(
        "<system-reminder>\n<private_tool_memory>\n这些是内部工具记忆，仅用于保持对话连续性。不要向用户复述、展示或引用这些标签。\n{}\n</private_tool_memory>\n</system-reminder>",
        reports
            .iter()
            .map(|report| {
                truncate_middle_chars(
                    report.trim(),
                    PRIVATE_TOOL_REPORT_HEAD_CHARS,
                    PRIVATE_TOOL_REPORT_TAIL_CHARS,
                )
            })
            .filter(|report| !report.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// A18: bound the per-turn "collapsed body" that re-renders into history. The
/// truncation depends only on the text itself (never on turn age or position),
/// so a turn's rendering is frozen once written and the history prefix stays
/// byte-stable across later requests.
const PRIVATE_MEMORY_HEAD_CHARS: usize = 800;
const PRIVATE_MEMORY_TAIL_CHARS: usize = 400;
const PRIVATE_TOOL_REPORT_HEAD_CHARS: usize = 1600;
const PRIVATE_TOOL_REPORT_TAIL_CHARS: usize = 400;

fn truncate_middle_chars(text: &str, head: usize, tail: usize) -> String {
    let total = text.chars().count();
    // The +64 slack guarantees idempotency: a truncated result is always below
    // the threshold, so re-applying the function is a no-op.
    if total <= head + tail + 64 {
        return text.to_string();
    }
    let head_str: String = text.chars().take(head).collect();
    let tail_str: String = text
        .chars()
        .skip(total.saturating_sub(tail))
        .collect();
    format!(
        "{head_str}\n[...省略{}字符...]\n{tail_str}",
        total - head - tail
    )
}

fn private_reasoning_memory(reasoning: &str) -> Option<String> {
    (!reasoning.trim().is_empty()).then(|| {
        let reasoning =
            truncate_middle_chars(reasoning, PRIVATE_MEMORY_HEAD_CHARS, PRIVATE_MEMORY_TAIL_CHARS);
        format!(
            "<system-reminder>\n<previous_assistant_reasoning>\n{reasoning}\n</previous_assistant_reasoning>\n这些是上一轮 assistant 已经产生的原始思考内容，用于继续工作；不要向用户复述这些标签。\n</system-reminder>"
        )
    })
}

fn push_assistant_context_messages(
    messages: &mut Vec<ChatMessage>,
    content: &str,
    reasoning: Option<&str>,
    force_assistant_message: bool,
) {
    push_assistant_message_with_reasoning(
        messages,
        content.to_string(),
        reasoning,
        None,
        None,
        force_assistant_message,
    );
}

fn push_assistant_message_with_reasoning(
    messages: &mut Vec<ChatMessage>,
    content: String,
    reasoning: Option<&str>,
    thinking_signature: Option<&str>,
    tool_calls: Option<Vec<ToolCall>>,
    force_assistant_message: bool,
) {
    let has_tool_calls = tool_calls.as_ref().is_some_and(|calls| !calls.is_empty());
    if has_tool_calls {
        // A17: DeepSeek thinking mode requires the `reasoning_content` KEY on
        // assistant tool_calls turns of the live tool loop (an empty string is
        // accepted, a missing key is a 400). Carry it on the assistant message
        // itself; the provider adapter strips it for endpoints that do not
        // understand the field and rebuilds the Anthropic thinking block from
        // the signature where present.
        let mut message = ChatMessage::assistant(content, tool_calls);
        message.reasoning_content = Some(reasoning.unwrap_or_default().to_string());
        message.thinking_signature = thinking_signature.map(str::to_string);
        messages.push(message);
        return;
    }
    if let Some(reasoning) = reasoning.and_then(private_reasoning_memory) {
        messages.push(ChatMessage::turn_context(reasoning));
    }
    if force_assistant_message || !content.trim().is_empty() {
        messages.push(ChatMessage::assistant(content, None));
    }
}

fn turn_context_tokens(turn: &crate::state::Turn) -> usize {
    let mut messages = vec![ChatMessage::plain("user", &turn.user_content)];
    // Fossilized transient tail is replayed with the turn, so count it.
    messages.extend(turn.context_messages.iter().cloned());
    for exchange in &turn.question_exchanges {
        messages.push(ChatMessage::plain(
            "assistant",
            crate::question::assistant_exchange_text(exchange),
        ));
        messages.push(ChatMessage::plain(
            "user",
            crate::question::user_exchange_text(exchange),
        ));
    }
    for followup in &turn.followups {
        push_assistant_context_messages(
            &mut messages,
            followup
                .preceding_assistant_content
                .as_deref()
                .unwrap_or_default(),
            followup.preceding_assistant_reasoning.as_deref(),
            false,
        );
        messages.push(ChatMessage::plain("user", &followup.content));
    }
    push_assistant_context_messages(
        &mut messages,
        &turn.assistant_content,
        turn.assistant_reasoning.as_deref(),
        true,
    );
    if !turn.tool_reports.is_empty() {
        messages.push(ChatMessage::turn_context(private_tool_memory(
            &turn.tool_reports,
        )));
    }
    overflow::estimate_messages_tokens(&messages)
}

fn assistant_replay_content(turn: &crate::state::Turn) -> &str {
    if !turn.assistant_content.trim().is_empty() {
        return &turn.assistant_content;
    }
    turn.assistant_reasoning
        .as_deref()
        .filter(|reasoning| !reasoning.trim().is_empty())
        .unwrap_or(&turn.assistant_content)
}

fn followup_assistant_replay_content(followup: &crate::state::TurnFollowup) -> Option<&str> {
    followup
        .preceding_assistant_content
        .as_deref()
        .filter(|content| !content.trim().is_empty())
        .or_else(|| {
            followup
                .preceding_assistant_reasoning
                .as_deref()
                .filter(|reasoning| !reasoning.trim().is_empty())
        })
}

fn interrupted_turn_replay_messages(agent: &Agent, turn: &crate::state::Turn) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    messages.push(ChatMessage::turn_context(
        "<interrupted-turn-recovery>上一轮回复已中断。以下内容是中断前已经持久化的模型输出和工具进度；不要重新执行已经完成的工具，基于这些内容继续处理当前用户请求。</interrupted-turn-recovery>",
    ));

    // A redo revision only journals the new branch. Preserve the already
    // committed clarification/follow-up prefix from the turn row before
    // replaying the new branch's events.
    let replayed_prompt_ids = turn
        .journal_events
        .iter()
        .filter(|event| event.kind == "queued_prompts_consumed")
        .flat_map(|event| {
            event
                .text_payload
                .as_deref()
                .and_then(|payload| serde_json::from_str::<Vec<String>>(payload).ok())
                .unwrap_or_default()
        })
        .collect::<HashSet<_>>();
    if turn.revision > 0 {
        let prefix_question_count = turn
            .journal_events
            .iter()
            .find(|event| event.kind == "redo_prefix_question_count")
            .and_then(|event| event.text_payload.as_deref())
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or_else(|| {
                let branch_answers = turn
                    .journal_events
                    .iter()
                    .filter(|event| {
                        event.kind == "tool_result"
                            && event.name.as_deref() == Some("ask_question")
                            && event
                                .text_payload
                                .as_deref()
                                .and_then(|payload| serde_json::from_str::<Value>(payload).ok())
                                .and_then(|payload| {
                                    payload
                                        .get("status")
                                        .and_then(Value::as_str)
                                        .map(|status| status == "answered")
                                })
                                .unwrap_or(false)
                    })
                    .count();
                turn.question_exchanges.len().saturating_sub(branch_answers)
            });
        for exchange in turn.question_exchanges.iter().take(prefix_question_count) {
            messages.push(ChatMessage::plain(
                "assistant",
                crate::question::assistant_exchange_text(exchange),
            ));
            messages.push(ChatMessage::plain(
                "user",
                crate::question::user_exchange_text(exchange),
            ));
        }
        for followup in &turn.followups {
            if replayed_prompt_ids.contains(&followup.prompt_id) {
                continue;
            }
            push_assistant_context_messages(
                &mut messages,
                followup
                    .preceding_assistant_content
                    .as_deref()
                    .unwrap_or_default(),
                followup.preceding_assistant_reasoning.as_deref(),
                false,
            );
            messages.push(agent.followup_user_message(followup));
        }
    }

    let mut assistant_text = String::new();
    let mut assistant_reasoning = String::new();
    let mut pending_calls = Vec::<ToolCall>::new();
    let mut open_calls = Vec::<ToolCall>::new();
    let mut progress = HashMap::<String, String>::new();
    let mut command_tail = HashMap::<String, Vec<u8>>::new();

    for event in &turn.journal_events {
        match event.kind.as_str() {
            "assistant_content" => {
                if let Some(text) = &event.text_payload {
                    assistant_text.push_str(text);
                }
            }
            "assistant_reasoning" => {
                if let Some(text) = &event.text_payload {
                    assistant_reasoning.push_str(text);
                }
            }
            "reasoning_reset" => assistant_reasoning.clear(),
            "tool_call" => {
                let Some(call_id) = event.call_id.clone() else {
                    continue;
                };
                let Some(name) = event.name.as_deref() else {
                    continue;
                };
                pending_calls.push(ToolCall {
                    id: call_id,
                    kind: "function".to_string(),
                    function: ToolCallFunction {
                        name: replay_tool_function_name(name),
                        arguments: event.text_payload.clone().unwrap_or_default(),
                    },
                });
            }
            "tool_result" => {
                open_calls.extend(flush_interrupted_assistant(
                    &mut messages,
                    &mut assistant_reasoning,
                    &mut assistant_text,
                    &mut pending_calls,
                ));
                if let Some(call_id) = &event.call_id {
                    let output = event.text_payload.as_deref().unwrap_or_default();
                    messages.push(ChatMessage::tool(call_id, truncate_chars(output, 48_000)));
                    open_calls.retain(|call| call.id != *call_id);
                    progress.remove(call_id);
                    command_tail.remove(call_id);
                }
            }
            "tool_progress" => {
                if let Some(call_id) = &event.call_id {
                    progress.insert(
                        call_id.clone(),
                        truncate_chars(event.text_payload.as_deref().unwrap_or_default(), 4_000),
                    );
                }
            }
            "command_stdout" | "command_stderr" => {
                if let Some(call_id) = &event.call_id {
                    let tail = command_tail.entry(call_id.clone()).or_default();
                    if let Some(bytes) = &event.blob_payload {
                        tail.extend_from_slice(bytes);
                        const MAX_COMMAND_TAIL: usize = 8 * 1024;
                        if tail.len() > MAX_COMMAND_TAIL {
                            let start = tail.len() - MAX_COMMAND_TAIL;
                            tail.drain(..start);
                        }
                    }
                }
            }
            "queued_prompts_consumed" => {
                open_calls.extend(flush_interrupted_assistant(
                    &mut messages,
                    &mut assistant_reasoning,
                    &mut assistant_text,
                    &mut pending_calls,
                ));
                append_interrupted_tool_results(
                    &mut messages,
                    &mut open_calls,
                    &mut progress,
                    &mut command_tail,
                );
                let prompt_ids = event
                    .text_payload
                    .as_deref()
                    .and_then(|payload| serde_json::from_str::<Vec<String>>(payload).ok())
                    .unwrap_or_default();
                for prompt_id in prompt_ids {
                    if let Some(followup) = turn
                        .followups
                        .iter()
                        .find(|followup| followup.prompt_id == prompt_id)
                    {
                        messages.push(agent.followup_user_message(followup));
                    }
                }
            }
            _ => {}
        }
    }

    open_calls.extend(flush_interrupted_assistant(
        &mut messages,
        &mut assistant_reasoning,
        &mut assistant_text,
        &mut pending_calls,
    ));
    append_interrupted_tool_results(
        &mut messages,
        &mut open_calls,
        &mut progress,
        &mut command_tail,
    );
    messages
}

fn flush_interrupted_assistant(
    messages: &mut Vec<ChatMessage>,
    assistant_reasoning: &mut String,
    assistant_text: &mut String,
    pending_calls: &mut Vec<ToolCall>,
) -> Vec<ToolCall> {
    if assistant_reasoning.trim().is_empty()
        && assistant_text.trim().is_empty()
        && pending_calls.is_empty()
    {
        return Vec::new();
    }
    if !assistant_reasoning.trim().is_empty() {
        if let Some(reasoning) = private_reasoning_memory(assistant_reasoning) {
            messages.push(ChatMessage::turn_context(reasoning));
        }
    }
    assistant_reasoning.clear();
    let text = std::mem::take(assistant_text);
    let calls = std::mem::take(pending_calls);
    let replay_calls = (!calls.is_empty()).then(|| calls.clone());
    messages.push(ChatMessage::assistant(text, replay_calls));
    calls
}

fn append_interrupted_tool_results(
    messages: &mut Vec<ChatMessage>,
    open_calls: &mut Vec<ToolCall>,
    progress: &mut HashMap<String, String>,
    command_tail: &mut HashMap<String, Vec<u8>>,
) {
    for call in std::mem::take(open_calls) {
        let mut output =
            "tool execution was interrupted before a final result was persisted".to_string();
        if let Some(message) = progress.remove(&call.id) {
            output.push_str("\nlast progress: ");
            output.push_str(&message);
        }
        if let Some(bytes) = command_tail.remove(&call.id) {
            let tail = String::from_utf8_lossy(&bytes);
            if !tail.trim().is_empty() {
                output.push_str("\nlast command output:\n");
                output.push_str(&truncate_chars(&tail, 8_000));
            }
        }
        messages.push(ChatMessage::tool(call.id, output));
    }
}

fn replay_tool_function_name(name: &str) -> String {
    match name.split_once(':').map(|(prefix, _)| prefix) {
        Some("load_skill") | Some("load_tools") | Some("task") => {
            name.split(':').next().unwrap_or(name).to_string()
        }
        _ => name.to_string(),
    }
}

fn redo_checkpoint_payload(
    messages: &[ChatMessage],
    replay_start: usize,
    base_tool_reports: &[String],
    pending_tool_reports: &[(String, String)],
    tool_rounds: usize,
    question_rounds: usize,
) -> TurnRedoCheckpointPayload {
    let mut prefix_tool_reports = Vec::with_capacity(
        base_tool_reports
            .len()
            .saturating_add(pending_tool_reports.len()),
    );
    prefix_tool_reports.extend(base_tool_reports.iter().cloned());
    prefix_tool_reports.extend(
        pending_tool_reports
            .iter()
            .map(|(_, report)| report.clone()),
    );
    TurnRedoCheckpointPayload {
        replay_messages: messages.get(replay_start..).unwrap_or_default().to_vec(),
        prefix_tool_reports,
        tool_rounds,
        question_rounds,
        loaded_items: Vec::new(),
        prefix_question_count: 0,
        prefix_image_asset_ids: Vec::new(),
        prefix_artifact_asset_ids: Vec::new(),
    }
}

fn evicted_turn_entries(
    turns: &[crate::state::Turn],
) -> (Vec<crate::state::StoredConversationEntry>, Vec<EvictedTurn>) {
    let mut entries = Vec::new();
    let mut evicted = Vec::new();
    for turn in turns {
        entries.push(crate::state::StoredConversationEntry {
            timestamp: turn.user_timestamp.clone(),
            role: "user".to_string(),
            content: turn.user_content.clone(),
            reasoning: None,
        });
        evicted.push(EvictedTurn {
            source_id: format!("{}:user", turn.turn_id),
            timestamp: turn.user_timestamp.clone(),
            role: "user".to_string(),
            content: turn.user_content.clone(),
            ..EvictedTurn::default()
        });

        for (index, exchange) in turn.question_exchanges.iter().enumerate() {
            let timestamp = exchange.answered_at.clone();
            let assistant_content = crate::question::assistant_exchange_text(exchange);
            entries.push(crate::state::StoredConversationEntry {
                timestamp: timestamp.clone(),
                role: "assistant_clarification".to_string(),
                content: assistant_content.clone(),
                reasoning: None,
            });
            evicted.push(EvictedTurn {
                source_id: format!("{}:question:{index}", turn.turn_id),
                timestamp: timestamp.clone(),
                role: "assistant".to_string(),
                content: assistant_content,
                ..EvictedTurn::default()
            });
            let user_content = crate::question::user_exchange_text(exchange);
            entries.push(crate::state::StoredConversationEntry {
                timestamp: timestamp.clone(),
                role: "user_clarification".to_string(),
                content: user_content.clone(),
                reasoning: None,
            });
            evicted.push(EvictedTurn {
                source_id: format!("{}:answer:{index}", turn.turn_id),
                timestamp,
                role: "user".to_string(),
                content: user_content,
                ..EvictedTurn::default()
            });
        }

        for followup in &turn.followups {
            if followup_assistant_replay_content(followup).is_some() {
                let content = followup
                    .preceding_assistant_content
                    .clone()
                    .unwrap_or_default();
                entries.push(crate::state::StoredConversationEntry {
                    timestamp: followup.submitted_at.clone(),
                    role: "assistant".to_string(),
                    content: content.clone(),
                    reasoning: followup.preceding_assistant_reasoning.clone(),
                });
                evicted.push(EvictedTurn {
                    source_id: format!("{}:before:{}", turn.turn_id, followup.prompt_id),
                    timestamp: followup.submitted_at.clone(),
                    role: "assistant".to_string(),
                    content,
                    ..EvictedTurn::default()
                });
            }
            entries.push(crate::state::StoredConversationEntry {
                timestamp: followup.submitted_at.clone(),
                role: "user".to_string(),
                content: followup.content.clone(),
                reasoning: None,
            });
            evicted.push(EvictedTurn {
                source_id: format!("{}:followup:{}", turn.turn_id, followup.prompt_id),
                timestamp: followup.submitted_at.clone(),
                role: "user".to_string(),
                content: followup.content.clone(),
                ..EvictedTurn::default()
            });
        }

        let timestamp = turn.assistant_timestamp.clone().unwrap_or_default();
        entries.push(crate::state::StoredConversationEntry {
            timestamp: timestamp.clone(),
            role: "assistant".to_string(),
            content: turn.assistant_content.clone(),
            reasoning: turn.assistant_reasoning.clone(),
        });
        evicted.push(EvictedTurn {
            source_id: format!("{}:assistant", turn.turn_id),
            timestamp: timestamp.clone(),
            role: "assistant".to_string(),
            content: turn.assistant_content.clone(),
            ..EvictedTurn::default()
        });

        for (index, report) in turn.tool_reports.iter().enumerate() {
            entries.push(crate::state::StoredConversationEntry {
                timestamp: timestamp.clone(),
                role: "assistant".to_string(),
                content: report.clone(),
                reasoning: None,
            });
            evicted.push(EvictedTurn {
                source_id: format!("{}:tool:{index}", turn.turn_id),
                timestamp: timestamp.clone(),
                role: "assistant".to_string(),
                content: report.clone(),
                ..EvictedTurn::default()
            });
        }
    }
    (entries, evicted)
}

pub(crate) fn archive_and_delete_visible_turns(
    state: &StateStore,
    memory: &MemoryStore,
    turns: &[crate::state::Turn],
) -> Result<Vec<crate::state::StoredConversationEntry>> {
    archive_and_delete_visible_turns_checked(state, memory, turns, None)
}

fn archive_and_delete_visible_turns_checked(
    state: &StateStore,
    memory: &MemoryStore,
    turns: &[crate::state::Turn],
    expected_loaded_tools: Option<&[(String, Option<String>)]>,
) -> Result<Vec<crate::state::StoredConversationEntry>> {
    let (entries, mut evicted) = evicted_turn_entries(turns);
    memory.apply_evicted_ownership(&mut evicted);
    let turn_ids = turns
        .iter()
        .map(|turn| turn.turn_id.clone())
        .collect::<Vec<_>>();
    if let Some(archive_db) = memory.prepare_evicted_context_db()? {
        state.archive_and_delete_visible_turns(
            &archive_db,
            &evicted,
            &turn_ids,
            expected_loaded_tools,
        )?;
    } else if expected_loaded_tools.is_some() {
        state.delete_visible_turns_checked(&turn_ids, expected_loaded_tools)?;
    } else {
        state.delete_visible_turns(&turn_ids)?;
    }
    Ok(entries)
}

fn compact_remembered_fact_report(output: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let content = value.get("content").and_then(Value::as_str)?.trim();
    if content.is_empty() {
        return None;
    }
    let mut report = serde_json::json!({
        "remembered_fact": {
            "content": content,
        }
    });
    if let Some(id) = value.get("id").and_then(Value::as_i64) {
        report["remembered_fact"]["id"] = serde_json::json!(id);
    }
    if let Some(source) = value.get("source").and_then(Value::as_str) {
        let source = source.trim();
        if !source.is_empty() {
            report["remembered_fact"]["source"] = serde_json::json!(source);
        }
    }
    serde_json::to_string(&report).ok()
}

fn compact_loaded_tools_report(output: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    let names = value
        .get("loaded_tools")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|item| {
            item.as_str()
                .or_else(|| item.get("name").and_then(Value::as_str))
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    if names.is_empty() {
        return None;
    }
    serde_json::to_string(&serde_json::json!({ "loaded_tools": names })).ok()
}

#[derive(Default)]
struct LoadedItems {
    targets: Vec<String>,
    tools: Vec<String>,
}

fn loaded_items_from_output(output: &str) -> LoadedItems {
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return LoadedItems::default();
    };
    let targets = value
        .get("loaded_targets")
        .and_then(Value::as_array)
        .map(|items| string_array_items(items))
        .unwrap_or_default();
    let tools = value
        .get("loaded_tools")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .or_else(|| item.get("name").and_then(Value::as_str))
                })
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    LoadedItems { targets, tools }
}

fn string_array_items(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

fn compact_sent_meme_report(output: &str) -> Option<String> {
    const MAX_DESCRIPTION_CHARS: usize = 120;

    let value = serde_json::from_str::<Value>(output).ok()?;
    if value.get("success").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let id = value.get("id").and_then(Value::as_str)?.trim();
    if id.is_empty() {
        return None;
    }
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .map(compact_one_line)
        .filter(|description| !description.is_empty())
        .map(|description| truncate_chars(&description, MAX_DESCRIPTION_CHARS));
    let id = xml_text_escape(id);
    match description {
        Some(description) => Some(format!(
            "<sent_meme>发送了一个表情包：id={}；description={}</sent_meme>",
            id,
            xml_text_escape(&description)
        )),
        None => Some(format!("<sent_meme>发送了一个表情包：id={id}</sent_meme>")),
    }
}

fn compact_one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            output.push('…');
            return output;
        }
        output.push(ch);
    }
    output
}

fn xml_text_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn loaded_tools_from_messages(messages: &[ChatMessage]) -> BTreeSet<String> {
    let mut loaded = BTreeSet::new();
    for message in messages {
        let Some(ChatContent::Text(text)) = message.content.as_ref() else {
            continue;
        };
        collect_loaded_tools_from_text(text, &mut loaded);
    }
    loaded
}

fn collect_loaded_tools_from_text(text: &str, loaded: &mut BTreeSet<String>) {
    let mut rest = text;
    let start_tag = "<previous_tool_report name=\"load_tools\">";
    let end_tag = "</previous_tool_report>";
    while let Some(start) = rest.find(start_tag) {
        let body_start = start + start_tag.len();
        let Some(end) = rest[body_start..].find(end_tag) else {
            break;
        };
        let body = &rest[body_start..body_start + end];
        if let Ok(value) = serde_json::from_str::<Value>(body.trim()) {
            if let Some(names) = value.get("loaded_tools").and_then(Value::as_array) {
                for name in names.iter().filter_map(Value::as_str) {
                    if !name.trim().is_empty() {
                        loaded.insert(name.trim().to_string());
                    }
                }
            }
        }
        rest = &rest[body_start + end + end_tag.len()..];
    }
}

fn tool_event_name(name: &str, arguments: &str) -> String {
    let Ok(args) = serde_json::from_str::<Value>(arguments) else {
        return name.to_string();
    };
    match name {
        "load_skill" => args
            .get("name")
            .and_then(Value::as_str)
            .map(|skill| format!("load_skill:{skill}"))
            .unwrap_or_else(|| name.to_string()),
        "load_tools" => args
            .get("names")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .filter(|tools| !tools.is_empty())
            .map(|tools| format!("load_tools:{tools}"))
            .unwrap_or_else(|| name.to_string()),
        // Each subagent gets a distinct event name so concurrent task calls
        // render as separate status lines instead of one aggregated counter.
        "task" => args
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(|description| {
                let truncated: String = description.chars().take(32).collect();
                format!("task:{truncated}")
            })
            .unwrap_or_else(|| name.to_string()),
        _ => name.to_string(),
    }
}

fn clipboard_binary_image_from_tool_result(
    tool_name: &str,
    output: &str,
) -> Option<ClipboardImage> {
    if tool_name != "read_clipboard" {
        return None;
    }
    let value = serde_json::from_str::<Value>(output).ok()?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    if value.get("kind").and_then(Value::as_str) != Some("clipboard") {
        return None;
    }
    if value.get("content_type").and_then(Value::as_str) != Some("image") {
        return None;
    }
    if value.get("source").and_then(Value::as_str) != Some("clipboard_binary") {
        return None;
    }
    let path = value.get("path").and_then(Value::as_str)?;
    let mime = value
        .get("mime")
        .and_then(Value::as_str)
        .unwrap_or("image/png")
        .to_string();
    let data = std::fs::read(path).ok()?;
    Some(ClipboardImage::new(mime, data))
}

fn resolve_pasted_image_paths(
    images: &[Option<PastedImage>],
    paths: &MiyuPaths,
    image_platform: Option<&str>,
) -> Vec<Option<String>> {
    images
        .iter()
        .enumerate()
        .map(|(i, image)| match image {
            Some(PastedImage::Binary(img)) => image_platform
                .map(|platform| {
                    img.write_cache_file(
                        &paths.cache_dir,
                        &PathBuf::from("platform_images").join(platform),
                    )
                })
                .unwrap_or_else(|| img.write_temp_file(&paths.cache_dir, i + 1))
                .ok()
                .map(|path| path.display().to_string()),
            Some(PastedImage::Path(path)) => Some(path.clone()),
            None => None,
        })
        .collect()
}

fn rewrite_image_placeholders_with_paths(input: &str, paths: &[Option<String>]) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("[Image ") {
        output.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let Some(end) = after_start.find(']') else {
            output.push_str(after_start);
            return output;
        };
        let placeholder = &after_start[..=end];
        if let Some(index) = image_placeholder_index(placeholder) {
            if let Some(Some(path)) = paths.get(index - 1) {
                output.push_str(&format!("[Image {index}: {path}]"));
            } else {
                output.push_str(placeholder);
            }
        } else {
            output.push_str(placeholder);
        }
        rest = &after_start[end + 1..];
    }
    output.push_str(rest);
    output
}

fn image_placeholder_index(placeholder: &str) -> Option<usize> {
    let inner = placeholder
        .strip_prefix("[Image ")?
        .strip_suffix(']')?
        .trim_start();
    let num: String = inner.chars().take_while(|c| c.is_ascii_digit()).collect();
    let index = num.parse::<usize>().ok()?;
    (index > 0).then_some(index)
}

fn vision_analysis_progress(tick: usize) -> String {
    let dots = match tick % 3 {
        1 => ".",
        2 => "..",
        _ => "...",
    };
    if crate::i18n::is_zh() {
        format!("视觉分析{dots}")
    } else {
        format!("Vision analysis{dots}")
    }
}

fn with_mode_reminder(system_prompt: String, mode: AgentMode) -> String {
    let mut prompt = system_prompt;
    if let Some(reminder) = mode.reminder() {
        prompt.push_str("\n\n");
        prompt.push_str(reminder);
    }
    prompt
}

fn with_runtime_system_context(mut system_prompt: String, context: &[String]) -> String {
    for item in context
        .iter()
        .map(String::as_str)
        .filter(|item| !item.is_empty())
    {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(item);
    }
    system_prompt
}

/// Appends the static host block to the stable prefix.
///
/// It belongs here rather than in the per-turn `<runtime …/>` tail: the tail is
/// fossilized into `turns.context_messages` and replayed byte-for-byte by every
/// later turn, so a process-constant put there is re-sent once per turn and
/// piles up in the request; in the system prompt it is paid once and then
/// served from the provider's prefix cache.
///
/// Only owner sessions get it. A QQ reply has no use for kernel versions, and
/// skipping the append outright — rather than adding an empty block — keeps
/// those sessions' system prompt byte-identical to what the provider already
/// has cached, so the platform side sees no cold start at all.
fn with_host_environment(
    mut system_prompt: String,
    audience: PromptAudience,
    paths: &MiyuPaths,
) -> String {
    if audience != PromptAudience::Owner {
        return system_prompt;
    }
    system_prompt.push_str("\n\n");
    system_prompt.push_str(&crate::host_info::host_environment_block(&paths.root_dir));
    // 渲染能力说明(仅 owner 会话):终端与 WebUI 都支持 LaTeX。
    // 不放人格提示词里——QQ 等平台的排版能力不同,不该看到这段。
    system_prompt.push_str(
        "\n\n输出数学公式时使用 LaTeX:重要公式用块级定界符(`$$…$$` 或 `\\[…\\]`,独立成段)会渲染成排版图;行内用 `$…$` 或 `\\(…\\)`,会转写为 Unicode 数学文本;表格单元格内的公式同样支持,分式会排成上下结构。不要用裸 Unicode 或 ASCII 手拼公式。",
    );
    system_prompt
}

fn active_text_pool_supports_vision(config: &AppConfig) -> bool {
    let choices = config.active_provider_model_choices();
    !choices.is_empty()
        && choices.iter().all(|choice| {
            config.model_supports_any_input(&choice.provider_id, &choice.model, &["image"])
        })
}

fn should_use_active_text_pool_for_images(config: &AppConfig) -> bool {
    config.plugins.vision.prefer_current_multimodal_model
        && active_text_pool_supports_vision(config)
}

#[derive(Default)]
struct ReasoningTitleFilter {
    pending: String,
    decided: bool,
    trim_body_prefix: bool,
}

impl ReasoningTitleFilter {
    fn push(&mut self, text: &str) -> (Option<String>, Option<String>) {
        if self.decided {
            let text = if self.trim_body_prefix {
                let text = text.trim_start_matches(['\r', '\n']);
                if text.is_empty() {
                    return (None, None);
                }
                self.trim_body_prefix = false;
                text
            } else {
                text
            };
            return (None, (!text.is_empty()).then(|| text.to_string()));
        }
        self.pending.push_str(text);
        let trimmed = self.pending.trim_start();
        if "**".starts_with(trimmed) {
            return (None, None);
        }
        if let Some(body) = trimmed.strip_prefix("**") {
            let Some(close) = body.find("**") else {
                if trimmed.chars().count() <= 160 {
                    return (None, None);
                }
                return self.release_without_title();
            };
            let title = clean_reasoning_title(&body[..close]);
            let suffix = &body[close + 2..];
            if only_line_breaks(suffix) {
                return self.finish_decision(title, String::new());
            }
            if !suffix.starts_with("\n\n") && !suffix.starts_with("\r\n\r\n") {
                return self.release_without_title();
            }
            let rest = suffix.trim_start_matches(['\r', '\n']).to_string();
            return self.finish_decision(title, rest);
        }
        if possible_markdown_heading_prefix(trimmed) {
            return (None, None);
        }
        if let Some(title_start) = markdown_heading_content_start(trimmed) {
            let Some(end) = trimmed.find('\n') else {
                if trimmed.chars().count() <= 160 {
                    return (None, None);
                }
                return self.release_without_title();
            };
            let suffix = &trimmed[end + 1..];
            if only_line_breaks(suffix) {
                return (None, None);
            }
            let title = clean_reasoning_title(&trimmed[title_start..end]);
            let rest = suffix.trim_start_matches(['\r', '\n']).to_string();
            return self.finish_decision(title, rest);
        }
        self.release_without_title()
    }

    fn finish_decision(&mut self, title: String, rest: String) -> (Option<String>, Option<String>) {
        self.pending.clear();
        self.decided = true;
        self.trim_body_prefix = rest.is_empty();
        (
            (!title.is_empty()).then_some(title),
            (!rest.is_empty()).then_some(rest),
        )
    }

    fn release_without_title(&mut self) -> (Option<String>, Option<String>) {
        self.decided = true;
        (None, Some(std::mem::take(&mut self.pending)))
    }

    fn finish(&mut self) -> (Option<String>, Option<String>) {
        if self.pending.is_empty() {
            return (None, None);
        }
        self.decided = true;
        let pending = std::mem::take(&mut self.pending);
        let trimmed = pending.trim_start();
        if let Some(body) = trimmed.strip_prefix("**") {
            if let Some(close) = body.find("**") {
                let suffix = &body[close + 2..];
                if suffix.is_empty()
                    || ((suffix.starts_with("\n\n") || suffix.starts_with("\r\n\r\n"))
                        && only_line_breaks(suffix))
                {
                    let title = clean_reasoning_title(&body[..close]);
                    return ((!title.is_empty()).then_some(title), None);
                }
            }
        }
        if let Some(title_start) = markdown_heading_content_start(trimmed) {
            let title = clean_reasoning_title(&trimmed[title_start..]);
            return ((!title.is_empty()).then_some(title), None);
        }
        (None, Some(trimmed.to_string()))
    }
}

fn possible_markdown_heading_prefix(text: &str) -> bool {
    !text.is_empty() && text.len() <= 6 && text.bytes().all(|byte| byte == b'#')
}

fn only_line_breaks(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b'\r' | b'\n'))
}

fn markdown_heading_content_start(text: &str) -> Option<usize> {
    let hashes = text.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = text.get(hashes..)?;
    let whitespace = rest
        .bytes()
        .take_while(|byte| matches!(*byte, b' ' | b'\t'))
        .count();
    (whitespace > 0).then_some(hashes + whitespace)
}

fn clean_reasoning_title(value: &str) -> String {
    let value = compact_one_line(value);
    let value = value.trim_matches(['*', '#', ' ', '\t', '.', '。', '!', '！', '?', '？']);
    truncate_chars(value, 80)
}

fn emit_filtered_chunk_at<F>(
    chunk: ChatStreamChunk,
    received_at: Instant,
    filter: &mut ReasoningTitleFilter,
    on_event: &mut F,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    match chunk.kind {
        ChatStreamKind::ReasoningPartStart => {
            *filter = ReasoningTitleFilter::default();
            on_event(AgentEvent::ReasoningPartStart { received_at })?;
        }
        ChatStreamKind::ReasoningReset => {
            *filter = ReasoningTitleFilter::default();
            on_event(AgentEvent::ReasoningReset { received_at })?;
        }
        ChatStreamKind::ReasoningPartEnd => {
            let (title, text) = filter.finish();
            if let Some(title) = title {
                on_event(AgentEvent::ReasoningTitle(title))?;
            }
            if let Some(text) = text {
                on_event(AgentEvent::Chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Reasoning,
                    text,
                }))?;
            }
            on_event(AgentEvent::ReasoningPartEnd { received_at })?;
        }
        ChatStreamKind::ToolCall => {
            // The chunk carries only the tool name, emitted the moment it is
            // decoded — the arguments are still streaming behind it. That is
            // exactly the window a long patch or file write spends looking
            // frozen, so the hint goes up here rather than at ToolCall.
            if crate::tools::preparing_phase(&chunk.text).is_some() {
                on_event(AgentEvent::ToolPreparing {
                    name: chunk.text.clone(),
                })?;
            }
            on_event(AgentEvent::Chunk(chunk))?;
        }
        ChatStreamKind::Reasoning => {
            let (title, text) = filter.push(&chunk.text);
            if let Some(title) = title {
                on_event(AgentEvent::ReasoningTitle(title))?;
            }
            if let Some(text) = text {
                on_event(AgentEvent::Chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Reasoning,
                    text,
                }))?;
            }
        }
        _ => on_event(AgentEvent::Chunk(chunk))?,
    }
    Ok(())
}

fn emit_model_chunk_at<F>(
    chunk: ChatStreamChunk,
    received_at: Instant,
    filter: &mut ReasoningTitleFilter,
    on_event: &mut F,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    if chunk.kind == ChatStreamKind::Reasoning {
        on_event(AgentEvent::RawReasoning(chunk.clone()))?;
    }
    emit_filtered_chunk_at(chunk, received_at, filter, on_event)
}

#[cfg(test)]
fn emit_filtered_chunk<F>(
    chunk: ChatStreamChunk,
    filter: &mut ReasoningTitleFilter,
    on_event: &mut F,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    emit_filtered_chunk_at(chunk, Instant::now(), filter, on_event)
}

#[cfg(test)]
fn parse_reasoning_title(reasoning: &str) -> (Option<String>, String) {
    parse_reasoning_title_chunks([reasoning])
}

#[cfg(test)]
fn parse_reasoning_title_chunks<'a>(
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

/// The transient runtime stamp that rides the turn tail.
///
/// `platform` strips everything a chat message cannot use. A QQ turn has no
/// working directory, no shell and no terminal — those attributes were pure
/// scaffolding there, and they were re-sent at full price on every single
/// turn (285 chars against a ~45-char timestamp).
fn runtime_context(mode: AgentMode, platform: bool) -> String {
    if platform {
        return format!(
            "<runtime now=\"{}\"/>",
            Local::now().format("%Y年%m月%d日 %A %H:%M")
        );
    }
    let cwd = crate::tools::workspace::effective_workdir()
        .display()
        .to_string();
    if mode == AgentMode::Chat {
        format!(
            "<runtime now=\"{}\" cwd=\"{}\" note=\"cwd is workspace context only; do not infer assistant identity from paths or project names\"/>",
            Local::now().format("%Y年%m月%d日 %A %H:%M"),
            xml_attr_escape(&cwd),
        )
    } else {
        let runtime = terminal_runtime_context();
        format!(
            "<runtime now=\"{}\" cwd=\"{}\" note=\"cwd is workspace context only; do not infer assistant identity from paths or project names\" {runtime}/>",
            Local::now().format("%Y年%m月%d日 %A %H:%M"),
            xml_attr_escape(&cwd),
        )
    }
}

fn terminal_runtime_context() -> String {
    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    let stderr_tty = std::io::stderr().is_terminal();
    let environment = if stdin_tty || stdout_tty || stderr_tty {
        if crate::i18n::agent_is_zh() {
            "终端会话"
        } else {
            "terminal session"
        }
    } else if crate::i18n::agent_is_zh() {
        "非交互或管道环境"
    } else {
        "non-interactive or piped environment"
    };
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let mut terminal_parts = Vec::new();
    for key in ["TERM_PROGRAM", "TERM", "COLORTERM"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                terminal_parts.push(format!("{key}={value}"));
            }
        }
    }
    let terminal = if terminal_parts.is_empty() {
        "unknown".to_string()
    } else {
        terminal_parts.join(", ")
    };
    format!(
        "env=\"{}\" shell=\"{}\" terminal=\"{}\"",
        xml_attr_escape(environment),
        xml_attr_escape(&shell),
        xml_attr_escape(&terminal)
    )
}

fn clean_user_visible_text(input: &str) -> String {
    let mut output = input.to_string();
    for tag in ["system-reminder", "system_reminder"] {
        output = strip_tagged_sections(output, tag);
    }
    output
}

fn strip_tagged_sections(mut text: String, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    while let Some(start) = text.find(&open) {
        let Some(relative_end) = text[start..].find(&close) else {
            text.replace_range(start.., "");
            break;
        };
        let end = start + relative_end + close.len();
        text.replace_range(start..end, "");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ActiveProviderModelConfig, AppConfig, ProviderConfig};
    use crate::paths::MiyuPaths;
    use crate::platforms::{
        ConversationKind, OutboundMessage, PlatformAdapter, PlatformConversation, SendReceipt,
    };
    use crate::tools::{empty_parameters, ToolSpec};
    use futures_util::future::BoxFuture;
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    struct NoopPlatformAdapter;

    /// 闲聊模式比普通模式更紧:连 Presentation 都不放行,只留只读。
    #[test]
    fn chat_mode_allows_read_only_tools_and_nothing_else() {
        assert!(mode_allows_tool_permission(
            AgentMode::Chat,
            ToolPermission::ReadOnly
        ));
        assert!(!mode_allows_tool_permission(
            AgentMode::Chat,
            ToolPermission::Presentation
        ));
        assert!(!mode_allows_tool_permission(
            AgentMode::Chat,
            ToolPermission::Writes
        ));
        assert!(mode_allows_tool_permission(
            AgentMode::Normal,
            ToolPermission::Writes
        ));
    }

    #[test]
    fn artifact_delivery_detection_is_conservative() {
        assert!(artifact_delivery_requested(&[ChatMessage::plain(
            "user",
            "生成一个 Linux 游玩报告，保存为 Markdown 文件",
        )]));
        assert!(artifact_delivery_requested(&[ChatMessage::plain(
            "user",
            "create a standalone HTML file",
        )]));
        assert!(!artifact_delivery_requested(&[ChatMessage::plain(
            "user",
            "修改 src/main.rs 修复这个错误",
        )]));
    }

    #[test]
    fn artifact_candidates_only_include_new_files() {
        let created = artifact_candidate_paths(
            "write_file",
            r#"{"ok":true,"created":true,"path":"report.md"}"#,
        );
        assert_eq!(created.len(), 1);
        assert!(artifact_candidate_paths(
            "write_file",
            r#"{"ok":true,"created":false,"path":"src/main.rs"}"#,
        )
        .is_empty());
        assert!(artifact_candidate_paths(
            "apply_patch",
            r#"{"ok":true,"files":[{"path":"report.md","operation":"update"}]}"#,
        )
        .is_empty());
    }

    #[test]
    fn tool_call_stream_announces_preparation_for_slow_argument_tools() {
        let mut filter = ReasoningTitleFilter::default();
        let mut prepared = Vec::new();
        let mut streamed = Vec::new();
        let mut on_event = |event| {
            match event {
                AgentEvent::ToolPreparing { name } => prepared.push(name),
                AgentEvent::Chunk(chunk) if chunk.kind == ChatStreamKind::ToolCall => {
                    streamed.push(chunk.text)
                }
                _ => {}
            }
            Ok(())
        };
        let names = [
            "apply_patch",
            "apply_artifact_patch",
            "write_file",
            "edit_string",
            "run_command",
            "task",
            "ask_question",
            // Arguments arrive in one chunk: a hint here would only flicker.
            "read_file",
        ];
        for name in names {
            emit_filtered_chunk(
                ChatStreamChunk {
                    kind: ChatStreamKind::ToolCall,
                    text: name.to_string(),
                },
                &mut filter,
                &mut on_event,
            )
            .unwrap();
        }
        assert_eq!(
            prepared,
            [
                "apply_patch",
                "apply_artifact_patch",
                "write_file",
                "edit_string",
                "run_command",
                "task",
                "ask_question"
            ]
        );
        assert_eq!(streamed, names);
    }

    #[test]
    fn artifact_tool_report_keeps_cross_turn_filename_memory() {
        let report = extract_persistable_tool_report(
            "apply_artifact_patch",
            r#"{"ok":true,"files":[{"path":"report.md","operation":"update"}]}"#,
        )
        .unwrap();
        assert!(report.contains("report.md"));
        assert!(!report.contains("/home/test"));
    }

    impl PlatformAdapter for NoopPlatformAdapter {
        fn send<'a>(&'a self, _message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
            Box::pin(async { bail!("send is not used in this test") })
        }

        fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
            Box::pin(async { Ok("Miyu".to_string()) })
        }
    }

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
    fn formats_dynamic_load_tool_names() {
        assert_eq!(
            tool_event_name("load_skill", r#"{"name":"web-search"}"#),
            "load_skill:web-search"
        );
        assert_eq!(
            tool_event_name("load_tools", r#"{"names":["get_weather","todoupdate"]}"#),
            "load_tools:get_weather,todoupdate"
        );
    }

    #[test]
    fn restores_loaded_tools_from_previous_tool_report() {
        let messages = vec![ChatMessage::plain(
            "assistant",
            "<previous_tool_report name=\"load_tools\">\n{\"loaded_tools\":[\"get_weather\",\"todoupdate\"]}\n</previous_tool_report>",
        )];
        let loaded = loaded_tools_from_messages(&messages);
        assert!(loaded.contains("get_weather"));
        assert!(loaded.contains("todoupdate"));
    }

    #[test]
    fn persists_loaded_tools_with_previous_tool_report_wrapper() {
        let output = serde_json::json!({
            "loaded_tools": [
                {"name": "get_weather"},
                {"name": "todoupdate"}
            ]
        })
        .to_string();

        assert_eq!(
            extract_persistable_tool_report("load_tools", &output).as_deref(),
            Some("<previous_tool_report name=\"load_tools\">\n{\"loaded_tools\":[\"get_weather\",\"todoupdate\"]}\n</previous_tool_report>")
        );
    }

    #[test]
    fn tool_footprint_extracts_paths_and_memories() {
        let fp = tool_call_footprint("read_file", r#"{"path":"/tmp/a.txt"}"#).unwrap();
        assert!(fp.read.contains("/tmp/a.txt"));
        let fp = tool_call_footprint("edit_string", r#"{"path":"b.rs","old_string":"x","new_string":"y"}"#).unwrap();
        assert!(fp.modified.contains("b.rs"));
        // stub-mode wrapped arguments unwrap
        let fp = tool_call_footprint("write_file", r#"{"arguments":{"path":"c.md","content":"hi"}}"#).unwrap();
        assert!(fp.modified.contains("c.md"));
        let fp = tool_call_footprint("remember_fact", r#"{"content":"用户住在杭州"}"#).unwrap();
        assert!(fp.memories.contains("用户住在杭州"));
        assert!(tool_call_footprint("bash", r#"{"command":"ls"}"#).is_none());
    }

    #[test]
    fn persists_compact_sent_meme_report() {
        let output = serde_json::json!({
            "success": true,
            "id": "sha256:abc123",
            "description": "猫猫\n开心 & <得意>",
            "unused": "ignored",
        })
        .to_string();

        assert_eq!(
            extract_persistable_tool_report("show_meme", &output).as_deref(),
            Some("<sent_meme>发送了一个表情包：id=sha256:abc123；description=猫猫 开心 &amp; &lt;得意&gt;</sent_meme>")
        );
    }

    #[test]
    fn sent_meme_report_allows_missing_description() {
        let output = serde_json::json!({
            "success": true,
            "id": "sha256:abc123",
        })
        .to_string();

        assert_eq!(
            extract_persistable_tool_report("show_meme", &output).as_deref(),
            Some("<sent_meme>发送了一个表情包：id=sha256:abc123</sent_meme>")
        );
    }

    #[test]
    fn sent_meme_report_skips_failed_result() {
        let output = serde_json::json!({
            "success": false,
            "id": "sha256:abc123",
            "description": "猫猫",
        })
        .to_string();

        assert!(extract_persistable_tool_report("show_meme", &output).is_none());
    }

    #[test]
    fn mode_reminder_does_not_inject_a_reasoning_title_protocol() {
        let prompt = with_mode_reminder("base".to_string(), AgentMode::Normal);
        assert_eq!(prompt, "base");
        assert!(!prompt.contains("<runtime"));

        let prompt = with_mode_reminder("base".to_string(), AgentMode::Chat);
        assert!(prompt.contains("base"));
        assert!(prompt.contains(crate::prompts::CHAT_REMINDER));
        assert!(!prompt.contains("<runtime"));
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

    #[test]
    fn runtime_context_contains_dynamic_runtime_only() {
        let context = runtime_context(AgentMode::Normal, false);
        assert!(context.starts_with("<runtime "));
        assert!(context.contains("now=\""));
        assert!(context.contains("cwd=\""));
    }

    #[test]
    fn a_platform_runtime_stamp_carries_nothing_a_chat_message_cannot_use() {
        // A QQ turn has no working directory, no shell and no terminal. Those
        // attributes were re-sent at full price on every single turn — 285
        // chars where a timestamp needs about 45.
        let platform = runtime_context(AgentMode::Normal, true);
        assert!(platform.contains("now=\""), "{platform}");
        for noise in ["cwd=", "shell=", "terminal=", "env=", "note="] {
            assert!(!platform.contains(noise), "{noise} in {platform}");
        }
        assert!(
            platform.len() * 3 < runtime_context(AgentMode::Normal, false).len(),
            "{platform}"
        );
    }

    #[test]
    fn host_environment_rides_the_system_prompt_for_owners_only() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());

        let owner = with_host_environment("base".to_string(), PromptAudience::Owner, &paths);
        assert!(owner.starts_with("base\n\n<host-environment os=\""));
        assert!(owner.contains("/>"));
        assert!(owner.contains("LaTeX"), "渲染能力说明应跟随 owner 提示词");
        assert!(owner.contains(&format!(" miyu_home=\"{}\"", paths.root_dir.display())));
        // The static block must not be mistaken for the per-turn stamp, and
        // `mode_reminder_does_not_inject_a_reasoning_title_protocol` asserts the
        // system prompt never carries a `<runtime` tag.
        assert!(!owner.contains("<runtime"));

        // Platform and judge sessions come out byte-identical to today's prompt,
        // so they take no prefix-cache cold start from this change at all.
        for audience in [PromptAudience::External, PromptAudience::Internal] {
            assert_eq!(
                with_host_environment("base".to_string(), audience, &paths),
                "base",
                "{audience:?} must be untouched"
            );
        }
    }

    #[test]
    fn host_environment_is_byte_stable_across_prompt_rebuilds() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        // Rebuilt on every turn by `prepare_for_turn`; a value that drifted
        // between rebuilds would move the prefix and cost a cache miss a turn.
        let first = with_host_environment(String::new(), PromptAudience::Owner, &paths);
        let second = with_host_environment(String::new(), PromptAudience::Owner, &paths);
        assert_eq!(first, second);
    }

    #[test]
    fn user_identity_is_limited_to_owner_prompts() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut config = AppConfig::default();
        std::fs::create_dir_all(config.identities_dir_path(&paths)).unwrap();
        std::fs::write(config.user_identity_path(&paths), "legacy-owner-marker").unwrap();

        let owner = config
            .system_prompt_for(&paths, PromptAudience::Owner)
            .unwrap();
        let external = config
            .system_prompt_for(&paths, PromptAudience::External)
            .unwrap();
        let internal = config
            .system_prompt_for(&paths, PromptAudience::Internal)
            .unwrap();
        assert!(owner.contains("legacy-owner-marker"));
        assert!(!external.contains("legacy-owner-marker"));
        assert!(!internal.contains("legacy-owner-marker"));

        config.prompt.active_identity = "owner.md".to_string();
        std::fs::write(
            config.identity_path(&paths, "owner.md"),
            "active-owner-marker",
        )
        .unwrap();
        assert!(config
            .system_prompt_for(&paths, PromptAudience::Owner)
            .unwrap()
            .contains("active-owner-marker"));
        assert!(!config
            .system_prompt_for(&paths, PromptAudience::External)
            .unwrap()
            .contains("active-owner-marker"));
    }

    #[test]
    fn runtime_system_context_refreshes_the_effective_prompt_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();

        agent
            .set_runtime_system_context(vec!["  platform-only notice  ".to_string()])
            .unwrap();
        assert!(agent.system_prompt.contains("platform-only notice"));
        assert_eq!(
            agent.runtime_system_context,
            vec!["platform-only notice".to_string()]
        );
    }

    #[test]
    fn structured_platform_context_can_suppress_ambiguous_session_replay() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        state
            .start_turn("old", "anonymous old user", 999_999)
            .unwrap();
        state.complete_turn("old", "old assistant", None).unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();

        assert!(agent
            .chat_messages("current", "new user")
            .unwrap()
            .iter()
            .any(|message| format!("{:?}", message.content).contains("anonymous old user")));
        agent.set_session_history_suppressed(true);
        let messages = agent.chat_messages("current", "new user").unwrap();
        assert!(!messages
            .iter()
            .any(|message| format!("{:?}", message.content).contains("anonymous old user")));
        // [.., user, runtime tail]: the current user message sits right before
        // the transient runtime stamp.
        assert!(format!("{:?}", messages[messages.len() - 2].content).contains("new user"));
        assert!(format!("{:?}", messages.last().unwrap().content).contains("<runtime now="));
    }

    #[test]
    fn fossilized_transient_tail_replays_between_user_and_assistant() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        state.start_turn("old", "old question", 999_999).unwrap();
        state
            .set_turn_context_messages(
                "old",
                &[
                    ChatMessage::turn_context("<runtime now=\"frozen stamp\"/>"),
                    ChatMessage::turn_context(
                        "<associative-memory>frozen recall</associative-memory>",
                    ),
                ],
            )
            .unwrap();
        state.complete_turn("old", "old answer", None).unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let agent = Agent::new(
            config,
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();

        let messages = agent.chat_messages("current", "next question").unwrap();
        let text = |message: &ChatMessage| format!("{:?}", message.content);
        let user = messages
            .iter()
            .position(|m| text(m).contains("old question"))
            .unwrap();
        let assistant = messages
            .iter()
            .position(|m| text(m).contains("old answer"))
            .unwrap();
        // The fossils sit, in order, strictly between the user message and the
        // assistant reply — byte-for-byte what the live request sent.
        assert_eq!(messages[user + 1].role, "user");
        assert!(text(&messages[user + 1]).contains("frozen stamp"));
        assert_eq!(messages[user + 2].role, "user");
        assert!(text(&messages[user + 2]).contains("frozen recall"));
        assert!(user + 2 < assistant);
    }

    #[test]
    fn a_still_running_turn_stays_out_of_everyone_elses_history() {
        // A running turn holds a placeholder that is overwritten with the real
        // reply when it finishes, so replaying it puts two different byte
        // sequences at the same position and drops the prefix cache for every
        // turn behind it. About a fifth of this group's turns overlap.
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let state = StateStore::new(&paths).unwrap();
        state
            .start_turn("t1", "第一条", std::process::id())
            .unwrap();
        state
            .complete_turn_with_usage_and_model(
                "t1",
                "答复一",
                None,
                None,
                None,
                TurnTokens::default(),
                false,
            )
            .unwrap();
        state
            .start_turn("t2", "并发的一条", std::process::id())
            .unwrap();

        let visible = state.load_visible_turns_excluding("t3").unwrap();
        let running: Vec<&str> = visible
            .iter()
            .filter(|turn| turn.status == crate::state::TurnStatus::Running)
            .map(|turn| turn.turn_id.as_str())
            .collect();
        assert_eq!(running, ["t2"], "the store still hands them over");
        assert_eq!(
            visible
                .iter()
                .filter(|turn| turn.status != crate::state::TurnStatus::Running)
                .count(),
            1,
            "and exactly one is replayable"
        );
    }

    #[test]
    fn nothing_after_the_leading_prompt_may_carry_the_system_role() {
        // Provider chat templates gather every `system` message to the front of
        // the rendered prompt, so one appearing mid-conversation shifts that
        // block and drops the prefix cache to zero. Measured on DeepSeek with a
        // byte-identical prefix: appending `assistant + user` hit 99%, the same
        // append with one `system` in front of it hit 0%, and moving that
        // `system` to the very end still hit 0%.
        let messages = vec![
            ChatMessage::system("persona"),
            ChatMessage::plain("user", "问题"),
            ChatMessage::turn_context("<runtime now=\"x\"/>"),
            ChatMessage::turn_context("<associative-memory>x</associative-memory>"),
            ChatMessage::assistant("答案", None),
        ];
        let stray: Vec<usize> = messages
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, message)| message.role == "system")
            .map(|(index, _)| index)
            .collect();
        assert!(
            stray.is_empty(),
            "system role at {stray:?} would reset the prefix cache"
        );
    }

    #[test]
    fn a_fossil_written_before_the_role_change_replays_as_a_user_block() {
        // Old turns stored the transient tail as `system`. Replaying that
        // verbatim would keep poisoning the prefix for the rest of the
        // session's life, so it is re-roled on the way out.
        let stored = ChatMessage::system("<runtime now=\"old\"/>");
        let replayed = replay_fossil(&stored);
        assert_eq!(replayed.role, "user");
        assert!(replayed.transient_context);
        assert!(matches!(
            replayed.content.as_ref(),
            Some(ChatContent::Text(content)) if content == "<runtime now=\"old\"/>"
        ));

        // Already-converted fossils pass through untouched.
        let fresh = ChatMessage::turn_context("<runtime now=\"new\"/>");
        assert_eq!(replay_fossil(&fresh).role, "user");
    }

    #[test]
    fn fossil_capture_stops_at_the_first_non_context_message() {
        let tail = vec![
            ChatMessage::turn_context("<runtime now=\"x\"/>"),
            ChatMessage::turn_context("hint"),
            ChatMessage::plain("assistant", "loop starts here"),
            ChatMessage::turn_context("after loop — must not be captured"),
        ];
        let fossil = fossil_context_messages(&tail);
        assert_eq!(fossil.len(), 2);
        assert!(format!("{:?}", fossil[1].content).contains("hint"));
    }

    #[test]
    fn visible_association_lines_collects_only_replayed_memory_blocks() {
        let block = "<associative-memory>\n以下是根据当前输入联想到的完整人格记忆。\n\n曾经记住的相关知识点：\n- [2026-08-10] [公共知识] AUR 镜像只读\n</associative-memory>";
        let messages = vec![
            ChatMessage::system("prompt"),
            // 回放的化石块：user 角色、正文以标签开头 → 计入
            ChatMessage::plain("user", block),
            // 用户正文中途引用同样文本 → 不以标签开头，不计入
            ChatMessage::plain("user", format!("用户引用了 {block}")),
            // 非 user 角色 → 不计入
            ChatMessage::plain("assistant", "- [2026-08-10] [公共知识] AUR 镜像只读"),
        ];
        let seen = visible_association_lines(&messages);
        assert_eq!(seen.len(), 1);
        assert!(seen.contains("- [2026-08-10] [公共知识] AUR 镜像只读"));
    }

    #[test]
    fn turn_context_blocks_already_visible_in_fossils_are_skipped() {
        let notice = "[SystemInfo:LongReplyImageConversion]\n1. 你的一条长回复（约 480 字）已被自动渲染为 1 张图片发送。";
        let messages = vec![
            ChatMessage::system("prompt"),
            // 上一轮化石里已经带着同样的通知
            ChatMessage::plain("user", format!("<qq-request-context>…</qq-request-context>\n\n{notice}")),
            ChatMessage::plain("assistant", "回复"),
        ];
        assert!(turn_context_block_visible(&messages, notice));
        // 内容变化(记录数不同)不再匹配,照常注入
        let changed = "[SystemInfo:LongReplyImageConversion]\n1. 你的一条长回复（约 480 字）已被自动渲染为 1 张图片发送。\n2. 你的一条长回复（约 900 字）已被自动渲染为 2 张图片发送。";
        assert!(!turn_context_block_visible(&messages, changed));
        // 非 user 角色的出现不算
        let assistant_only = vec![ChatMessage::plain("assistant", notice)];
        assert!(!turn_context_block_visible(&assistant_only, notice));
        // 只有 [SystemInfo: 前缀的常驻通告参与去重;指涉"当前回合"的块
        // (唤醒通知/身份告警/审核初判)即使字节相同也必须重发
        assert!(notice.starts_with(STANDING_ADVISORY_PREFIX));
        assert!(!"本轮由系统自动触发：一个后台任务刚刚结束。".starts_with(STANDING_ADVISORY_PREFIX));
        assert!(!"<qq-identity-warning>…</qq-identity-warning>".starts_with(STANDING_ADVISORY_PREFIX));
    }

    #[test]
    fn vision_support_requires_every_effective_text_pool_model() {
        let mut config = AppConfig::default();
        let provider = config.providers.first_mut().unwrap();
        provider.default_model = "vision-model".to_string();
        provider.models = vec!["vision-model".to_string(), "text-model".to_string()];
        provider.model_modalities.insert(
            "vision-model".to_string(),
            vec!["text".to_string(), "image".to_string()],
        );
        provider
            .model_modalities
            .insert("text-model".to_string(), vec!["text".to_string()]);
        let provider_id = provider.id.clone();

        config.active_provider_models = Some(vec![ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: "vision-model".to_string(),
        }]);
        assert!(active_text_pool_supports_vision(&config));

        config
            .active_provider_models
            .as_mut()
            .unwrap()
            .push(ActiveProviderModelConfig {
                provider_id,
                model: "text-model".to_string(),
            });
        assert!(!active_text_pool_supports_vision(&config));
    }

    #[test]
    fn vision_preference_controls_direct_image_delivery_to_the_text_pool() {
        let mut config = AppConfig::default();
        let provider = config.providers.first_mut().unwrap();
        provider.model_modalities.insert(
            provider.default_model.clone(),
            vec!["text".to_string(), "image".to_string()],
        );

        assert!(should_use_active_text_pool_for_images(&config));
        config.plugins.vision.prefer_current_multimodal_model = false;
        assert!(!should_use_active_text_pool_for_images(&config));
    }

    #[tokio::test]
    async fn platform_images_register_a_turn_scoped_vision_tool() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();
        agent.set_image_platform("qq", "QQ");
        let images = vec![Some(PastedImage::Binary(ClipboardImage::new(
            "image/png".to_string(),
            vec![1, 2, 3],
        )))];

        let prepared = agent.prepare_user_input("看图", &images).await.unwrap();
        let hint = format!("{:?}", prepared.hints);
        assert!(hint.contains("vision_analyze"));
        let tools = agent.tools.lock().unwrap().clone();
        assert!(tools.contains("vision_analyze"));
        // A real but unattached local file must be rejected. Use a temp file
        // rather than a Unix path so the test also passes on Windows.
        let foreign = temp.path().join("foreign-image.png");
        std::fs::write(&foreign, b"not an attached image").unwrap();
        let request = serde_json::json!({ "image": foreign.display().to_string() }).to_string();
        let error = tools.call("vision_analyze", &request).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("image is not attached to the current platform turn"));
    }

    #[tokio::test]
    async fn context_image_ids_register_vision_without_a_current_image() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut agent = Agent::new(
            config.clone(),
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();
        agent.set_image_platform("qq", "QQ");
        let context = Arc::new(PlatformTurnContext::new(
            PlatformConversation {
                platform: "onebot".to_string(),
                account_id: "10000".to_string(),
                kind: ConversationKind::Group,
                conversation_id: "20000".to_string(),
            },
            "30000".to_string(),
            "tester".to_string(),
            false,
            config,
            paths.clone(),
            StateStore::new(&paths).unwrap(),
            Arc::new(NoopPlatformAdapter),
            Arc::new(crate::platforms::plugins::PlatformPluginRegistry::default()),
        ));
        agent.set_platform_context_images(
            context,
            vec![PlatformContextImageRef {
                id: "context_image_1".to_string(),
                message_id: "90".to_string(),
                image_index: 1,
            }],
        );

        let prepared = agent.prepare_user_input("接着说", &[]).await.unwrap();
        assert!(format!("{:?}", prepared.hints).contains("context_image_1"));
        let tools = agent.tools.lock().unwrap();
        assert!(tools.contains("vision_analyze"));
        let definition = tools
            .definitions()
            .into_iter()
            .find(|definition| definition.function.name == "vision_analyze")
            .unwrap();
        assert!(definition.function.description.contains("context_image_N"));
    }

    #[tokio::test]
    async fn binary_image_reaches_vision_pool_then_text_model() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let vision_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let text_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config =
            queue_test_config(format!("http://{}/v1", text_listener.local_addr().unwrap()));
        config.tools.enabled = false;
        config.plugins.vision.enabled = true;
        config.providers.push(ProviderConfig {
            id: "vision-test".to_string(),
            display_name: "Vision Test".to_string(),
            base_url: format!("http://{}/v1", vision_listener.local_addr().unwrap()),
            protocol: "openai-chat".to_string(),
            api_key: Some("test-key".to_string()),
            models: vec!["vision-model".to_string()],
            model_context_window: Default::default(),
            model_modalities: [(
                "vision-model".to_string(),
                vec!["text".to_string(), "image".to_string()],
            )]
            .into(),
            default_model: "vision-model".to_string(),
            timeout_seconds: 30,
            temperature: 0.0,
            anthropic_max_tokens: 4096,
            extra_body: None,
        });
        config.active_multimodal_provider_models = Some(vec![ActiveProviderModelConfig {
            provider_id: "vision-test".to_string(),
            model: "vision-model".to_string(),
        }]);

        let (vision_request_tx, vision_request_rx) = oneshot::channel();
        let vision_server = tokio::spawn(async move {
            let (mut stream, _) = vision_listener.accept().await.unwrap();
            let request = read_test_http_request(&mut stream).await;
            let _ = vision_request_tx.send(request);
            write_test_sse(
                &mut stream,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"a red square\"}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;
        });
        let (text_request_tx, text_request_rx) = oneshot::channel();
        let text_server = tokio::spawn(async move {
            let (mut stream, _) = text_listener.accept().await.unwrap();
            let request = read_test_http_request(&mut stream).await;
            let _ = text_request_tx.send(request);
            write_test_sse(
                &mut stream,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"I can see it.\"}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;
        });

        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        let text_provider = config.provider(None).unwrap().clone();
        let client = OpenAiCompatibleClient::new(&text_provider, &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();
        let image = PastedImage::Binary(ClipboardImage::new(
            "image/png".to_string(),
            b"qq-image-bytes".to_vec(),
        ));

        let result = agent
            .chat_stream_with_images("What is shown?", &[Some(image)], |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(result.content, "I can see it.");
        let vision_request: Value =
            serde_json::from_slice(&vision_request_rx.await.unwrap()).unwrap();
        let vision_parts = vision_request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "user")
            .unwrap()["content"]
            .as_array()
            .unwrap();
        assert!(vision_parts.iter().any(|part| {
            part["type"] == "image_url"
                && part["image_url"]["url"]
                    .as_str()
                    .is_some_and(|url| url.starts_with("data:image/png;base64,"))
        }));

        let text_request: Value = serde_json::from_slice(&text_request_rx.await.unwrap()).unwrap();
        let serialized = serde_json::to_string(&text_request).unwrap();
        assert!(serialized.contains("What is shown?"));
        assert!(serialized.contains("a red square"));
        vision_server.await.unwrap();
        text_server.await.unwrap();
    }

    #[test]
    fn effective_context_tokens_include_tool_definitions() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(ToolSpec::new(
            "heavy_context_tool",
            "This tool has a deliberately long description so effective context includes tool definitions.",
            empty_parameters(),
            |_| async { Ok(String::new()) },
        ));
        let with_tools = Agent::new(
            config.clone(),
            &paths,
            state.clone(),
            client.clone(),
            tools,
            AgentMode::Normal,
        )
        .unwrap();
        let without_tools = Agent::new(
            AppConfig {
                tools: crate::config::ToolsConfig {
                    enabled: false,
                    ..config.tools.clone()
                },
                ..config
            },
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();

        assert!(
            with_tools.effective_context_tokens().unwrap()
                > without_tools.effective_context_tokens().unwrap()
        );
    }

    #[test]
    fn overflow_check_tokens_triggers_at_threshold() {
        let check = overflow::OverflowCheck::new(Some(100_000), 0.9, None);
        assert!(!check.check_tokens(60_000));
        assert!(check.check_tokens(95_000));
    }

    #[test]
    fn overflow_check_disabled_when_no_window() {
        let check = overflow::OverflowCheck::new(None, 0.9, None);
        assert!(!check.is_enabled());
        assert!(!check.check_tokens(1_998_998));
    }

    #[test]
    fn overflow_check_estimate_triggers() {
        let check = overflow::OverflowCheck::new(Some(1_000), 0.9, None);
        let big_msg = ChatMessage::plain("user", &"token ".repeat(2_000));
        let small_msg = ChatMessage::plain("user", "hi");
        assert!(check.check_estimate(&[big_msg]));
        assert!(!check.check_estimate(&[small_msg]));
    }

    #[test]
    fn structured_tool_business_failure_marks_the_event_failed() {
        assert!(!tool_output_succeeded(r#"{"success":false}"#));
        assert!(!tool_output_succeeded(r#"{"ok":false}"#));
        assert!(tool_output_succeeded(r#"{"success":true}"#));
        assert!(tool_output_succeeded("plain tool output"));
    }

    #[tokio::test]
    async fn queue_ingress_waits_for_a_reserved_tool_followup() {
        let barrier = Arc::new(QueueIngressBarrier::default());
        barrier.tool_started("call_1");
        let reservation = barrier
            .try_reserve()
            .expect("active tool accepts follow-up");
        barrier.tool_finished("call_1");

        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            barrier.wait_for_reserved_ingress()
        )
        .await
        .is_err());
        assert!(barrier.try_reserve().is_none());

        drop(reservation);
        tokio::time::timeout(
            Duration::from_millis(100),
            barrier.wait_for_reserved_ingress(),
        )
        .await
        .expect("released follow-up reservation wakes the agent");
    }

    #[test]
    fn queue_ingress_tracks_parallel_tool_calls_by_id() {
        let barrier = Arc::new(QueueIngressBarrier::default());
        barrier.tool_started("call_1");
        barrier.tool_started("call_2");
        barrier.tool_finished("call_1");
        assert!(barrier.try_reserve().is_some());
        barrier.tool_finished("call_2");
        assert!(barrier.try_reserve().is_none());
    }

    #[test]
    fn journal_persists_a_stream_batch_before_displaying_it() {
        let temp = tempfile::tempdir().unwrap();
        let state = crate::state::StateStore::new(&test_paths(temp.path())).unwrap();
        state
            .start_turn("journal-turn", "long task", std::process::id())
            .unwrap();
        let mut sink = TurnJournalSink::new(state.clone(), "journal-turn".to_string(), 0);
        let mut displayed = Vec::new();
        {
            let mut on_event = |event| {
                if let AgentEvent::Chunk(chunk) = event {
                    displayed.push(chunk.text);
                }
                Ok(())
            };
            sink.emit(
                AgentEvent::Chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Content,
                    text: "durable partial".to_string(),
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
        assert_eq!(displayed, ["durable partial"]);
        assert_eq!(state.load_turns().unwrap()[0].journal_events.len(), 1);

        state.interrupt_turn("journal-turn").unwrap();
        assert!(state.load_turns().unwrap()[0]
            .assistant_content
            .contains("durable partial"));
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

    #[test]
    fn journal_flush_precedes_queued_prompt_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let state = crate::state::StateStore::new(&test_paths(temp.path())).unwrap();
        state
            .start_turn("boundary-turn", "long task", std::process::id())
            .unwrap();
        state
            .enqueue_prompt("q1", "followup", "followup", &[])
            .unwrap();
        let mut sink = TurnJournalSink::new(state.clone(), "boundary-turn".to_string(), 0);
        let mut displayed = Vec::new();
        let mut transport = |event| {
            if let AgentEvent::Chunk(chunk) = event {
                displayed.push(chunk.text);
            }
            Ok(())
        };
        let mut journaled = |event| sink.emit(event, &mut transport);

        journaled(AgentEvent::Chunk(ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: "answer before followup".to_string(),
        }))
        .unwrap();
        journaled(AgentEvent::FlushJournal).unwrap();
        state
            .consume_queued_prompts(
                "boundary-turn",
                &[("q1".to_string(), "followup".to_string())],
                Some("answer before followup"),
                None,
            )
            .unwrap();
        journaled(AgentEvent::QueuedPromptsConsumed {
            prompt_ids: vec!["q1".to_string()],
            mode: AgentMode::Normal,
            provider_id: None,
            model: None,
        })
        .unwrap();

        let events = state.load_turns().unwrap()[0].journal_events.clone();
        assert_eq!(displayed, ["answer before followup"]);
        assert_eq!(
            events
                .iter()
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            ["assistant_content", "queued_prompts_consumed"]
        );
    }

    #[test]
    fn turn_context_tokens_match_sent_messages() {
        let mut turn = crate::state::Turn {
            turn_id: "t1".to_string(),
            seq: 1,
            user_content: "question".to_string(),
            display_content: "question".to_string(),
            user_timestamp: String::new(),
            assistant_content: "answer".to_string(),
            assistant_reasoning: Some("hidden reasoning ".repeat(1_000)),
            assistant_provider_id: None,
            assistant_model: None,
            assistant_timestamp: None,
            status: crate::state::TurnStatus::Completed,
            tool_reports: Vec::new(),
            question_exchanges: Vec::new(),
            followups: Vec::new(),
            attachments: Vec::new(),
            hidden: false,
            is_summary: false,
            owner_pid: None,
            token_total: 0,
            token_prompt: 0,
            token_cache_read: 0,
            token_usage_estimated: false,
            revision: 0,
            journal_events: Vec::new(),
            context_messages: Vec::new(),
        };
        let with_reasoning = turn_context_tokens(&turn);
        turn.assistant_reasoning = None;
        let without_reasoning = turn_context_tokens(&turn);
        assert!(with_reasoning > without_reasoning);

        turn.tool_reports.push("persisted tool result".to_string());
        assert!(turn_context_tokens(&turn) > without_reasoning);

        turn.tool_reports.clear();
        turn.assistant_content.clear();
        turn.assistant_reasoning = Some("replayed reasoning ".repeat(1_000));
        assert_eq!(
            assistant_replay_content(&turn),
            turn.assistant_reasoning.as_deref().unwrap()
        );
        let with_replayed_reasoning = turn_context_tokens(&turn);
        turn.assistant_reasoning = None;
        assert!(with_replayed_reasoning > turn_context_tokens(&turn));
    }

    #[test]
    fn assistant_reasoning_is_replayed_as_private_context() {
        let mut messages = Vec::new();
        push_assistant_context_messages(
            &mut messages,
            "visible answer",
            Some("raw provider reasoning"),
            true,
        );

        assert_eq!(messages.len(), 2);
        // Rides as a `user` block: a mid-conversation `system` message resets
        // the provider's whole prefix cache.
        assert_eq!(messages[0].role, "user");
        assert!(matches!(
            messages[0].content.as_ref(),
            Some(ChatContent::Text(content))
                if content.contains("<previous_assistant_reasoning>\nraw provider reasoning")
        ));
        assert_eq!(messages[1].role, "assistant");
        assert!(matches!(
            messages[1].content.as_ref(),
            Some(ChatContent::Text(content)) if content == "visible answer"
        ));
    }

    #[test]
    fn interrupted_redo_replays_prefix_followups_before_new_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let agent = Agent::new(
            config,
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();
        let followup =
            |prompt_id: &str, content: &str, preceding: &str| crate::state::TurnFollowup {
                prompt_id: prompt_id.to_string(),
                content: content.to_string(),
                display_content: content.to_string(),
                attachments: Vec::new(),
                uploaded_attachments: Vec::new(),
                submitted_at: String::new(),
                preceding_assistant_content: Some(preceding.to_string()),
                preceding_assistant_reasoning: None,
                preceding_assistant_provider_id: None,
                preceding_assistant_model: None,
            };
        let mut turn = crate::state::Turn {
            turn_id: "redo-turn".to_string(),
            seq: 1,
            user_content: "initial".to_string(),
            display_content: "initial".to_string(),
            user_timestamp: String::new(),
            assistant_content: crate::state::pending_placeholder().to_string(),
            assistant_reasoning: None,
            assistant_provider_id: None,
            assistant_model: None,
            assistant_timestamp: None,
            status: crate::state::TurnStatus::Interrupted,
            tool_reports: Vec::new(),
            question_exchanges: vec![
                QuestionExchange {
                    questions: vec![crate::question::QuestionPrompt {
                        header: "Route".to_string(),
                        question: "Pick a route".to_string(),
                        options: vec![crate::question::QuestionOption {
                            label: "A".to_string(),
                            description: "".to_string(),
                        }],
                        multiple: false,
                        custom: false,
                    }],
                    answers: vec![vec!["A".to_string()]],
                    answered_at: String::new(),
                },
                QuestionExchange {
                    questions: vec![crate::question::QuestionPrompt {
                        header: "Branch".to_string(),
                        question: "Current branch question".to_string(),
                        options: vec![crate::question::QuestionOption {
                            label: "B".to_string(),
                            description: "".to_string(),
                        }],
                        multiple: false,
                        custom: false,
                    }],
                    answers: vec![vec!["B".to_string()]],
                    answered_at: String::new(),
                },
            ],
            followups: vec![
                followup("q1", "edited first followup", "first answer"),
                followup("q2", "new followup", "after q1"),
            ],
            attachments: Vec::new(),
            hidden: false,
            is_summary: false,
            owner_pid: None,
            token_total: 0,
            token_prompt: 0,
            token_cache_read: 0,
            token_usage_estimated: false,
            revision: 1,
            journal_events: vec![
                crate::state::TurnJournalEvent {
                    event_id: 0,
                    revision: 1,
                    segment_index: 0,
                    kind: "redo_prefix_question_count".to_string(),
                    call_id: None,
                    name: None,
                    text_payload: Some("1".to_string()),
                    blob_payload: None,
                    ok: None,
                },
                crate::state::TurnJournalEvent {
                    event_id: 1,
                    revision: 1,
                    segment_index: 0,
                    kind: "assistant_content".to_string(),
                    call_id: None,
                    name: None,
                    text_payload: Some("after q1".to_string()),
                    blob_payload: None,
                    ok: None,
                },
                crate::state::TurnJournalEvent {
                    event_id: 2,
                    revision: 1,
                    segment_index: 0,
                    kind: "queued_prompts_consumed".to_string(),
                    call_id: None,
                    name: None,
                    text_payload: Some("[\"q2\"]".to_string()),
                    blob_payload: None,
                    ok: None,
                },
                crate::state::TurnJournalEvent {
                    event_id: 3,
                    revision: 1,
                    segment_index: 1,
                    kind: "assistant_content".to_string(),
                    call_id: None,
                    name: None,
                    text_payload: Some("after q2".to_string()),
                    blob_payload: None,
                    ok: None,
                },
            ],
            context_messages: Vec::new(),
        };

        let messages = interrupted_turn_replay_messages(&agent, &turn);
        let text_messages = messages
            .iter()
            .filter_map(|message| match message.content.as_ref() {
                Some(ChatContent::Text(text)) => Some((message.role.as_str(), text.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let q1 = text_messages
            .iter()
            .position(|(_, text)| *text == "edited first followup")
            .unwrap();
        let clarification = text_messages
            .iter()
            .position(|(_, text)| text.contains("Pick a route"))
            .unwrap();
        assert!(!text_messages
            .iter()
            .any(|(_, text)| text.contains("Current branch question")));
        let after_q1 = text_messages
            .iter()
            .position(|(_, text)| *text == "after q1")
            .unwrap();
        let q2 = text_messages
            .iter()
            .position(|(_, text)| *text == "new followup")
            .unwrap();
        let after_q2 = text_messages
            .iter()
            .position(|(_, text)| *text == "after q2")
            .unwrap();
        assert!(clarification < q1);
        assert!(q1 < after_q1);
        assert!(after_q1 < q2);
        assert!(q2 < after_q2);

        turn.journal_events
            .retain(|event| event.kind != "redo_prefix_question_count");
        turn.journal_events.push(crate::state::TurnJournalEvent {
            event_id: 4,
            revision: 1,
            segment_index: 1,
            kind: "tool_result".to_string(),
            call_id: Some("question-call".to_string()),
            name: Some("ask_question".to_string()),
            text_payload: Some("{\"status\":\"answered\"}".to_string()),
            blob_payload: None,
            ok: Some(true),
        });
        let legacy_messages = interrupted_turn_replay_messages(&agent, &turn);
        let legacy_text = legacy_messages
            .iter()
            .filter_map(|message| match message.content.as_ref() {
                Some(ChatContent::Text(text)) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(legacy_text.iter().any(|text| text.contains("Pick a route")));
        assert!(!legacy_text
            .iter()
            .any(|text| text.contains("Current branch question")));
    }

    #[tokio::test]
    async fn parallel_task_calls_run_concurrently_and_map_outputs() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(crate::tools::ToolSpec::new(
            "task",
            "stub subagent",
            crate::tools::empty_parameters(),
            |args| async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                Ok(format!(
                    "done:{}",
                    args.get("n")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?")
                ))
            },
        ));
        let agent = Agent::new(
            config,
            &paths,
            state.clone(),
            client,
            registry,
            AgentMode::Normal,
        )
        .unwrap();

        let calls: Vec<crate::llm::ToolCall> = (0..3)
            .map(|index| crate::llm::ToolCall {
                id: format!("call_{index}"),
                kind: "function".to_string(),
                function: crate::llm::ToolCallFunction {
                    name: "task".to_string(),
                    arguments: format!(r#"{{"n":"{index}"}}"#),
                },
            })
            .collect();
        let mut events = Vec::new();
        let started = std::time::Instant::now();
        let outputs = agent
            .execute_parallel_task_calls(&calls, &std::collections::BTreeSet::new(), &mut |event| {
                match &event {
                    AgentEvent::ToolCall { call_id, .. } => events.push((call_id.clone(), "call")),
                    AgentEvent::ToolResult {
                        call_id, ok: true, ..
                    } => events.push((call_id.clone(), "ok")),
                    AgentEvent::ToolResult {
                        call_id, ok: false, ..
                    } => events.push((call_id.clone(), "err")),
                    _ => {}
                }
                Ok(())
            })
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(outputs.len(), 3);
        for index in 0..3 {
            assert_eq!(outputs[&index].output, format!("done:{index}"));
        }
        // Three 80ms tasks run concurrently, not sequentially (~240ms).
        assert!(
            elapsed < Duration::from_millis(200),
            "tasks did not run in parallel: {elapsed:?}"
        );
        for index in 0..3 {
            let call_id = format!("call_{index}");
            assert!(events.contains(&(call_id.clone(), "call")));
            assert!(events.contains(&(call_id, "ok")));
        }

        // Fewer than two task calls: empty map, serial path handles it.
        let single = agent
            .execute_parallel_task_calls(
                &calls[..1],
                &std::collections::BTreeSet::new(),
                &mut |_| Ok(()),
            )
            .await
            .unwrap();
        assert!(single.is_empty());
    }

    #[test]
    fn trim_visible_context_keeps_summary_and_removes_oldest_turn() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig {
            tools: crate::config::ToolsConfig {
                enabled: false,
                ..AppConfig::default().tools
            },
            ..AppConfig::default()
        };
        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state.clone(),
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();
        state
            .insert_summary_turn(&"summary ".repeat(2_000), TurnTokens::default(), true)
            .unwrap();
        for id in ["t1", "t2"] {
            state
                .start_turn(id, &format!("{id} {}", "question ".repeat(2_000)), 999999)
                .unwrap();
            state
                .complete_turn(id, &"answer ".repeat(2_000), None)
                .unwrap();
        }
        agent.trim_at_ratio = 1.0;
        let context_window = agent.effective_context_tokens().unwrap() as usize;
        let choice = agent.config.active_provider_model_choices().remove(0);
        agent
            .config
            .providers
            .iter_mut()
            .find(|provider| provider.id == choice.provider_id)
            .unwrap()
            .model_context_window
            .insert(choice.model, context_window);
        assert_eq!(agent.context_window(), Some(context_window));

        let evicted = agent.trim_visible_context().unwrap();

        assert!(!evicted.is_empty());
        let visible = state.load_visible_turns().unwrap();
        assert_eq!(visible.len(), 2);
        assert!(visible[0].is_summary);
        assert_eq!(visible[1].turn_id, "t2");
    }

    #[test]
    fn trim_accounts_for_tool_definitions_unloaded_with_a_popped_turn() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let mut config = AppConfig::default();
        config.tools.loading_mode = "hybrid".to_string();
        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(
            ToolSpec::new(
                "heavy_context_tool",
                "heavy context ".repeat(20_000),
                empty_parameters(),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(false),
        );
        let mut agent = Agent::new(
            config,
            &paths,
            state.clone(),
            client,
            tools,
            AgentMode::Normal,
        )
        .unwrap();
        for id in ["t1", "t2"] {
            state.start_turn(id, id, 999999).unwrap();
            state.complete_turn(id, "reply", None).unwrap();
        }
        state
            .add_session_loaded_tools(&["heavy_context_tool".to_string()], Some("t1"))
            .unwrap();
        agent.trim_at_ratio = 1.0;
        agent.trim_batch_ratio = 0.5;
        let context_window = agent.effective_context_tokens().unwrap() as usize;
        let choice = agent.config.active_provider_model_choices().remove(0);
        agent
            .config
            .providers
            .iter_mut()
            .find(|provider| provider.id == choice.provider_id)
            .unwrap()
            .model_context_window
            .insert(choice.model, context_window);

        agent.trim_visible_context().unwrap();

        let visible = state.load_visible_turns().unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].turn_id, "t2");
        assert!(state.load_session_loaded_tools().unwrap().is_empty());
    }

    #[test]
    fn trim_ignores_stale_loaded_tool_sources_when_persistence_is_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let mut config = AppConfig::default();
        config.tools.loading_mode = "hybrid".to_string();
        config.tools.persist_loaded_tools = false;
        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(
            ToolSpec::new(
                "stale_heavy_tool",
                "stale heavy context ".repeat(20_000),
                empty_parameters(),
                |_| async { Ok(String::new()) },
            )
            .with_always_loaded(false),
        );
        let mut agent = Agent::new(
            config,
            &paths,
            state.clone(),
            client,
            tools,
            AgentMode::Normal,
        )
        .unwrap();
        for id in ["t1", "t2"] {
            state.start_turn(id, id, 999999).unwrap();
            state.complete_turn(id, "reply", None).unwrap();
        }
        state
            .add_session_loaded_tools(&["stale_heavy_tool".to_string()], Some("t1"))
            .unwrap();
        agent.trim_at_ratio = 1.0;
        agent.trim_batch_ratio = 0.5;
        let context_window = agent.effective_context_tokens().unwrap() as usize;
        let choice = agent.config.active_provider_model_choices().remove(0);
        agent
            .config
            .providers
            .iter_mut()
            .find(|provider| provider.id == choice.provider_id)
            .unwrap()
            .model_context_window
            .insert(choice.model, context_window);

        agent.trim_visible_context().unwrap();

        assert!(state.load_visible_turns().unwrap().is_empty());
    }

    #[test]
    fn explicit_pop_archives_context_content_but_not_reasoning() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        state.start_turn("t1", "promptonlyalpha", 999999).unwrap();
        state
            .complete_turn("t1", "answeronlybeta", Some("reasoningonlyquasar"))
            .unwrap();
        state
            .append_persisted_context("t1", "toolonlygamma")
            .unwrap();
        let memory = MemoryStore::new(&config, &paths);
        let turns = state.oldest_evictable_visible_turns(1).unwrap();

        archive_and_delete_visible_turns(&state, &memory, &turns).unwrap();

        assert!(state.load_visible_turns().unwrap().is_empty());
        for query in ["promptonlyalpha", "answeronlybeta", "toolonlygamma"] {
            assert!(
                !memory.search_evicted_context(query, 10).unwrap()["results"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(memory
            .search_evicted_context("reasoningonlyquasar", 10)
            .unwrap()["results"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn explicit_pop_still_deletes_when_evicted_context_archiving_is_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let mut config = AppConfig::default();
        config.memory.evicted_context_enabled = false;
        let state = StateStore::new(&paths).unwrap();
        state.start_turn("t1", "unarchived-marker", 999999).unwrap();
        state.complete_turn("t1", "reply", None).unwrap();
        let memory = MemoryStore::new(&config, &paths);
        let turns = state.oldest_evictable_visible_turns(1).unwrap();

        archive_and_delete_visible_turns(&state, &memory, &turns).unwrap();

        assert!(state.load_visible_turns().unwrap().is_empty());
        assert!(memory
            .search_evicted_context("unarchived-marker", 10)
            .unwrap()["results"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn explicit_pop_does_not_archive_a_turn_removed_before_commit() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        state
            .start_turn("t1", "stale-archive-quasar", 999999)
            .unwrap();
        state.complete_turn("t1", "reply", None).unwrap();
        let turns = state.oldest_evictable_visible_turns(1).unwrap();
        state.delete_visible_turns(&["t1".to_string()]).unwrap();
        let memory = MemoryStore::new(&config, &paths);

        assert!(archive_and_delete_visible_turns(&state, &memory, &turns).is_err());

        assert!(memory
            .search_evicted_context("stale-archive-quasar", 10)
            .unwrap()["results"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn failed_concurrent_pop_preserves_archive_from_the_successful_pop() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        state
            .start_turn("t1", "successful-pop-quasar", 999999)
            .unwrap();
        state.complete_turn("t1", "reply", None).unwrap();
        let turns = state.oldest_evictable_visible_turns(1).unwrap();
        let memory = MemoryStore::new(&config, &paths);

        archive_and_delete_visible_turns(&state, &memory, &turns).unwrap();
        assert!(archive_and_delete_visible_turns(&state, &memory, &turns).is_err());

        assert!(!memory
            .search_evicted_context("successful-pop-quasar", 10)
            .unwrap()["results"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn explicit_pop_removes_new_archive_when_the_turn_still_exists_hidden() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        state
            .start_turn("t1", "hidden-stale-quasar", 999999)
            .unwrap();
        state.complete_turn("t1", "reply", None).unwrap();
        let turns = state.oldest_evictable_visible_turns(1).unwrap();
        state
            .replace_visible_with_summary(
                &["t1".to_string()],
                &["t1".to_string()],
                "summary",
                TurnTokens::default(),
                false,
                None,
            )
            .unwrap();
        let memory = MemoryStore::new(&config, &paths);

        assert!(archive_and_delete_visible_turns(&state, &memory, &turns).is_err());

        assert!(memory
            .search_evicted_context("hidden-stale-quasar", 10)
            .unwrap()["results"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn queued_prompt_continues_after_a_completed_model_call() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let mut config = queue_test_config(base_url);
        config.tools.enabled = false;
        config.providers[0].model_modalities.insert(
            "test-model".to_string(),
            vec!["text".to_string(), "image".to_string()],
        );
        let control = AgentTurnControl::new(
            AgentMode::Normal,
            ToolRegistry::new(),
            ToolRegistry::new(),
        );
        let server_control = control.clone();
        let (request_tx, request_rx) = oneshot::channel();
        let (redo_request_tx, redo_request_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let _ = read_test_http_request(&mut first).await;
            server_control.set_mode(AgentMode::Chat);
            write_test_sse(
                &mut first,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"first reasoning\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"first answer\"}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;

            let (mut second, _) = listener.accept().await.unwrap();
            let request = read_test_http_request(&mut second).await;
            let _ = request_tx.send(request);
            write_test_sse(
                &mut second,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"continued answer\"}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;

            let (mut third, _) = listener.accept().await.unwrap();
            let request = read_test_http_request(&mut third).await;
            let _ = redo_request_tx.send(request);
            write_test_sse(
                &mut third,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"redone answer\"}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;
        });

        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        let provider = config.provider(None).unwrap().clone();
        let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state.clone(),
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();
        state
            .enqueue_prompt(
                "q1",
                "queued followup",
                "queued followup",
                &[QueuedPromptAttachment::Binary {
                    mime: "image/png".to_string(),
                    data_base64: base64::engine::general_purpose::STANDARD.encode(b"image-data"),
                }],
            )
            .unwrap();

        let result = agent
            .chat_stream_with_control("initial prompt", &[], &control, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(result.content, "continued answer");
        assert_eq!(agent.mode(), AgentMode::Chat);
        let request: serde_json::Value =
            serde_json::from_slice(&request_rx.await.unwrap()).unwrap();
        let messages = request["messages"].as_array().unwrap();
        let first_answer = messages
            .iter()
            .position(|message| {
                message["role"] == "assistant" && message["content"] == "first answer"
            })
            .unwrap();
        let followup = messages
            .iter()
            .position(|message| {
                message["role"] == "user"
                    && message["content"].as_array().is_some_and(|parts| {
                        parts
                            .iter()
                            .any(|part| part["type"] == "text" && part["text"] == "queued followup")
                            && parts.iter().any(|part| part["type"] == "image_url")
                    })
            })
            .unwrap();
        assert!(messages.iter().any(|message| {
            message["role"] == "user"
                && message["content"].as_str().is_some_and(|content| {
                    content.contains("<previous_assistant_reasoning>\nfirst reasoning")
                })
        }));
        assert!(first_answer < followup);
        let turns = state.load_turns().unwrap();
        assert_eq!(
            turns[0].followups[0].preceding_assistant_content.as_deref(),
            Some("first answer")
        );
        assert_eq!(
            turns[0].followups[0]
                .preceding_assistant_reasoning
                .as_deref(),
            Some("first reasoning")
        );
        let history = agent.chat_messages("", "next prompt").unwrap();
        assert!(history.iter().any(|message| {
            matches!(
                message.content.as_ref(),
                Some(ChatContent::Parts(parts))
                    if parts.iter().any(|part| matches!(part, ChatContentPart::ImageUrl { .. }))
            )
        }));
        let candidate = state.redo_candidate().unwrap().unwrap();
        let redo = agent
            .redo_stream_with_control(
                &candidate,
                vec![RedoPromptInput {
                    prompt_id: "q1".to_string(),
                    content: "edited followup".to_string(),
                    display_content: "edited followup".to_string(),
                    images: vec![Some(PastedImage::Binary(ClipboardImage::new(
                        "image/png".to_string(),
                        b"image-data".to_vec(),
                    )))],
                }],
                &control,
                |_| Ok(()),
            )
            .await
            .unwrap();
        assert_eq!(redo.content, "redone answer");
        let redo_request: serde_json::Value =
            serde_json::from_slice(&redo_request_rx.await.unwrap()).unwrap();
        let redo_messages = redo_request["messages"].as_array().unwrap();
        assert!(redo_messages.iter().any(|message| {
            message["role"] == "assistant" && message["content"] == "first answer"
        }));
        assert!(redo_messages.iter().any(|message| {
            message["role"] == "user"
                && message["content"].as_array().is_some_and(|parts| {
                    parts
                        .iter()
                        .any(|part| part["type"] == "text" && part["text"] == "edited followup")
                })
        }));
        assert!(!redo_messages.iter().any(|message| {
            message["role"] == "assistant" && message["content"] == "continued answer"
        }));
        let turn = state.load_turns().unwrap().remove(0);
        assert_eq!(turn.assistant_content, "redone answer");
        assert_eq!(turn.followups[0].content, "edited followup");
        assert_eq!(turn.revision, 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn supersede_restarts_the_same_turn_without_replaying_partial_output() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let mut config = queue_test_config(base_url);
        config.tools.enabled = false;
        let (partial_tx, partial_rx) = oneshot::channel();
        let (second_request_tx, second_request_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let _ = read_test_http_request(&mut first).await;
            first
                .write_all(
                    concat!(
                        "HTTP/1.1 200 OK\r\n",
                        "content-type: text/event-stream\r\n",
                        "connection: close\r\n\r\n",
                        "data: {\"choices\":[{\"delta\":{\"content\":\"discarded partial\"}}]}\n\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            first.flush().await.unwrap();
            let _ = partial_tx.send(());
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(first);

            let (mut second, _) = listener.accept().await.unwrap();
            let request = read_test_http_request(&mut second).await;
            let _ = second_request_tx.send(request);
            write_test_sse(
                &mut second,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"updated final\"}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;
        });

        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        let provider = config.provider(None).unwrap().clone();
        let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state.clone(),
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();
        let signal = Arc::new(TurnSupersedeSignal::default());
        let mut control = AgentTurnControl::new(
            AgentMode::Normal,
            ToolRegistry::new(),
            ToolRegistry::new(),
        );
        control.set_supersede_signal(signal.clone());
        let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let event_log = events.clone();
        let chat = agent.chat_stream_with_control("original", &[], &control, move |event| {
            if matches!(event, AgentEvent::GenerationSuperseded { .. }) {
                event_log.lock().unwrap().push("superseded");
            }
            Ok(())
        });
        let enqueue = async {
            partial_rx.await.unwrap();
            state
                .enqueue_prompt("update", "changed requirement", "changed requirement", &[])
                .unwrap();
            signal.trigger();
        };
        let (result, ()) = tokio::join!(chat, enqueue);
        let result = result.unwrap();
        assert_eq!(result.content, "updated final");
        assert_eq!(&*events.lock().unwrap(), &["superseded"]);
        let request: Value = serde_json::from_slice(&second_request_rx.await.unwrap()).unwrap();
        let serialized = serde_json::to_string(&request["messages"]).unwrap();
        assert!(serialized.contains("changed requirement"));
        assert!(!serialized.contains("discarded partial"));
        let turns = state.load_turns().unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].assistant_content, "updated final");
        assert_eq!(turns[0].followups.len(), 1);
        assert!(turns[0].followups[0].preceding_assistant_content.is_none());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn responses_tool_round_uses_previous_response_id_and_only_new_input() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let mut config = queue_test_config(base_url);
        config.tools.enabled = true;
        config.tools.loading_mode = "full".to_string();
        config.skills.enabled = false;
        config.memory.enabled = false;
        config.providers[0].protocol = "openai-responses".to_string();
        config.providers[0].models = vec!["gpt-5".to_string()];
        config.providers[0].default_model = "gpt-5".to_string();

        let mut tools = ToolRegistry::new();
        tools.register(ToolSpec::new(
            "responses_continuation_tool",
            "returns a fixed result",
            empty_parameters(),
            |_| async { Ok("tool finished".to_string()) },
        ));
        let control = AgentTurnControl::new(
            AgentMode::Normal,
            tools.clone(),
            tools.clone(),
        );
        let server_control = control.clone();

        let (first_request_tx, first_request_rx) = oneshot::channel();
        let (second_request_tx, second_request_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let first_request = read_test_http_request(&mut first).await;
            let _ = first_request_tx.send(first_request);
            server_control.set_mode(AgentMode::Chat);
            write_test_sse(
                &mut first,
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
                    "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_1\",\"name\":\"responses_continuation_tool\",\"arguments\":\"\"}}\n\n",
                    "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_1\",\"delta\":\"{}\"}\n\n",
                    "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_1\",\"name\":\"responses_continuation_tool\",\"arguments\":\"{}\"}}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7}}}\n\n"
                ),
            )
            .await;

            let (mut second, _) = listener.accept().await.unwrap();
            let second_request = read_test_http_request(&mut second).await;
            let _ = second_request_tx.send(second_request);
            write_test_sse(
                &mut second,
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_2\"}}\n\n",
                    "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_2\",\"delta\":\"final answer\"}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_2\"}}\n\n"
                ),
            )
            .await;
        });

        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        let provider = config.provider(None).unwrap().clone();
        let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state.clone(),
            client,
            tools,
            AgentMode::Normal,
        )
        .unwrap();
        state
            .enqueue_prompt("q1", "queued followup", "queued followup", &[])
            .unwrap();

        let result = agent
            .chat_stream_with_control("initial prompt", &[], &control, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(result.content, "final answer");
        assert_eq!(agent.mode(), AgentMode::Chat);
        assert!(result.responses_continuation.is_none());
        assert!(result.usage_estimated);
        let tool_only_tokens =
            overflow::estimate_messages_tokens(&[ChatMessage::tool("call_1", "tool finished")])
                as u64;
        assert!(result.usage.as_ref().unwrap().prompt_tokens > 5 + tool_only_tokens);
        let first_request: Value =
            serde_json::from_slice(&first_request_rx.await.unwrap()).unwrap();
        assert!(first_request.get("previous_response_id").is_none());
        assert!(first_request["input"].as_array().is_some_and(|input| {
            input.iter().any(|item| item["role"] == "user")
                && input.iter().any(|item| item["role"] == "system")
        }));

        let second_request: Value =
            serde_json::from_slice(&second_request_rx.await.unwrap()).unwrap();
        assert_eq!(second_request["previous_response_id"], "resp_1");
        let input = second_request["input"].as_array().unwrap();
        let function_output = input
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap();
        assert_eq!(function_output["call_id"], "call_1");
        assert_eq!(function_output["output"], "tool finished");
        let function_index = input
            .iter()
            .position(|item| item["type"] == "function_call_output")
            .unwrap();
        // Responses-style user items carry their text as `input_text` parts,
        // so the block has to be read through both shapes.
        let item_text = |item: &Value| -> String {
            match &item["content"] {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            }
        };
        let is_mode_update = |item: &Value| {
            let text = item_text(item);
            item["role"] == "user"
                && text.contains("<mode-update active=\"chat\">")
                && text.contains(crate::prompts::CHAT_REMINDER)
        };
        let mode_index = input.iter().position(is_mode_update).unwrap();
        assert!(input.iter().any(is_mode_update));
        let queued_index = input
            .iter()
            .position(|item| {
                item["role"] == "user"
                    && item["content"].as_array().is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part["type"] == "input_text" && part["text"] == "queued followup"
                        })
                    })
            })
            .unwrap();
        assert!(input.iter().any(|item| {
            item["role"] == "user"
                && item["content"].as_array().is_some_and(|parts| {
                    parts.iter().any(|part| {
                        part["type"] == "input_text" && part["text"] == "queued followup"
                    })
                })
        }));
        assert!(function_index < mode_index && mode_index < queued_index);
        assert!(!serde_json::to_string(input)
            .unwrap()
            .contains("initial prompt"));
        assert!(second_request["tools"].as_array().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool["name"] == "responses_continuation_tool")
        }));
        assert_eq!(
            state.load_turns().unwrap()[0].assistant_content,
            "final answer"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn queued_prompts_are_consumed_after_tools_with_dispatch_time_mode() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let mut config = queue_test_config(base_url);
        config.tools.enabled = true;
        config.skills.enabled = false;
        config.memory.enabled = false;

        let mut normal_tools = ToolRegistry::new();
        normal_tools.register(ToolSpec::new(
            "queue_boundary_tool",
            "returns a fixed result",
            empty_parameters(),
            |_| async { Ok("tool finished".to_string()) },
        ));
        let control = AgentTurnControl::new(
            AgentMode::Normal,
            normal_tools.clone(),
            ToolRegistry::new(),
        );
        let server_control = control.clone();
        let (request_tx, request_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let _ = read_test_http_request(&mut first).await;
            server_control.set_mode(AgentMode::Chat);
            write_test_sse(
                &mut first,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"queue_boundary_tool\",\"arguments\":\"{}\"}}]}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;

            let (mut second, _) = listener.accept().await.unwrap();
            let request = read_test_http_request(&mut second).await;
            let _ = request_tx.send(request);
            write_test_sse(
                &mut second,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"final answer\"}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;
        });

        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        let provider = config.provider(None).unwrap().clone();
        let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state.clone(),
            client,
            normal_tools,
            AgentMode::Normal,
        )
        .unwrap();
        state
            .enqueue_prompt("q1", "first followup", "first followup", &[])
            .unwrap();
        state
            .enqueue_prompt("q2", "second followup", "second followup", &[])
            .unwrap();
        let mut consumed = None;

        let result = agent
            .chat_stream_with_control("initial prompt", &[], &control, |event| {
                if let AgentEvent::QueuedPromptsConsumed {
                    prompt_ids, mode, ..
                } = event
                {
                    consumed = Some((prompt_ids, mode));
                }
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(result.content, "final answer");
        assert_eq!(agent.mode(), AgentMode::Chat);
        assert_eq!(
            consumed,
            Some((vec!["q1".to_string(), "q2".to_string()], AgentMode::Chat))
        );
        let request: serde_json::Value =
            serde_json::from_slice(&request_rx.await.unwrap()).unwrap();
        let messages = request["messages"].as_array().unwrap();
        assert!(messages.iter().any(|message| {
            message["role"] == "user" && message["content"] == "first followup"
        }));
        assert!(messages.iter().any(|message| {
            message["role"] == "user" && message["content"] == "second followup"
        }));
        assert!(messages
            .iter()
            .any(|message| { message["role"] == "tool" && message["content"] == "tool finished" }));
        assert!(state.load_queued_prompts().unwrap().is_empty());
        let turns = state.load_turns().unwrap();
        assert_eq!(turns[0].followups.len(), 2);
        assert_eq!(turns[0].assistant_content, "final answer");
        server.await.unwrap();
    }

    async fn read_test_http_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(read > 0);
            request.extend_from_slice(&buffer[..read]);
            if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length: ")
                    .or_else(|| line.strip_prefix("Content-Length: "))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(read > 0);
            request.extend_from_slice(&buffer[..read]);
        }
        request[header_end..header_end + content_length].to_vec()
    }

    async fn write_test_sse(stream: &mut TcpStream, body: &str) {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    /// v7 byte-prefix guard (compact scenario): request N must be a pure
    /// element-wise prefix extension of request N-1, except immediately
    /// after a compaction — and each compaction may reset the prefix at most
    /// once. Catches any regression that inserts, deletes, or perturbs
    /// already-sent history bytes (the failure mode is symptomless in
    /// production: cache hit rate silently degrades).
    #[tokio::test]
    async fn compaction_resets_the_byte_prefix_at_most_once_each() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let mut config = queue_test_config(base_url);
        config.tools.enabled = false;
        config.providers[0]
            .model_context_window
            .insert("test-model".to_string(), 3000);
        config.context.compact_tail_tokens = Some(600);
        // Isolated summary path: its request is identifiable by the compact
        // system prompt and excluded from the prefix chain.
        config.context.compact_cache_reuse = false;
        config.context.prune_stale_tool_reports = false;
        // Pin the persona. This test is about compaction's effect on the byte
        // prefix, not about whatever `prompts/miyu.md` currently weighs —
        // editing the persona used to move the overflow point and flip the
        // outcome.
        config.system_prompt = Some("prefix cache guard fixture persona".to_string());

        let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
        let server_bodies = bodies.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let body = read_test_http_request(&mut stream).await;
                let body = String::from_utf8_lossy(&body).to_string();
                let is_compact = body.contains("context summarization assistant");
                server_bodies.lock().unwrap().push(body);
                let sse = if is_compact {
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"## Task Goal\\nmock summary\"}}]}\n\n",
                        "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                        "data: [DONE]\n\n"
                    )
                } else {
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
                        "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                        "data: [DONE]\n\n"
                    )
                };
                write_test_sse(&mut stream, sse).await;
            }
        });

        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();

        // Pin the workspace too: `runtime_context` embeds the effective working
        // directory in the system prompt, so the token budget would otherwise
        // shift with the length of the path the test happens to be run from.
        let filler = "prefix cache guard filler content 前缀缓存守卫填充 ".repeat(40);
        let workspace = temp.path().to_path_buf();
        crate::tools::workspace::with_workspace(workspace, async {
            for i in 0..6 {
                agent
                    .chat_stream(&format!("message {i}: {filler}"), |_| Ok(()))
                    .await
                    .unwrap();
                let tokens = agent.effective_context_tokens().unwrap();
                agent
                    .handle_overflow_after_turn(tokens, |_| Ok(()))
                    .await
                    .unwrap();
            }
        })
        .await;
        server.abort();

        let bodies = bodies.lock().unwrap().clone();
        let compact_requests = bodies
            .iter()
            .filter(|body| body.contains("context summarization assistant"))
            .count();
        assert!(
            compact_requests >= 1,
            "the scenario must trigger at least one compaction"
        );
        let chat: Vec<serde_json::Value> = bodies
            .iter()
            .filter(|body| !body.contains("context summarization assistant"))
            .map(|body| serde_json::from_str(body).unwrap())
            .collect();
        assert!(chat.len() >= 6);
        let mut resets = 0usize;
        for pair in chat.windows(2) {
            let prev = pair[0]["messages"].as_array().unwrap();
            let next = pair[1]["messages"].as_array().unwrap();
            let shared = prev
                .iter()
                .zip(next.iter())
                .take_while(|(a, b)| a == b)
                .count();
            if shared == prev.len() {
                continue; // pure append-only extension
            }
            resets += 1;
            assert!(shared >= 1, "the system prompt must never diverge");
            let checkpoint = next[1]["content"].as_str().unwrap_or_default();
            assert!(
                checkpoint.contains("<conversation-checkpoint>"),
                "a reset must be a compaction (summary checkpoint in slot 1), got: {}",
                &checkpoint[..checkpoint.len().min(120)]
            );
        }
        // The cache guarantee is one-directional: a reset may only ever be a
        // compaction, and compaction may not reset more than once per run.
        // Requiring the converse — that every compaction resets — is not a
        // property of the system: when the fold cannot save enough, the
        // compactor keeps the existing history and the prefix simply extends.
        assert!(
            resets >= 1,
            "the scenario must exercise at least one real prefix reset"
        );
        assert!(
            resets <= compact_requests,
            "prefix reset {resets} times against {compact_requests} compactions; \
             nothing but compaction may reset the byte prefix"
        );
    }

    fn queue_test_config(base_url: String) -> AppConfig {
        let mut config = AppConfig {
            active_provider: "queue-test".to_string(),
            active_provider_models: None,
            providers: vec![ProviderConfig {
                id: "queue-test".to_string(),
                display_name: "Queue Test".to_string(),
                base_url,
                protocol: "openai-chat".to_string(),
                api_key: Some("test-key".to_string()),
                models: vec!["test-model".to_string()],
                model_context_window: Default::default(),
                model_modalities: Default::default(),
                default_model: "test-model".to_string(),
                timeout_seconds: 30,
                temperature: 0.0,
                anthropic_max_tokens: 4096,
                extra_body: None,
            }],
            ..AppConfig::default()
        };
        config.skills.enabled = false;
        config.memory.enabled = false;
        config
    }

    #[test]
    fn binary_image_cache_is_isolated_by_platform() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let images = vec![Some(PastedImage::Binary(ClipboardImage::new(
            "image/jpeg".to_string(),
            b"same-image-content".to_vec(),
        )))];

        let platform = resolve_pasted_image_paths(&images, &paths, Some("qq"));
        let platform_path = PathBuf::from(platform[0].as_deref().unwrap());
        assert!(platform_path.starts_with(paths.cache_dir.join("platform_images/qq")));
        assert!(platform_path.is_file());

        let clipboard = resolve_pasted_image_paths(&images, &paths, None);
        let clipboard_path = PathBuf::from(clipboard[0].as_deref().unwrap());
        assert!(clipboard_path.starts_with(paths.cache_dir.join("clipboard_images")));
        assert!(clipboard_path.is_file());
        assert_ne!(platform_path, clipboard_path);
    }

    fn test_paths(root: &std::path::Path) -> MiyuPaths {
        MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("fish/miyu.fish"),
            bash_hook_file: root.join("shell/bash-hook.sh"),
            zsh_hook_file: root.join("shell/zsh-hook.zsh"),
            scripts_dir: root.join("config/scripts"),
            system_scripts_dir: PathBuf::new(),
        }
    }
}
