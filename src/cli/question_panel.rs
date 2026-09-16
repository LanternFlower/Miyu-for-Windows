//! Shared question handling for direct CLI output and the live REPL.

use miyu_base::question::{QuestionRequest, QuestionResponse};
use miyu_hosts::render::StreamRenderer;

pub(super) fn answer(
    renderer: &mut StreamRenderer,
    request: QuestionRequest,
    responder: tokio::sync::oneshot::Sender<QuestionResponse>,
    scroll: Option<&mut dyn FnMut(isize, u16)>,
) -> anyhow::Result<()> {
    let leave_summary = !renderer.timeline_static();
    let response = crate::question_tui::ask_with(&request, scroll, leave_summary)
        .unwrap_or_else(|err| QuestionResponse::Unavailable(err.to_string()));
    // Persist the exchange in the body buffer before the next full-screen repaint.
    renderer.timeline_push_question(&request, &response)?;
    renderer.write_question_exchange(&request, &response)?;
    if !matches!(&response, QuestionResponse::Cancelled) {
        renderer.start_waiting()?;
    }
    let _ = responder.send(response);
    Ok(())
}
