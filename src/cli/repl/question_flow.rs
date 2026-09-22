//! `question.requested` 的终端侧处理：弹面板、把答案回发给 daemon。
//!
//! 两条泵共用一份（09-20）。原来只有自己起的那一轮（`remote::one_shot`）处理
//! 它；daemon 自己开的轮（目标续轮、后台任务唤醒）走 `repl::wake` 的另一张
//! 手写分发表，那张表里没有这个分支，事件落进 `_ => {}`——面板不弹、没人回答，
//! 那一步就停在「准备问题」上，直到回合循环那 30 分钟的兜底超时才收场
//! （用户 09-20：`/goal 试试用问问题工具随意问我一个问题` 永远卡住）。
//!
//! `ipc_events.rs` 顶上记着同款教训：解码表抄两份，谁漏抄一个变体，那条路径
//! 就少一个功能。这次把**处理**也收成一份，第三条泵接上来只要调这个函数。

use crate::cli::repl::tail::*;
use crate::cli::*;

/// 弹出提问面板，按结果回发 `AnswerQuestion` / `CloseQuestion` / `Cancel`。
///
/// `live` 为 `None` = 没有活动的 REPL 尾巴（一次性客户端）：面板照弹，只是不
/// 需要挂起/恢复那一套。
pub(in crate::cli) async fn handle_question_requested(
    paths: &MiyuPaths,
    config: &AppConfig,
    mut live: Option<&mut LiveReplTail>,
    renderer: &mut render::StreamRenderer,
    data: &serde_json::Value,
    run_id: &str,
    // 在 herdr 里跑时这一轮的守卫。她反问 = 「卡住等人」，侧栏把整条 tab /
    // workspace 标红；答完报回 `working`。两条泵都要，所以放在这里而不是各自
    // 的分发表里（09-20：one_shot 那边只报了 blocked 没报 resumed，wake 那边
    // 两个都没报）。
    herdr_turn: Option<&crate::cli::repl::herdr::TurnGuard>,
) -> Result<()> {
    // 报回 `working` 走 RAII：这个函数有好几条出口（答完、关掉、取消、各种
    // `?`），只在成功路径上报的话，答完侧栏还一直红着——herdr 那一项就是这么
    // 栽的（九条出口只报了一条）。
    let _resume_on_exit = HerdrBlocked::begin(
        herdr_turn,
        data.get("questions")
            .and_then(|questions| questions.get(0))
            .and_then(|question| question.get("question"))
            .and_then(serde_json::Value::as_str),
    );
    // 只让屏、不切线：这一步得等答案到手才补得进去。
    renderer.prepare_for_panel()?;
    if let Some(live) = live.as_deref_mut() {
        live.apply_renderer_frame(renderer)?;
        synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
    }
    let request = miyu_base::question::QuestionRequest {
        questions: serde_json::from_value(data.get("questions").cloned().unwrap_or_default())?,
    };
    notify_if_unfocused(
        &config,
        live.as_deref().map(|live| live.editor.focused),
        t("Miyu is waiting on you", "Miyu 在等你回答"),
        // 问题正文同样不外泄，理由同上。
        t("waiting for you", "正在等待处理"),
        miyu_base::notify::NotifySound::Question,
    );
    // A panel that cannot be shown is not a reason to abort the
    // turn: fall through to the same path a closed panel takes, so
    // the daemon gets an answer instead of the run dying on an
    // error the user cannot act on. The direct-mode handler has
    // always done this; this branch used to propagate instead.
    let asked = {
        let mut scroll = |delta: isize, panel_rows: u16| {
            if let Some(live) = live.as_deref_mut() {
                if let Some(screen) = live.screen.as_mut() {
                    let _ = screen.scroll_question_body(delta, panel_rows);
                }
            }
        };
        // 详情就地印的面自己把一问一答写成那一步的正文，面板退场别留东西。
        let leave_summary = !renderer.caps().detail_inline();
        crate::question_tui::ask_with(&request, Some(&mut scroll), leave_summary).unwrap_or_else(
            |err| miyu_base::question::QuestionResponse::Unavailable(err.to_string()),
        )
    };
    // 全屏下面板退场之后，下一帧就按缓冲恢复正文和输入区，
    // 问了什么、答了什么会一起消失（用户原话「回答完问题也没输出」）。
    // 写进缓冲它才算进了历史、回翻找得到。
    renderer.timeline_push_question(&request, &asked)?;
    // 这一步补进去了，现在才切：屏幕上的顺序就成了
    // 「…询问用户 → Worked for… → 问答块」，和实际发生的顺序一致。
    //
    // 静态时间线不切：一问一答已经是那一步的正文了，切了这一段就断
    // 成两截（问答块底下空一行、下一步没有连线接上来）。
    if !renderer.caps().commit_immediately {
        renderer.prepare_for_external_output()?;
        renderer.write_question_exchange(&request, &asked)?;
    }
    match asked {
        miyu_base::question::QuestionResponse::Answered(answers) => {
            send_ipc_command(
                paths,
                IpcCommand::AnswerQuestion {
                    question_id: ipc_text(&data, "question_id").to_string(),
                    answers,
                },
            )
            .await?;
            renderer.start_waiting()?;
        }
        // Nobody could be shown the panel — no tty, or it failed to
        // open. That is not the user calling the turn off, so the
        // question is resolved and the turn carries on; the tool
        // that asked finds out that nobody answered and can say so.
        miyu_base::question::QuestionResponse::Unavailable(_) => {
            let _ = send_ipc_command(
                paths,
                IpcCommand::CloseQuestion {
                    question_id: ipc_text(&data, "question_id").to_string(),
                },
            )
            .await;
        }
        // The terminal question UI maps its close gestures to
        // Cancelled; that one really is "stop this turn".
        miyu_base::question::QuestionResponse::Closed
        | miyu_base::question::QuestionResponse::Cancelled => {
            let _ = send_ipc_command(
                paths,
                IpcCommand::Cancel {
                    run_id: run_id.to_string(),
                },
            )
            .await;
        }
    }
    if let Some(live) = live.as_deref_mut() {
        live.external_output_active = false;
        live.output_cursor = cursor_position_or(live.output_cursor);
        live.resume_at(live.output_cursor)?;
    }
    Ok(())
}

/// 「她在等你回话」的 herdr 状态，作用域结束（不管怎么结束）就报回 `working`。
struct HerdrBlocked<'a>(Option<&'a crate::cli::repl::herdr::TurnGuard>);

impl<'a> HerdrBlocked<'a> {
    fn begin(
        guard: Option<&'a crate::cli::repl::herdr::TurnGuard>,
        question: Option<&str>,
    ) -> Self {
        if let Some(guard) = guard {
            guard.blocked(question);
        }
        Self(guard)
    }
}

impl Drop for HerdrBlocked<'_> {
    fn drop(&mut self) {
        if let Some(guard) = self.0 {
            guard.resumed();
        }
    }
}
