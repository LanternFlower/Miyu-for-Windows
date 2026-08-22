//! 工具桥内层调用的 progress 泵。
//!
//! 桥调用(`miyu tool-call` / MCP 桥)在 daemon 侧执行,工具的 progress 事件
//! 没有回合的事件发射端可挂——此前 `registry.call` 给的是空 progress,图片
//! 被静默丢弃,`prepare_for_external_output` 还会误答 true 把图打进 daemon
//! 自己的 stdout。这里补一条与回合渲染同构的路:图片照常落 image asset
//! (挂到该会话正在跑的 turn 上),再以 `tool.image` 事件直发 EventHub——
//! REPL 与 WebUI 按 run_id 过滤,拿到的与回合内的真事件同形。

use crate::web::*;

/// 以带 progress 的方式执行一次桥内层调用:图片事件落资产 + 广播,其余
/// progress 语义与空 progress 一致(丢弃;`prepare_for_external_output`
/// 明确答 false——桥没有用户终端可打)。
pub(in crate::web) async fn call_with_bridge_progress(
    state: &DaemonState,
    session_id: &str,
    registry: &crate::tools::ToolRegistry,
    name: &str,
    arguments: &str,
) -> Result<String> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let future = registry.call_with_progress_future(
        name,
        arguments,
        sender,
        &crate::tools::GuardCtx::default(),
    )?;
    tokio::pin!(future);
    let mut image_seq = 0usize;
    let result = loop {
        tokio::select! {
            result = &mut future => break result,
            event = receiver.recv() => match event {
                Some(event) => {
                    handle_bridge_progress(state, session_id, name, &mut image_seq, event);
                }
                None => break future.await,
            },
        }
    };
    // future 先完成时排空遗留事件(图片常在收尾一刻发出)。
    while let Ok(event) = receiver.try_recv() {
        handle_bridge_progress(state, session_id, name, &mut image_seq, event);
    }
    result
}

fn handle_bridge_progress(
    state: &DaemonState,
    session_id: &str,
    tool_name: &str,
    image_seq: &mut usize,
    event: crate::tools::ToolProgressEvent,
) {
    match event {
        crate::tools::ToolProgressEvent::Image { path, alt, size } => {
            *image_seq += 1;
            let Some((run, turn_id)) = resolve_bridge_turn(state, session_id, tool_name, "image")
            else {
                return;
            };
            let tool_id = format!("bridge_{tool_name}_{image_seq}");
            match state
                .state_store
                .save_image_asset(&turn_id, Some(&tool_id), &path, &alt)
            {
                Ok(asset) => {
                    tracing::info!(
                        session_id,
                        tool = tool_name,
                        turn_id = %turn_id,
                        asset_id = %asset.asset_id,
                        "bridge tool image persisted"
                    );
                    // 与 RunEventMapper 的 tool.image 同形,REPL/WebUI 按
                    // run_id 过滤后走同一条渲染路;没有活动 run 就只落库
                    // (刷新可见)。
                    if let Some(run_id) = run {
                        state.events.publish(
                            "tool.image",
                            json!({
                                "run_id": run_id,
                                "tool_id": tool_id,
                                "name": tool_name,
                                "size": size,
                                "asset": SafeImageAsset::from_asset(asset, false),
                            }),
                        );
                    }
                }
                Err(error) => tracing::warn!(
                    session_id,
                    tool = tool_name,
                    %error,
                    "failed to persist a bridge tool image"
                ),
            }
        }
        // Artifact 与图片同构:落 artifact asset + tool.artifact 事件。丢掉
        // 它就是 present_artifact "假 ok" 的根源(工具答成功,WebUI 里啥都
        // 没有)。
        crate::tools::ToolProgressEvent::Artifact { path, title } => {
            *image_seq += 1;
            let Some((run, turn_id)) =
                resolve_bridge_turn(state, session_id, tool_name, "artifact")
            else {
                return;
            };
            let tool_id = format!("bridge_{tool_name}_{image_seq}");
            match state
                .state_store
                .save_artifact_asset(&turn_id, Some(&tool_id), &path, &title)
            {
                Ok(asset) => {
                    tracing::info!(
                        session_id,
                        tool = tool_name,
                        turn_id = %turn_id,
                        "bridge tool artifact persisted"
                    );
                    if let Some(run_id) = run {
                        state.events.publish(
                            "tool.artifact",
                            json!({
                                "run_id": run_id,
                                "tool_id": tool_id,
                                "name": tool_name,
                                "artifact": SafeArtifactAsset::from(asset),
                            }),
                        );
                    }
                }
                Err(error) => tracing::warn!(
                    session_id,
                    tool = tool_name,
                    %error,
                    "failed to persist a bridge tool artifact"
                ),
            }
        }
        crate::tools::ToolProgressEvent::PrepareForExternalOutput { ready } => {
            // 桥没有用户终端:明确拒绝,工具走 report_image 等非终端路径。
            // (空 progress 会误答 true,print_image 曾把图打进 daemon stdout。)
            let _ = ready.send(false);
        }
        // 文本进度/命令输出/artifact 在桥语义下没有去处,与既往一致丢弃。
        _ => {}
    }
}

/// 桥调用挂靠的 (活动 run, running turn):没有运行中回合就放弃——asset 的
/// turn_id 外键必须指向真实 turn 行。
fn resolve_bridge_turn(
    state: &DaemonState,
    session_id: &str,
    tool_name: &str,
    what: &str,
) -> Option<(Option<String>, String)> {
    let run = {
        let manager = state.manager.lock().unwrap();
        manager
            .active_runs
            .iter()
            .find(|(_, info)| &*info.session_id == session_id)
            .map(|(run_id, info)| (run_id.clone(), info.turn_id.clone()))
    };
    let turn_id = match &run {
        Some((_, Some(turn_id))) => Some(turn_id.clone()),
        _ => state
            .state_store
            .pinned(session_id)
            .running_turn_queue_target()
            .ok()
            .flatten()
            .map(|target| target.turn_id),
    };
    match turn_id {
        Some(turn_id) => Some((run.map(|(run_id, _)| run_id), turn_id)),
        None => {
            tracing::warn!(
                session_id,
                tool = tool_name,
                "bridge tool {what} dropped: no running turn to attach it to"
            );
            None
        }
    }
}
