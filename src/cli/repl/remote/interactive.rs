//! 交互式远端 REPL。
//!
//! 主循环：读输入 → 发 IPC → 收事件流 → 刷活动区，中间还要处理后台任务唤醒、
//! 排队消息、窗口变化。这是终端的日常路径。
//!
//! 事件分发表在这里维护，和 [`crate::cli::repl::live_turn`] 的本地泵是两份——
//! 加新事件时两边都要过一遍（08-17「footer 不刷新」就是漏了一边）。
//!
//! 09-17 拆分:循环外的十个状态收成 [`RemoteRepl`],斜杠命令住 `slash_*.rs`,发回合住 `submit.rs`;
//! 这里只剩「起 daemon、建状态、回放尾巴、主循环」。

use crate::cli::repl::editor::*;
use crate::cli::repl::input::*;
use crate::cli::repl::tail::*;
use crate::cli::*;

pub(in crate::cli) async fn run_remote_repl(paths: &MiyuPaths, mode: PersonaLane) -> Result<()> {
    let _cursor_restore = ReplCursorRestore;
    ipc::ensure_daemon(paths, None).await?;
    let refreshed = MiyuPaths::new()?;
    let paths = &refreshed;
    initialize_models_cache(paths);
    let config = AppConfig::load_or_default(paths)?;
    // The REPL resumes its own lane rather than the terminal session, so
    // reopening a REPL lands back where the last one left off while shell-hook
    // keeps talking to whatever session it was on.
    let (daemon_state, repl_session_data) = send_ipc_admin(
        paths,
        IpcCommand::GetReplSession {
            mode: mode.is_dev().then(|| "dev".to_string()),
        },
    )
    .await?;
    let active_session_id = daemon_state.session_id.clone();
    let history_state = StateStore::new(paths)?.pinned(&active_session_id);
    let history = load_repl_input_history(&history_state, paths)?;
    drop(history_state);
    let cumulative_tokens = state_cumulative(&daemon_state);
    // footer 的模型标签与思考程度必须同源:都从会话作用域配置推导。
    // 曾经 client 用全局配置,标签显示会话覆盖模型、·max 却算的全局
    // 模型,两边各说各话(验收#23)。
    let session_config = footer_config_for_session(paths, &config, &active_session_id);
    let mut footer = ReplFooterStatus::from_config(
        &session_config,
        daemon_state.context_tokens,
        cumulative_tokens,
    );
    let client = OpenAiCompatibleClient::from_config(&session_config, paths)?;
    let thinking_summary = client.thinking_variant_summary();
    footer.update_thinking_variant(thinking_summary.as_deref());
    footer.update_context_window(
        daemon_state.context_window,
        daemon_state.context_window_assumed,
    );
    let mut live_repl = LiveReplTail::new(mode, history.clone(), Vec::new(), footer.clone())?;
    // 空会话:挂 banner,Tab 可换车道;有过回合的会话直接是输入框。
    live_repl.set_session_empty(&config, paths, session_is_empty(paths, &active_session_id));
    let jobs_shared = spawn_jobs_poll_thread(paths.clone());
    let jobs_feed = JobsFeed::Shared(jobs_shared.clone());
    // 在 herdr 的 pane 里跑的话，侧栏这就多一行 `miyu`（不在就是 no-op）。
    // 带上会话 id：`herdr agent list` 会显示它，将来做「重启后恢复」也靠它指回来。
    herdr::report(herdr::HerdrState::Idle, None, Some(&active_session_id));
    herdr::set_terminal_title_for_session(paths, &active_session_id);

    // Terminal closed (SIGHUP) or process killed (SIGTERM): the graceful
    // exit path at the bottom never runs, so stop this session's background
    // jobs from a signal task before dying. SIGKILL still leaks them — the
    // daemon keeps those running and their completion wakes queue up.
    {
        let paths = paths.clone();
        let feed = jobs_shared.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let (Ok(mut hangup), Ok(mut terminate)) = (
                signal(SignalKind::hangup()),
                signal(SignalKind::terminate()),
            ) else {
                return;
            };
            tokio::select! {
                _ = hangup.recv() => {}
                _ = terminate.recv() => {}
            }
            // 后台任务归 daemon 管:前端死了任务照跑,完成后有唤醒
            // (验收:dsh 语义,前端退出不拖死会话任务)。
            let _ = (&paths, &feed);
            // 死之前把 herdr 那个 pane 的权威还回去，否则侧栏上一直挂着一个
            // 不存在的 miyu。`release` 是起进程、不等，这里要等它真的跑完再
            // `exit`——所以同步调一次而不是丢给线程。
            herdr::release_blocking();
            // SIGTERM 时终端往往还活着:process::exit 绕过 Drop,先尽力
            // 恢复 raw mode,否则用户的 shell 停在原始模式里。
            let _ = crossterm::terminal::disable_raw_mode();
            std::process::exit(0);
        });
    }

    // Redraw the tail of the session we just resumed. The tail is not on
    // screen yet (`rendered == false`), so `apply_output_frame` writes the
    // frame raw and re-reads the cursor — no layout budget applies and the
    // frame can be arbitrarily long.
    if config.display.repl_replay_turns > 0 {
        let replay_store = StateStore::new(paths)?.pinned(&active_session_id);
        match replay_store.session_replay(config.display.repl_replay_turns) {
            Ok(replays) if !replays.is_empty() => {
                // 全屏下按正文区的宽度排，不是整屏：左右各两列边距，按整屏排出来
                // 的东西会比可视区宽、被缓冲硬折一次。
                let (cols, _) = terminal::size().unwrap_or((80, 24));
                let cols = crate::cli::content_viewport()
                    .map(|(cols, _)| cols)
                    .unwrap_or(cols);
                // 混合模型池的「本次供应商 / 模型」按会话的池判(BUG-05)。
                let endpoint_line = show_mixed_model_endpoint(
                    &crate::cli::model_cmds::session_scoped_config(&replay_store, &config),
                    true,
                );
                let frame = session_replay_frame(
                    &replays,
                    mode,
                    &config,
                    usize::from(cols.max(1)),
                    endpoint_line,
                )?;
                live_repl.apply_output_frame(&frame)?;
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(error = %error, "session replay unavailable"),
        }
    }

    // 这条会话钉的模型被供应商下架了：daemon 已经退回全局池并把覆盖清掉，得说
    // 一声——不说的话 footer 上的模型悄悄换了人，看着像自己乱跳。放在回放之后，
    // 免得插在历史前面。
    if let Some(listed) = repl_session_data
        .get("stale_model_override")
        .and_then(|value| value.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join("、")
        })
        .filter(|listed| !listed.is_empty())
    {
        let note = t(
            "this session pinned models the providers no longer list ({}); back to the global pool",
            "这条会话钉的模型已不在供应商清单里（{}），已退回全局模型池",
        )
        .replace("{}", &listed);
        // 直接写进正文而不是走 `repl_note`：那条路在全屏下会变成 2.2 秒就散的
        // 浮层提示，而这是一次性的状态变更（钉的池没了），晚一眼看过去也得还在。
        live_repl.apply_output_frame(format!("\x1b[2m{note}\x1b[0m\n\n").as_bytes())?;
    }

    let mut repl = RemoteRepl {
        paths: refreshed,
        config,
        mode,
        active_session_id,
        history,
        cumulative_tokens,
        footer,
        live_repl,
        jobs_shared,
        jobs_feed,
    };
    let outcome = repl.run().await;
    // 正常退出也要把 pane 的权威还回去（信号那条路另有一处）。跑不跑得成都要还，
    // 所以放在 `?` 之外。
    herdr::release_blocking();
    outcome
}

