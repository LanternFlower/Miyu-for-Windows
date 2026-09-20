//! 回合跑着时的 `/models` / `/session` 面板：**寄宿在回合循环里**，不分离。
//!
//! 原来这两条命令在回合中走「分离 → 开面板 → 挂回来」：每分离一次就把当前渲染
//! 段收尾定稿吐一行小结，挂回来又是全新的渲染器重新计时——用户 09-20 实测
//! 「我每 /models 一次就会多一行 Worked for」，三次就是三行。试过分离时不收尾
//! 定稿，更糟：每次在屏幕上留一个冻住的「思考中」块。段落被切开这件事本身躲
//! 不掉，只有不分离才没有接缝。
//!
//! 这里的做法和她反问时弹的提问面板（`question_flow`）是同一条路：面板是个同步
//! 事件循环，只泵键盘；这段时间 IPC 事件在 socket 里排队（daemon 那头的转发
//! 器带回放缓冲，读得慢不会把它卡住，见 `web/server.rs::follow_run` 的
//! `Lagged` 分支），面板收掉之后回合循环接着收、接着画，**渲染器始终是同一个**。
//! 回合在面板开着时跑完也不用特殊处理：`run.completed` 就排在队里，面板一收
//! 照常收尾。
//!
//! 顺带说明：这条路**不**让「面板开着时正文继续流」——面板下方的正文区是视口
//! 语义（和提问面板共用 `scroll_question_body`），窗口不跟着长。09-20 评估后
//! 用户裁定不做。
//!
//! 两条泵（自己起的轮 `remote::one_shot`、挂上去跟的轮 `wake`）共用这一份。

use crate::cli::repl::tail::*;
use crate::cli::*;

/// 面板收掉之后回合循环该怎么走。
pub(in crate::cli) enum HostedPanel {
    /// 留在这一轮上接着看（取消、或 `/models` 已就地落地）。
    Stayed,
    /// `/session` 挑了另一条会话：回合循环收口，把它带回 `RemoteRepl` 去切。
    SwitchSession(ipc::SessionState),
}

/// 在回合循环里跑一个面板。只认 `during_turn` 判成 `Panel` 的那两条命令。
///
/// 调用方要保证是全屏 TUI（`live.screen.is_some()`）：行内 REPL 没有面板，
/// 那条路仍走分离。
pub(in crate::cli) async fn host_panel(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    renderer: &mut render::StreamRenderer,
    command: ReplSlashCommand,
    session_id: &str,
) -> Result<HostedPanel> {
    // 先把攒着的那一帧落到屏上：面板压在正文之上画，底下得是最新的画面，
    // 收掉面板整屏重画时也才不会少一截。
    live.apply_renderer_frame(renderer)?;
    match command {
        ReplSlashCommand::Models => {
            models_panel(paths, live, session_id).await?;
            Ok(HostedPanel::Stayed)
        }
        ReplSlashCommand::Session => {
            // 列表、挑选、Ctrl+D 当场删、删到自己头上退到兜底会话——都在
            // `repl_pick_session` 里，和空闲时敲 `/session` 是同一份。
            let mode = live.mode();
            Ok(
                match repl_pick_session(paths, live, mode, session_id).await? {
                    Some(state) => HostedPanel::SwitchSession(state),
                    None => HostedPanel::Stayed,
                },
            )
        }
        _ => Ok(HostedPanel::Stayed),
    }
}

/// `/models` 面板 + 落盘 + footer：和 `RemoteRepl::cmd_models` 的全屏路一个流程。
///
/// 出错不往外抛（没配模型、IPC 失败）：那是这条命令的事，不该把正在看的这一轮
/// 打断，一行红字说清楚就行——空闲时的 `cmd_models` 也是这么收的。
async fn models_panel(paths: &MiyuPaths, live: &mut LiveReplTail, session_id: &str) -> Result<()> {
    let changed = match super::pickers::pick_models_panel(paths, live, session_id).await {
        Ok(changed) => changed,
        Err(error) => {
            repl_note(live, &format!("\x1b[31m{error:#}\x1b[0m\n"))?;
            return Ok(());
        }
    };
    if !changed {
        return Ok(());
    }
    // footer 上的模型标签当场换掉（空闲时 `/models` 也是选完就换）。回合还在
    // 跑：运行转轮与右上角的目标提示都是 footer 上的活字段，重算出来的那份
    // 没有它们，得从旧的搬过来，不然转轮熄一下、提示闪一下。
    let config = AppConfig::load_or_default(paths)?;
    let (mut footer, _) = session_footer_status(paths, &config, live, session_id).await?;
    footer.goal = live.footer.goal.clone();
    footer.running_spinner = live.footer.running_spinner;
    live.refresh_footer(footer)?;
    // `RemoteRepl` 手里那份 footer 还是旧模型，回合一结束会盖回来；让它先重算。
    live.session_footer_stale = true;
    repl_note(
        live,
        &format!(
            "\x1b[2m{}\x1b[0m\n",
            t(
                "session model updated; takes effect from the next turn",
                "会话模型已更新，下一轮生效"
            )
        ),
    )?;
    Ok(())
}
