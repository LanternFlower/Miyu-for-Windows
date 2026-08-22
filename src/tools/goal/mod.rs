//! 同会话长任务目标：面向模型的三件套工具。
//!
//! 目标本身的真源在 SQLite（`state::conversation_db::goals`）。围绕它分四块：
//!
//! - **runtime**：只在进程里活的状态（armed、空转等待、待注入通知、续轮 run
//!   登记），集中在一张表里。armed 有意不落库——daemon 重启后必须由人
//!   `/goal resume` 重新授权，不然一次崩溃重启就能让机器在无人看管的情况下
//!   继续自己开轮。
//! - **prompt**：喂给模型的文案（续轮提示词、收尾指令、变更通知）。
//! - **command**：`/goal` 命令，REPL 与 WebUI 同一份实现。
//! - 本文件：工具注册与权限。**权限二元分立**，靠回合来源标记判定，而不是去
//!   扫会话事件猜：create/edit/pause/resume 只认人类发起的回合；
//!   complete/blocked 额外接受「恰好是当前目标的这一轮自动续轮」。

mod command;
mod prompt;
mod runtime;

pub use command::{session_goal_json, try_execute_goal_command};
pub use prompt::goal_round_prompt;
pub use runtime::{
    finish_run, forget_session, is_armed, is_awaiting_human, mark_run_productive, push_turn_notice,
    set_armed, set_awaiting_human, take_turn_notices, track_goal_run,
};

use super::{ToolRegistry, ToolSpec};
use crate::paths::MiyuPaths;
use crate::state::{GoalDenied, GoalRecord, StateStore};
use crate::tools::workspace::{self, TurnOrigin};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// 模型自报受阻的机械下限：同一阻塞至少要熬过这么多连续自动轮才收。
///
/// 没有这道闸，模型第一轮遇到点麻烦就能宣布「我被挡住了」，长任务退化成
/// 「起个头就收工」。人类直接授权不受此限——人说停就是停。
pub const BLOCKED_AFTER_CONSECUTIVE_ROUNDS: i64 = 3;

/// 续轮 run 的标签。
///
/// 走的是后台唤醒那条可见性通道（REPL 客户端靠它发现并挂上 daemon 自己发起
/// 的回合），但**不该像后台唤醒那样打一行表头**：长任务会连着跑几十轮，每轮
/// 顶一行只会把真正的输出挤散。REPL 和 WebUI 都拿这个常量识别它。
pub const GOAL_ROUND_LABEL: &str = "goal-round";

fn store(paths: &MiyuPaths) -> Result<StateStore> {
    StateStore::new(paths)
}

fn session_for_call() -> Result<String> {
    workspace::try_session()
        .map(|session| session.to_string())
        .ok_or_else(|| anyhow::anyhow!("goal tools require a session turn"))
}

fn goal_value(goal: Option<&GoalRecord>, session_id: &str) -> Value {
    match goal {
        None => json!({ "goal": null }),
        Some(goal) => json!({
            "goal": {
                "id": goal.goal_id,
                "revision": goal.revision,
                "objective": goal.objective,
                "phase": goal.phase.as_str(),
                "roundsStarted": goal.rounds_started,
                "maxGoalRounds": goal.max_rounds,
                "blockedReason": goal.blocked_code.as_ref().map(|code| json!({
                    "code": code,
                    "message": goal.blocked_message.clone().unwrap_or_default(),
                })),
            },
            "activation": if is_armed(session_id) { "armed" } else { "disarmed" },
        }),
    }
}

fn render_goal(goal: Option<&GoalRecord>, session_id: &str) -> String {
    goal_value(goal, session_id).to_string()
}

