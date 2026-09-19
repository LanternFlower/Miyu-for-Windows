//! 远端 REPL 的会话类斜杠命令:/new /session /rename /delete /sandbox /goal。09-17 从 `run_remote_repl` 抽出。

use super::interactive::{LoopStep, RemoteRepl};
use crate::cli::*;

impl RemoteRepl {
    pub(super) async fn cmd_new(&mut self, command_args: &str) -> Result<LoopStep> {
        let name = command_args.trim();
        let Some((_, data)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::CreateSession {
                name: (!name.is_empty()).then(|| name.to_string()),
                switch: false,
                kind: None,
                mode: self.mode.is_dev().then(|| "dev".to_string()),
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        let Some(session_id) = data
            .get("session")
            .and_then(|session| session.get("session_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
        else {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[31m{}\x1b[0m\n",
                    t("created session has no id", "新会话缺少 ID")
                ),
            )?;
            return Ok(LoopStep::Continue);
        };
        let Some((state, _)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::GetSessionState {
                target: miyu_core::ipc::SessionRef::Id { id: session_id },
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        apply_repl_session_switch(
            &self.paths,
            &self.config,
            self.mode,
            &state,
            &mut self.active_session_id,
            &mut self.history,
            &mut self.live_repl,
            &mut self.footer,
            &mut self.cumulative_tokens,
        )
        .await?;
        Ok(LoopStep::Continue)
    }

    /// `/dev` 与 `/normal`：去那条车道上的会话（车道指针指向的那条，
    /// `GetReplSession{dev|None}`——`/dev` 和 `miyu dev` 启动拿到的是同一条），**不是**
    /// 把当前会话改成另一个模式：模式钉在会话上（daemon `turn_mode_for_session`），
    /// 换模式就是换会话。所以它不受 Tab 那条「空会话才能换车道」的限制。已经在
    /// 那条车道上就只提示一句。
    pub(super) async fn cmd_lane(&mut self, lane: PersonaLane) -> Result<LoopStep> {
        if self.mode == lane {
            let note = if lane.is_dev() {
                t(
                    "already in dev mode; /normal goes back",
                    "已经在开发模式；/normal 回普通模式",
                )
            } else {
                t(
                    "already in normal mode; /dev goes to the dev lane",
                    "已经在普通模式；/dev 去开发模式",
                )
            };
            repl_note(&mut self.live_repl, &format!("\x1b[2m{note}\x1b[0m\n"))?;
            return Ok(LoopStep::Continue);
        }
        let Some((state, _)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::GetReplSession {
                mode: lane.is_dev().then(|| "dev".to_string()),
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        self.switch_to_session(&state).await?;
        Ok(LoopStep::Continue)
    }

    /// 切到一条会话，**车道跟着会话走**：目标是另一侧（普通 ↔ 开发）的会话，
    /// 输入框竖条、banner、footer 一并换过去——不然会话已经跑在 dev 人格上，
    /// 屏幕上还是普通模式的样子（初诊 §五 ③）。老 daemon 不报 `mode` 就留在原车道。
    pub(super) async fn switch_to_session(&mut self, state: &ipc::SessionState) -> Result<()> {
        let lane = match state.mode.as_str() {
            "dev" => PersonaLane::Dev,
            "normal" => PersonaLane::Active,
            _ => self.mode,
        };
        if lane != self.mode {
            // 先换色再切：切换的回执行和输入框竖条都按新模式画（和 Tab 换车道一样）。
            self.live_repl.set_mode(lane);
        }
        apply_repl_session_switch(
            &self.paths,
            &self.config,
            lane,
            state,
            &mut self.active_session_id,
            &mut self.history,
            &mut self.live_repl,
            &mut self.footer,
            &mut self.cumulative_tokens,
        )
        .await?;
        self.mode = lane;
        Ok(())
    }

    pub(super) async fn cmd_session(&mut self, command_args: &str) -> Result<LoopStep> {
        let arg = command_args.trim();
        let state = if arg.is_empty() {
            match repl_pick_session(
                &self.paths,
                &mut self.live_repl,
                self.mode,
                &self.active_session_id,
            )
            .await?
            {
                Some(state) => state,
                None => return Ok(LoopStep::Continue),
            }
        } else {
            let target =
                match resolve_repl_session_target(&self.paths, &mut self.live_repl, self.mode, arg)
                    .await?
                {
                    Some(target) => target,
                    None => return Ok(LoopStep::Continue),
                };
            match repl_get_session_switch(
                &self.paths,
                &mut self.live_repl,
                target,
                &self.active_session_id,
            )
            .await?
            {
                Some(state) => state,
                None => return Ok(LoopStep::Continue),
            }
        };
        self.switch_to_session(&state).await?;
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_rename(&mut self, command_args: &str) -> Result<LoopStep> {
        let name = command_args.trim().to_string();
        if name.is_empty() {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t("usage: /rename <name>", "用法：/rename <新名称>")
                ),
            )?;
            return Ok(LoopStep::Continue);
        }
        if repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::RenameSession {
                target: miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
                name: name.clone(),
            },
        )
        .await?
        .is_some()
        {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}: {name}\x1b[0m\n",
                    t("session renamed", "会话已重命名")
                ),
            )?;
        }
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_delete(&mut self, command_args: &str) -> Result<LoopStep> {
        let arg = command_args.trim();
        let target = if arg.is_empty() {
            miyu_core::ipc::SessionRef::Id {
                id: self.active_session_id.clone(),
            }
        } else {
            match resolve_repl_session_target(&self.paths, &mut self.live_repl, self.mode, arg)
                .await?
            {
                Some(target) => target,
                None => return Ok(LoopStep::Continue),
            }
        };
        let Some(target_state) =
            repl_get_session_state(&self.paths, &mut self.live_repl, target).await?
        else {
            return Ok(LoopStep::Continue);
        };
        let deleted_active = target_state.session_id == self.active_session_id;
        if !confirm_inline(
            &mut self.live_repl,
            t(
                "delete this session and all of its self.history?",
                "确认删除该会话及其全部历史？",
            ),
        )? {
            repl_note(
                &mut self.live_repl,
                &format!("\x1b[2m{}\x1b[0m\n", t("cancelled", "已取消")),
            )?;
            return Ok(LoopStep::Continue);
        }
        let Some((_, _)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::DeleteSession {
                target: miyu_core::ipc::SessionRef::Id {
                    id: target_state.session_id,
                },
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        repl_note(
            &mut self.live_repl,
            &format!("\x1b[2m{}\x1b[0m", t("session deleted", "会话已删除")),
        )?;
        if deleted_active {
            let Some(state) =
                repl_fallback_session_state(&self.paths, &mut self.live_repl, self.mode).await?
            else {
                return Ok(LoopStep::Continue);
            };
            apply_repl_session_switch(
                &self.paths,
                &self.config,
                self.mode,
                &state,
                &mut self.active_session_id,
                &mut self.history,
                &mut self.live_repl,
                &mut self.footer,
                &mut self.cumulative_tokens,
            )
            .await?;
        }
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_sandbox(&mut self, command_args: &str) -> Result<LoopStep> {
        let (arg, allow_read) =
            miyu_core::slash_commands::take_repl_flag(command_args, "--allow-read");
        if arg.is_empty() && allow_read {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[31m{}\x1b[0m\n",
                    t(
                        "usage: /sandbox <path> [--allow-read]",
                        "用法:/sandbox <路径> [--allow-read]"
                    )
                ),
            )?;
            return Ok(LoopStep::Continue);
        }
        if arg.is_empty() {
            let Some(state) = repl_get_session_state(
                &self.paths,
                &mut self.live_repl,
                miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
            )
            .await?
            else {
                return Ok(LoopStep::Continue);
            };
            let note = match state.sandbox {
                Some(root) => format!(
                    "\x1b[2m{}: {root}\n{}: {}\n{}: {}\x1b[0m\n",
                    t("sandbox root", "沙盒根"),
                    t("writable", "可写"),
                    state.sandbox_writable.join(", "),
                    t("readable", "可读"),
                    state.sandbox_readable.join(", "),
                ),
                None => format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t(
                        "no sandbox bound; using the client working directory, nothing confined",
                        "未绑定沙盒;使用客户端当前目录,不设限"
                    )
                ),
            };
            repl_note(&mut self.live_repl, &note)?;
            return Ok(LoopStep::Continue);
        }
        if arg.eq_ignore_ascii_case("clear") {
            if repl_ipc_admin(
                &self.paths,
                &mut self.live_repl,
                IpcCommand::SetSandbox {
                    target: miyu_core::ipc::SessionRef::Id {
                        id: self.active_session_id.clone(),
                    },
                    root: None,
                    allow_read: false,
                },
            )
            .await?
            .is_some()
            {
                repl_note(
                    &mut self.live_repl,
                    &format!(
                        "\x1b[2m{}\x1b[0m\n",
                        t(
                            "sandbox unbound; later turns run unconfined",
                            "已解绑沙盒;之后的回合不设限"
                        )
                    ),
                )?;
            }
            return Ok(LoopStep::Continue);
        }
        let path = match std::fs::canonicalize(expand_tilde(arg)) {
            Ok(path) => path,
            Err(error) => {
                repl_note(
                    &mut self.live_repl,
                    &format!(
                        "\x1b[31m{}: {arg} ({error})\x1b[0m\n",
                        t("invalid sandbox path", "无效的沙盒路径")
                    ),
                )?;
                return Ok(LoopStep::Continue);
            }
        };
        if repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::SetSandbox {
                target: miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
                root: Some(path.clone()),
                allow_read,
            },
        )
        .await?
        .is_some()
        {
            // 读放开是把「防提示注入读走密钥」那一半关掉,回执得说清楚
            // ——网络本来就不在 Landlock 管辖内。
            let scope = if allow_read {
                t(
                    "later turns write only inside it (plus /tmp and the configured dirs); reading is unrestricted, including ~/.ssh and your API keys",
                    "之后的回合只能往这里面写(外加 /tmp 与配置里的目录);读不设限,~/.ssh 与 API key 也读得到",
                )
            } else {
                t(
                    "later turns read and write only inside it (plus /tmp and the configured toolchain dirs)",
                    "之后的回合只能在这里面读写(外加 /tmp 与配置里的工具链目录)",
                )
            };
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}: {}\n{scope}\x1b[0m\n",
                    t("sandbox bound", "已绑定沙盒"),
                    path.display(),
                ),
            )?;
        }
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_goal(&mut self, command_args: &str) -> Result<LoopStep> {
        // 走 IPC 而不是直连库：目标本身在库里，但「是否自动续跑」
        // 驻在 daemon 内存，REPL 进程自己设那个标记，续轮驱动器
        // 根本看不见。
        let Some((_, data)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::Goal {
                target: miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
                input: command_args.to_string(),
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        // 终端里没有 WebUI 那条常驻状态行，所以这里必须回一句
        // ——设完目标到第一轮真正开跑之间有一段静默（驱动器要等
        // 会话空下来），一个字都不说的话，用户只会以为命令没生效。
        // 但也就一句：状态、轮次这些留给 `/goal` 自己去查。
        let text = data
            .get("text")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        // 光敲 `/goal edit` 到不了这里：输入泵在提交前就原地变身
        // 成「/goal edit <当前目标>」（`prefill_goal_edit_input`），
        // 只有没目标时才落进来打提示。
        let summary = if command_args.trim().is_empty() {
            text.to_string()
        } else {
            // 多行的详情压成一句：命令回执不该占半屏。
            text.lines().next().unwrap_or_default().to_string()
        };
        // 输入框右上角那行 `/goal …` 当场跟上：命令的回执只说一句话，状态是否
        // 真的换了（设上了、暂停了、清掉了）要看那行提示变没变。回执里带着命令
        // 执行完之后的目标状态，直接用。
        //
        // 写的是轮询线程那份快照而不是 footer：空闲循环每一拍都拿快照去盖
        // footer，不同步的话刚清掉的目标会自己回来待满一秒。
        let goal = goal_hint_from_admin_data(&data);
        // 目标要开跑了就撤大厅：接下来的续轮会往屏幕上写正文，而大厅那层星空
        // 是盖在正文之上的——不撤的话第一轮跑完了屏幕上还是一片星空，什么都
        // 没有（用户 09-19 实测：空会话里 `/goal` 建目标不退出大厅）。
        //
        // 只在「真的会自己往前跑」时撤：`/goal` 查状态、`/goal pause`、
        // `/goal clear` 都不该把大厅弄没。
        if goal.as_ref().is_some_and(miyu_core::ipc::GoalHint::running) {
            self.live_repl
                .set_session_empty(&self.config, &self.paths, false);
        }
        self.jobs_feed.set_goal(goal.clone());
        self.live_repl.tick_goal_hint(goal)?;
        // 暗色 + 图标：这是系统回执，不是模型正文，得和邻居们
        // （工作目录绑定、后台任务表头）长得一族。单个 \n 收尾，
        // 和它们一致——多一个就空两行。
        repl_note(&mut self.live_repl, &format!("\x1b[2m◎ {summary}\x1b[0m\n"))?;
        Ok(LoopStep::Continue)
    }
}
