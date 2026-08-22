//! 桥版 ask_question:让 claude(中转)也能向用户提结构化问题。
//!
//! 正常回合里 ask_question 是回合循环的特例(AgentEvent::AskQuestion →
//! QuestionBroker → REPL/WebUI 应答);桥调用没有回合循环,这里直接对
//! broker 复刻同一条流——question.requested 事件带活动 run_id,前端照常
//! 弹问答,答案经既有 AnswerQuestion 通道回到 oneshot。问答对照常落到
//! running turn 上(回放语义与回合内一致)。

use crate::question::{
    answered_tool_output, closed_tool_output, unavailable_tool_output, QuestionExchange,
    QuestionRequest, QuestionResponse,
};
use crate::web::*;

/// 等答案的上限:claude 侧的 MCP 客户端超时已放宽到 30 分钟,这里保持一致。
const QUESTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

pub(in crate::web) async fn bridge_ask_question(
    state: &DaemonState,
    session_id: &str,
    arguments: &str,
) -> String {
    let request = match QuestionRequest::parse(arguments) {
        Ok(request) => request,
        Err(error) => return format!("tool error: {error:#}"),
    };
    // 问题要弹在正在跑的回合上;没有活动 run 就没有能应答的界面。
    let run = {
        let manager = state.manager.lock().unwrap();
        manager
            .active_runs
            .iter()
            .find(|(_, info)| &*info.session_id == session_id)
            .map(|(run_id, info)| (run_id.clone(), info.turn_id.clone()))
    };
    let Some((run_id, turn_id)) = run else {
        return unavailable_tool_output("no interactive session is attached to answer");
    };
    let (responder, receiver) = tokio::sync::oneshot::channel();
    let question_id = state.questions.insert(&run_id, request.clone(), responder);
    state.events.publish(
        "question.requested",
        json!({
            "run_id": run_id,
            "question_id": question_id,
            "tool_id": format!("bridge_{question_id}"),
            "name": "ask_question",
            "questions": request.questions,
        }),
    );
    let response = match tokio::time::timeout(QUESTION_TIMEOUT, receiver).await {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => QuestionResponse::Cancelled,
        Err(_) => return unavailable_tool_output("the question timed out with no answer"),
    };
    match response {
        QuestionResponse::Answered(answers) => match QuestionExchange::new(request, answers) {
            Ok(exchange) => {
                // 问答对挂到 running turn,回放语义与回合内 ask_question 一致。
                let turn_id = turn_id.or_else(|| {
                    state
                        .state_store
                        .pinned(session_id)
                        .running_turn_queue_target()
                        .ok()
                        .flatten()
                        .map(|target| target.turn_id)
                });
                if let Some(turn_id) = turn_id {
                    if let Err(error) = state
                        .state_store
                        .pinned(session_id)
                        .append_question_exchange(&turn_id, &exchange)
                    {
                        tracing::warn!(
                            session_id,
                            %error,
                            "failed to persist a bridge question exchange"
                        );
                    }
                }
                answered_tool_output(&exchange)
            }
            Err(error) => format!("tool error: {error:#}"),
        },
        QuestionResponse::Closed => closed_tool_output(),
        QuestionResponse::Cancelled => {
            unavailable_tool_output("the question was cancelled before an answer arrived")
        }
        QuestionResponse::Unavailable(reason) => unavailable_tool_output(&reason),
    }
}
