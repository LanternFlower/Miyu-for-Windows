//! 平台回合上下文的窄端口(09-16 拆 crate 前置)。
//!
//! 回合引擎对「这是个平台回合」只用得着四件事:平台名(用量记账来源)、排队消息带来的文件、
//! 把「按 id 读平台文件」的工具挂进工具面、以及把上下文交给看图工具(那边收的是
//! [`PlatformToolContext`])。此前 `TurnInput` 直接持有 `Arc<PlatformTurnContext>`,等于 agent
//! 认识整个平台层;现在只认识这张 trait,实现由 `platforms::turn_context` 提供(方向向下)。
//! 非平台回合(终端、WebUI)这里是 `None`,与从前 `platform_context.is_none()` 同义。

use crate::tools::ToolRegistry;
use miyu_base::platform_types::{PlatformContextFileRef, PlatformToolContext};
use std::sync::Arc;

pub trait PlatformTurn: PlatformToolContext {
    /// 平台名(`onebot` 一类):用量记账的来源标签。
    fn platform_name(&self) -> &str;
    /// 排队消息随带的文件:并入当前回合时取走(取过即清)。
    fn take_queued_files(&self, prompt_id: &str) -> Vec<PlatformContextFileRef>;
    /// 把「按 context id 读平台文件」的工具挂进本回合的工具面。
    fn register_file_reader(
        self: Arc<Self>,
        registry: &mut ToolRegistry,
        files: Vec<PlatformContextFileRef>,
    );
}
