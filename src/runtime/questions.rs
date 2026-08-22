//! 提问代理：把模型的提问送到前端，等回答再送回来。
// 兄弟模块的类型互相引用（DaemonState 持有 EventHub、run 记录引用
// ManagerState 等），统一从 mod.rs 的再导出取，免得每个文件维护一份
// 交叉导入清单。
use super::*;
use crate::question::{QuestionAnswers, QuestionRequest, QuestionResponse};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

// ── QuestionBroker 提问代理 ──
#[derive(Clone)]
pub(crate) struct QuestionBroker {
    pub(crate) pending: Arc<Mutex<HashMap<String, PendingQuestion>>>,
}

pub(crate) struct PendingQuestion {
    pub(crate) run_id: String,
    pub(crate) request: QuestionRequest,
    pub(crate) responder: oneshot::Sender<QuestionResponse>,
}

#[derive(Debug)]
pub(crate) enum AnswerFailure {
    NotFound,
    Invalid(String),
    Gone,
}

impl QuestionBroker {
    pub(crate) fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn insert(
        &self,
        run_id: &str,
        request: QuestionRequest,
        responder: oneshot::Sender<QuestionResponse>,
    ) -> String {
        let mut pending = self.pending.lock().unwrap();
        loop {
            let question_id = random_id("question", 18);
            if !pending.contains_key(&question_id) {
                pending.insert(
                    question_id.clone(),
                    PendingQuestion {
                        run_id: run_id.to_string(),
                        request,
                        responder,
                    },
                );
                return question_id;
            }
        }
    }

    pub(crate) fn answer<F>(
        &self,
        question_id: &str,
        answers: QuestionAnswers,
        before_resume: F,
    ) -> std::result::Result<(), AnswerFailure>
    where
        F: FnOnce(&str, &QuestionAnswers),
    {
        let mut all_pending = self.pending.lock().unwrap();
        let request = all_pending
            .get(question_id)
            .map(|pending| pending.request.clone())
            .ok_or(AnswerFailure::NotFound)?;
        let answers = normalize_answers(&request, answers).map_err(AnswerFailure::Invalid)?;
        let pending = all_pending
            .remove(question_id)
            .ok_or(AnswerFailure::NotFound)?;
        let run_id = pending.run_id;
        if pending.responder.is_closed() {
            return Err(AnswerFailure::Gone);
        }
        before_resume(&run_id, &answers);
        pending
            .responder
            .send(QuestionResponse::Answered(answers.clone()))
            .map_err(|_| AnswerFailure::Gone)?;
        Ok(())
    }

    pub(crate) fn close<F>(
        &self,
        question_id: &str,
        before_resume: F,
    ) -> std::result::Result<(), AnswerFailure>
    where
        F: FnOnce(&str),
    {
        let mut all_pending = self.pending.lock().unwrap();
        let pending = all_pending
            .remove(question_id)
            .ok_or(AnswerFailure::NotFound)?;
        let run_id = pending.run_id;
        if pending.responder.is_closed() {
            return Err(AnswerFailure::Gone);
        }
        before_resume(&run_id);
        pending
            .responder
            .send(QuestionResponse::Closed)
            .map_err(|_| AnswerFailure::Gone)?;
        Ok(())
    }

    pub(crate) fn cancel_run(&self, run_id: &str) {
        let cancelled = {
            let mut pending = self.pending.lock().unwrap();
            let ids = pending
                .iter()
                .filter(|(_, question)| question.run_id == run_id)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id))
                .collect::<Vec<_>>()
        };
        for pending in cancelled {
            let _ = pending.responder.send(QuestionResponse::Cancelled);
        }
    }
}

// ── normalize_answers ──
pub(crate) fn normalize_answers(
    request: &QuestionRequest,
    mut answers: QuestionAnswers,
) -> std::result::Result<QuestionAnswers, String> {
    for answer in &mut answers {
        for value in answer {
            *value = value.trim().to_string();
            if value.chars().any(char::is_control) {
                return Err("answers cannot contain control characters".to_string());
            }
        }
    }
    crate::question::validate_answers(request, &answers)
        .map_err(|error| safe_error_message(&error))?;
    Ok(answers)
}

// ── constant_time_eq ──
pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let left = left.get(index).copied().unwrap_or(0);
        let right = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left ^ right);
    }
    difference == 0
}
