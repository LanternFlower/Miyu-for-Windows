//! 回合的并发控制：占位守卫、超越信号、入队栅栏。
//!
//! 一个会话同时只能跑一个回合，但「同时」这件事有好几种发生方式：用户连发两
//! 条、重做撞上正在跑的回合、工具追加和主回合抢入队。这里的三个守卫各管一种。
//!
//! 全部实现了 `Drop`，且**回滚逻辑写在 Drop 里而不是正常路径上**——回合可能在
//! 任何一个 await 点被取消，只有 Drop 保证跑到。

use crate::agent::*;

pub(in crate::agent) const MAX_QUESTION_ROUNDS_PER_TURN: usize = 8;

pub struct PendingTurnGuard {
    pub(in crate::agent) state: StateStore,
    pub(in crate::agent) turn_id: String,
    pub(in crate::agent) completed: bool,
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

pub(in crate::agent) struct PendingRedoGuard {
    pub(in crate::agent) state: StateStore,
    pub(in crate::agent) turn_id: String,
    pub(in crate::agent) revision: i64,
    pub(in crate::agent) completed: bool,
}

impl PendingRedoGuard {
    pub fn new(state: StateStore, turn_id: String, revision: i64) -> Self {
        Self {
            state,
            turn_id,
            revision,
            completed: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::agent) fn complete_with_model(
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

/// 场所(CLI `--dev`、IPC/会话记录里的 `normal|dev`、WebUI 开关)说的会话形态。
/// Normal=人格全能力;Dev=极简开发形态(一行可编辑提示词、无人格全家、
/// 精简工具目录)。回合引擎内部没有这个概念(09-16 退役):`Agent::new` /
/// `switch_mode` 把它折成 `core.dev`(是不是保留人格 `dev` 的会话),提示词、
/// 工具面、记忆全按人格裁决;这里只剩对外词汇。原「闲聊(Chat)」模式已删除:
/// 平台路径从来只跑 Normal,安全靠 restricted registry(工具不存在)而非模式门。
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AgentMode {
    Normal,
    Dev,
}

#[derive(Clone)]
pub struct AgentTurnControl {
    pub(in crate::agent) mode: Arc<Mutex<AgentMode>>,
    pub(in crate::agent) normal_tools: ToolRegistry,
    pub(in crate::agent) dev_tools: ToolRegistry,
    pub(in crate::agent) queue_ingress: Option<Arc<QueueIngressBarrier>>,
    pub(in crate::agent) supersede: Option<Arc<TurnSupersedeSignal>>,
    pub(in crate::agent) supersede_seen: Arc<AtomicU64>,
}

#[derive(Default)]
pub struct TurnSupersedeSignal {
    pub(in crate::agent) generation: AtomicU64,
    pub(in crate::agent) changed: Notify,
}

impl TurnSupersedeSignal {
    pub fn trigger(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.changed.notify_waiters();
        generation
    }

    pub(in crate::agent) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(in crate::agent) async fn wait_after(&self, observed: u64) {
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
pub struct QueueIngressBarrier {
    pub(in crate::agent) state: Mutex<QueueIngressState>,
    pub(in crate::agent) changed: Notify,
}

#[derive(Default)]
pub(in crate::agent) struct QueueIngressState {
    pub(in crate::agent) active_calls: HashSet<String>,
    pub(in crate::agent) reservations: usize,
    pub(in crate::agent) closed: bool,
}

pub struct QueueIngressReservation {
    pub(in crate::agent) barrier: Arc<QueueIngressBarrier>,
}

impl QueueIngressBarrier {
    pub fn tool_started(&self, call_id: &str) {
        let mut state = self.state.lock().unwrap();
        if !state.closed {
            state.active_calls.insert(call_id.to_string());
        }
    }

    pub fn tool_finished(&self, call_id: &str) {
        self.state.lock().unwrap().active_calls.remove(call_id);
        self.changed.notify_waiters();
    }

    pub fn try_reserve(self: &Arc<Self>) -> Option<QueueIngressReservation> {
        let mut state = self.state.lock().unwrap();
        if state.closed || state.active_calls.is_empty() {
            return None;
        }
        state.reservations = state.reservations.saturating_add(1);
        Some(QueueIngressReservation {
            barrier: self.clone(),
        })
    }

    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.active_calls.clear();
        self.changed.notify_waiters();
    }

    pub(in crate::agent) async fn wait_for_reserved_ingress(&self) {
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
    pub fn new(mode: AgentMode, normal_tools: ToolRegistry, dev_tools: ToolRegistry) -> Self {
        Self {
            mode: Arc::new(Mutex::new(mode)),
            normal_tools,
            dev_tools,
            queue_ingress: None,
            supersede: None,
            supersede_seen: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn set_queue_ingress(&mut self, ingress: Arc<QueueIngressBarrier>) {
        self.queue_ingress = Some(ingress);
    }

    pub fn set_supersede_signal(&mut self, signal: Arc<TurnSupersedeSignal>) {
        self.supersede = Some(signal);
    }

    pub(in crate::agent) fn pending_supersede_generation(&self) -> Option<u64> {
        let generation = self.supersede.as_ref()?.generation();
        (generation != self.supersede_seen.load(Ordering::Acquire)).then_some(generation)
    }

    pub(in crate::agent) fn mark_supersede_seen(&self, generation: u64) {
        self.supersede_seen.store(generation, Ordering::Release);
    }

    pub fn mode(&self) -> AgentMode {
        *self.mode.lock().unwrap()
    }

    pub fn set_mode(&self, mode: AgentMode) {
        *self.mode.lock().unwrap() = mode;
    }

    pub(in crate::agent) fn tools(&self, mode: AgentMode) -> ToolRegistry {
        match mode {
            AgentMode::Normal => self.normal_tools.clone(),
            AgentMode::Dev => self.dev_tools.clone(),
        }
    }
}

impl AgentMode {
    pub fn label(self) -> &'static str {
        if miyu_base::i18n::is_zh() {
            match self {
                Self::Normal => "普通",
                Self::Dev => "开发",
            }
        } else {
            match self {
                Self::Normal => "NORMAL",
                Self::Dev => "DEV",
            }
        }
    }
}