/// 严格 schema 的填充容错：模型常按「必填」的直觉把用不上的字段填成空串或 0，
/// 那等于没提供。
fn meaningful_text(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

fn meaningful_rounds(value: Option<i64>) -> Option<i64> {
    value.filter(|rounds| *rounds != 0)
}

/// 本轮是不是当前目标正在跑的那一轮。
///
/// 只看目标和轮号，**不看 revision**。这里回答的是「你是谁」——你是不是当前
/// 这一轮；而 revision 回答的是「你改的是哪个版本」，那由 CAS 在写入时把关。
///
/// 早先把 revision 也算进来，结果是：人在一轮跑到一半时 `/goal resume` 或
/// `edit`（这两个都会推进 revision），正在跑的那一轮就**永久失去了报完成的
/// 资格**——它明明还是当前轮，却只能拿到一句「complete 需要人类发起的回合」，
/// 和真正的原因毫无关系。
fn origin_matches_goal(origin: &TurnOrigin, goal: &GoalRecord) -> bool {
    matches!(origin, TurnOrigin::GoalRound { goal_id, round, .. }
        if *goal_id == goal.goal_id && *round == goal.rounds_started)
}

fn require_human(origin: &TurnOrigin, verb: &str) -> Result<()> {
    if matches!(origin, TurnOrigin::Human) {
        return Ok(());
    }
    bail!("goal {verb} requires a direct human turn (this turn was started automatically)");
}

pub fn register(registry: &mut ToolRegistry, paths: MiyuPaths) {
    let get_paths = paths.clone();
    registry.register(
        ToolSpec::new(
            "get_goal",
            "Read the current same-session goal: exact id/revision for compare-and-set, objective, phase, rounds used/limit, blocker when present, and whether autonomous continuation is armed. Call this before update_goal.",
            super::registry::empty_parameters(),
            move |_args| {
                let paths = get_paths.clone();
                async move {
                    let session = session_for_call()?;
                    let goal = store(&paths)?.goal(&session)?;
                    Ok(render_goal(goal.as_ref(), &session))
                }
            },
        )
        .with_always_loaded(false),
    );

    let create_paths = paths.clone();
    registry.register(
        ToolSpec::new(
            "create_goal",
            "Create one persisted same-session completion goal when the current direct human request is a long-running objective that should continue across autonomous goal rounds. Infer that intent from any language; do not use this for trivial single-turn work. Rejected on automatic turns.",
            json!({
                "type": "object",
                "properties": {
                    "objective": {"type": "string", "description": "The concrete completion objective inferred from the direct human request."},
                    "max_goal_rounds": {"type": "integer", "description": "Optional positive limit on automatic continuation rounds."}
                },
                "required": ["objective"],
                "additionalProperties": false
            }),
            move |args: Value| {
                let paths = create_paths.clone();
                async move {
                    let session = session_for_call()?;
                    require_human(&workspace::current_turn_origin(), "create")?;
                    let objective = args
                        .get("objective")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let max_rounds =
                        meaningful_rounds(args.get("max_goal_rounds").and_then(Value::as_i64));
                    let goal = store(&paths)?.create_goal(&session, objective, max_rounds)?;
                    set_armed(&session, true);
                    Ok(render_goal(Some(&goal), &session))
                }
            },
        )
        .writes()
        .with_always_loaded(false),
    );

    let update_paths = paths;
    registry.register(
        ToolSpec::new(
            "update_goal",
            "Update the exact current goal revision (call get_goal first and copy its goal_id/revision). Actions: edit | pause | resume | complete | blocked. edit/pause/resume require a direct human turn; during the current autonomous goal round, complete and blocked are also allowed. blocked needs blocked_reason and is mechanically rejected before the configured minimum consecutive rounds.",
            json!({
                "type": "object",
                "properties": {
                    "goal_id": {"type": "string", "description": "Exact id returned by get_goal."},
                    "revision": {"type": "integer", "description": "Exact positive revision returned by get_goal."},
                    "action": {"type": "string", "enum": ["edit", "pause", "resume", "complete", "blocked"]},
                    "objective": {"type": "string", "description": "Replacement objective; only with action edit."},
                    "max_goal_rounds": {"type": "integer", "description": "Replacement round cap; only with action edit."},
                    "blocked_reason": {"type": "string", "description": "Concrete blocking condition; required with action blocked."}
                },
                "required": ["goal_id", "revision", "action"],
                "additionalProperties": false
            }),
            move |args: Value| {
                let paths = update_paths.clone();
                async move { update_goal(&paths, args).await }
            },
        )
        .writes()
        .with_always_loaded(false),
    );
}

async fn update_goal(paths: &MiyuPaths, args: Value) -> Result<String> {
    let session = session_for_call()?;
    let origin = workspace::current_turn_origin();
    let store = store(paths)?;
    let goal_id = args
        .get("goal_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let revision = args.get("revision").and_then(Value::as_i64).unwrap_or(0);
    if goal_id.is_empty() || revision < 1 {
        bail!("goal_id must be non-empty and revision must be a positive integer (call get_goal)");
    }
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let objective = meaningful_text(args.get("objective").and_then(Value::as_str));
    let max_rounds = meaningful_rounds(args.get("max_goal_rounds").and_then(Value::as_i64));
    let blocked_reason = meaningful_text(args.get("blocked_reason").and_then(Value::as_str));

    match action {
        "edit" => {
            require_human(&origin, "edit")?;
            if blocked_reason.is_some() {
                bail!("blocked_reason is valid only with action blocked");
            }
            let goal = store.edit_goal(&session, &goal_id, revision, objective, max_rounds)?;
            Ok(render_goal(Some(&goal), &session))
        }
        "pause" | "resume" => {
            require_human(&origin, action)?;
            if objective.is_some() || max_rounds.is_some() || blocked_reason.is_some() {
                bail!(
                    "objective/max_goal_rounds are valid only with edit; \
                     blocked_reason only with blocked"
                );
            }
            let goal = if action == "pause" {
                let goal = store.pause_goal(&session, &goal_id, revision)?;
                set_armed(&session, false);
                goal
            } else {
                let goal = store.resume_goal(&session, &goal_id, revision)?;
                set_armed(&session, true);
                goal
            };
            Ok(render_goal(Some(&goal), &session))
        }
        "complete" | "blocked" => {
            // 权限：人类直接改，或者恰好是当前这一轮自主续轮。
            let current = store
                .goal(&session)?
                .ok_or_else(|| anyhow::anyhow!("{}", GoalDenied::NotFound))?;
            let autonomous = origin_matches_goal(&origin, &current);
            if !autonomous {
                require_human(&origin, action)?;
            }
            if objective.is_some() || max_rounds.is_some() {
                bail!("objective/max_goal_rounds are valid only with action edit");
            }
            if action == "complete" {
                if blocked_reason.is_some() {
                    bail!("blocked_reason is valid only with action blocked");
                }
                let goal = store.complete_goal(&session, &goal_id, revision)?;
                set_armed(&session, false);
                if autonomous {
                    push_turn_notice(&session, prompt::wrapup_text(&goal.objective, None));
                }
                Ok(render_goal(Some(&goal), &session))
            } else {
                let Some(reason) = blocked_reason else {
                    bail!("blocked_reason is required with action blocked");
                };
                // 机械下限只约束自主轮：人说停就是停。
                if autonomous && current.rounds_started < BLOCKED_AFTER_CONSECUTIVE_ROUNDS {
                    bail!(
                        "blocked requires at least {BLOCKED_AFTER_CONSECUTIVE_ROUNDS} \
                         consecutive goal rounds; current round is {}",
                        current.rounds_started
                    );
                }
                let goal =
                    store.block_goal(&session, &goal_id, revision, "model-reported", reason)?;
                set_armed(&session, false);
                if autonomous {
                    push_turn_notice(&session, prompt::wrapup_text(&goal.objective, Some(reason)));
                }
                Ok(render_goal(Some(&goal), &session))
            }
        }
        other => bail!("unknown goal action: {other}"),
    }
}