/// 一个远端 REPL 会话跑着时的全部状态:配置、车道、当前会话、输入历史、footer 读数、
/// 活动区与后台任务源。斜杠命令与发回合都是它的方法。
pub(super) struct RemoteRepl {
    pub(super) paths: MiyuPaths,
    pub(super) config: AppConfig,
    pub(super) mode: PersonaLane,
    pub(super) active_session_id: String,
    pub(super) history: Vec<ReplHistoryEntry>,
    pub(super) cumulative_tokens: TurnTokens,
    pub(super) footer: ReplFooterStatus,
    pub(super) live_repl: LiveReplTail,
    pub(super) jobs_shared: std::sync::Arc<SharedJobsFeed>,
    pub(super) jobs_feed: JobsFeed,
}

/// 斜杠命令或发回合之后主循环该怎么走。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LoopStep {
    Continue,
    Break,
}

impl RemoteRepl {
    /// 主循环:读输入 → 斜杠命令或发回合,直到退出。
    pub(super) async fn run(&mut self) -> Result<()> {
        loop {
            // Keep the poll thread's session filter in step with /new & /session.
            *self.jobs_shared.repl_session.lock().unwrap() = Some(self.active_session_id.clone());
            // 目标提示是输入循环里一秒一拍自己往前走的（`tick_goal_hint` 写在
            // tail 的 footer 上），这儿先收回来：下面那次 set_footer 是整份覆盖，
            // 不收就拿上一轮的旧值盖掉它，一秒后才由下一拍补上——屏幕上是闪一下。
            self.footer.goal = self.live_repl.footer.goal.clone();
            self.live_repl.set_footer(self.footer.clone());
            let (next_mode, input, images, history_entry) = match read_live_repl_input(
                &mut self.live_repl,
                &self.paths,
                &self.jobs_feed,
                Some(&self.active_session_id),
            )? {
                LiveReplOutcome::Exit => break,
                LiveReplOutcome::StopJob { job_id } => {
                    let result = send_ipc_command(
                        &self.paths,
                        IpcCommand::StopJob {
                            job_id: job_id.clone(),
                        },
                    )
                    .await;
                    let note = match result {
                        Ok(_) => {
                            // 压住它：紧接着那次轮询还带着它，状态行会闪一下。
                            self.live_repl
                                .suppress_jobs(std::iter::once(job_id.as_str()));
                            let remaining: Vec<miyu_engine::tools::jobs::JobOverview> = self
                                .live_repl
                                .jobs
                                .iter()
                                .filter(|job| job.job_id != job_id)
                                .cloned()
                                .collect();
                            self.live_repl.set_jobs(remaining);
                            t("background task stopped", "已停止这个后台任务")
                        }
                        Err(_) => t("could not stop the task", "没能停掉这个后台任务"),
                    };
                    repl_note(&mut self.live_repl, &format!("\x1b[2m{note}\x1b[0m\n"))?;
                    continue;
                }
                LiveReplOutcome::StopJobs => {
                    let stopped = match repl_ipc_admin(
                        &self.paths,
                        &mut self.live_repl,
                        IpcCommand::StopSessionJobs {
                            session_id: self.active_session_id.clone(),
                        },
                    )
                    .await?
                    {
                        Some((_, data)) => data
                            .get("stopped")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                        None => 0,
                    };
                    // Drop the strip now instead of waiting out the ~1s jobs poll:
                    // every job of this session was just stopped, so an empty strip
                    // is the truth.
                    //
                    // 光清是不够的：紧接着那次轮询拿到的还是停之前的快照，状态行会
                    // 再冒出来一下——先把这些 id 压住，等轮询里真的没有了再放开。
                    let stopped_ids: Vec<String> = self
                        .live_repl
                        .jobs
                        .iter()
                        .map(|job| job.job_id.clone())
                        .collect();
                    self.live_repl
                        .suppress_jobs(stopped_ids.iter().map(String::as_str));
                    self.live_repl.set_jobs(Vec::new());
                    repl_note(
                        &mut self.live_repl,
                        &format!(
                            "\x1b[2m{}\x1b[0m\n",
                            if is_zh() {
                                format!("已停止 {stopped} 个后台任务")
                            } else {
                                format!("stopped {stopped} background task(s)")
                            }
                        ),
                    )?;
                    continue;
                }
                LiveReplOutcome::FollowWake {
                    run_id,
                    label,
                    from_start,
                } => {
                    if let Err(error) = follow_wake_run(
                        &self.paths,
                        &mut self.live_repl,
                        &run_id,
                        &label,
                        from_start,
                        &self.active_session_id,
                        &self.jobs_feed,
                        &self.jobs_shared,
                    )
                    .await
                    {
                        tracing::debug!(error = %error, "wake follow detached with an error");
                    }
                    continue;
                }
                LiveReplOutcome::Submit(next_mode, input, images, entry) => {
                    (next_mode, input, images, entry)
                }
                LiveReplOutcome::SwitchMode(next) => {
                    match switch_repl_lane(
                        &self.paths,
                        &self.config,
                        next,
                        &mut self.active_session_id,
                        &mut self.history,
                        &mut self.live_repl,
                        &mut self.footer,
                        &mut self.cumulative_tokens,
                    )
                    .await
                    {
                        Ok(()) => self.mode = next,
                        Err(error) => {
                            // 切不过去就留在原车道,把颜色也换回来。
                            self.live_repl.set_mode(self.mode);
                            repl_note(
                                &mut self.live_repl,
                                &format!(
                                    "\x1b[31m{}: {error:#}\x1b[0m\n",
                                    t("could not switch self.mode", "切换模式失败")
                                ),
                            )?;
                        }
                    }
                    continue;
                }
            };
            self.mode = next_mode;
            let input = input.trim();
            if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                break;
            }
            let (slash_command, command_args) = match parse_repl_input(input) {
                ReplInput::Chat => (None, ""),
                ReplInput::Slash(command, args) => (Some(command), args),
            };
            // 第一条消息发出去,会话就不空了:banner 撤、模式钉死。
            if submission_leaves_lobby(input) {
                self.live_repl
                    .set_session_empty(&self.config, &self.paths, false);
            }
            if let Some(command) = slash_command {
                // 命令也进上方向键历史：`/goal 长长的目标` 打错一个字重敲一遍，
                // 和重敲一条消息一样冤。落盘历史仍只收消息（命令是操作不是对话）。
                push_history_capped(&mut self.history, ReplHistoryEntry::plain(input));
                self.live_repl
                    .editor
                    .record_history(ReplHistoryEntry::plain(input));
                let spec = repl_command_spec(command);
                if spec.arg_hint.is_empty() && !command_args.trim().is_empty() {
                    repl_note(
                        &mut self.live_repl,
                        &format!(
                            "\x1b[2m{}: {}\x1b[0m\n",
                            t("this command takes no arguments", "该命令不接受参数"),
                            spec.name
                        ),
                    )?;
                    continue;
                }
                let step = match command {
                    ReplSlashCommand::Exit => LoopStep::Break,
                    ReplSlashCommand::Help => self.cmd_help().await?,
                    ReplSlashCommand::Stt => self.cmd_stt().await?,
                    ReplSlashCommand::History => self.cmd_history().await?,
                    ReplSlashCommand::Clear => self.cmd_clear().await?,
                    ReplSlashCommand::New => self.cmd_new(command_args).await?,
                    ReplSlashCommand::Session => self.cmd_session(command_args).await?,
                    ReplSlashCommand::Dev => self.cmd_lane(PersonaLane::Dev).await?,
                    ReplSlashCommand::Normal => self.cmd_lane(PersonaLane::Active).await?,
                    ReplSlashCommand::Rename => self.cmd_rename(command_args).await?,
                    ReplSlashCommand::Delete => self.cmd_delete(command_args).await?,
                    ReplSlashCommand::Sandbox => self.cmd_sandbox(command_args).await?,
                    ReplSlashCommand::Goal => self.cmd_goal(command_args).await?,
                    ReplSlashCommand::Usage => self.cmd_usage().await?,
                    ReplSlashCommand::Persona => self.cmd_persona(command_args).await?,
                    ReplSlashCommand::Models => self.cmd_models(command_args).await?,
                    ReplSlashCommand::Config => self.cmd_config().await?,
                    ReplSlashCommand::Effort => self.cmd_effort(command_args).await?,
                    ReplSlashCommand::Undo => self.cmd_undo().await?,
                    ReplSlashCommand::Pop => self.cmd_pop(command_args).await?,
                    ReplSlashCommand::Compact => self.cmd_compact().await?,
                    ReplSlashCommand::ResetMemory => self.cmd_reset_memory().await?,
                    ReplSlashCommand::ResetAllMemory => self.cmd_reset_all_memory().await?,
                    ReplSlashCommand::Reset => self.cmd_reset().await?,
                    ReplSlashCommand::Wipe => self.cmd_wipe().await?,
                };
                if step == LoopStep::Break {
                    break;
                }
                continue;
            }
            if input.is_empty() {
                continue;
            }
            self.submit_chat(input, &images, &history_entry).await?;
        }
        Ok(())
    }
}
