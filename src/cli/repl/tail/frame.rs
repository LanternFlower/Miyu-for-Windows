//! 终端画面的记账与重绘。
//!
//! `TerminalFrameTracker` 解析我们自己发出去的转义序列（走 vte），据此推算终端
//! 现在停在第几行——**不能每次都去问终端**（ESC[6n 要等回答，在流式输出里会
//! 卡住）。
//!
//! 正文里出现过 kitty 图片之后，腾地方的滚动改走 `queue_lifted_frame`：
//! 整屏滚而不是受限区滚，原因见那里的注释。

use crate::cli::repl::tail::*;

/// 正文里有 kitty 图片时，把帧写进页内的另一种办法：**帧本身一行都不滚**。
///
/// kitty 的 Unicode 占位符图片不是钉在坐标上的，而是每一帧扫描可见行、给每个
/// 占位符行生成一条一行高的引用；屏内的行只在脏了才重扫，**历史区的行每帧都
/// 重扫**，重扫前只清「本行当前 row」上的旧引用。受限区（DECSTBM）滚动时 kitty
/// 只搬完全落在页内的引用（`scroll_filter_margins_func`），历史区那些 row 为负
/// 的一律不动；而用户滚上去看历史时视口是钉住的（scrolled_by 逐次 +1），历史行
/// 下一帧在 row-1 重扫，旧 row 上那条引用没人清、又没被搬，就画在比原位低一行
/// 的地方——正文每滚一次多一条，这就是「图片切片一条条往下复制」。kitty 默认开着
/// pixel_scroll，每帧还会多扫视口上方一行，所以就算没滚上去看，图片底行刚进历史
/// 时也会留下一条。整屏滚动没有页边距，走 `scroll_filter_func`，负 row 的引用
/// 一起搬，干净（`testkit/kitty-image/ghost_probe.py` 三个变体的实测）。
///
/// 做法：帧要滚 k 行，就先整屏滚 k 行（活动区跟着上去），再在页底下面插回 k 行
/// 把活动区推回原位——页内的效果与受限区滚 k 行逐格相同，滚出去的也同样进
/// scrollback——然后把帧写在比原来高 k 行的位置，它落到页底时正好写完。帧里的
/// 滚动点由 `FrameScroll` 给出；一次最多只能抬「光标所在行数」那么多，帧比页
/// 还高时就按滚动点切成几段，每段照此办理。光标已在最顶行还要滚（页只有一行）
/// 才退回受限区滚动。
pub(in crate::cli) fn queue_lifted_frame(
    transaction: &mut Vec<u8>,
    frame: &[u8],
    frame_start: (u16, u16),
    bottom: u16,
    leading_scroll: u16,
    scrolls: &[FrameScroll],
    terminal_rows: u16,
) -> Result<()> {
    let last_row = terminal_rows.saturating_sub(1);
    let region = format!("\x1b[1;{}r", bottom.saturating_add(1));
    let queue_lift = |transaction: &mut Vec<u8>, lines: u16| -> Result<()> {
        queue!(transaction, Print("\x1b[r"), MoveTo(0, last_row))?;
        for _ in 0..lines {
            queue!(transaction, Print("\n"))?;
        }
        queue!(
            transaction,
            MoveTo(0, bottom.saturating_add(1).saturating_sub(lines)),
            Print(format!("\x1b[{lines}L"))
        )?;
        Ok(())
    };
    // 开头那几行「光标已经掉到页底之下」的滚动不消耗帧的内容,单独抬。
    if leading_scroll > 0 {
        queue_lift(transaction, leading_scroll)?;
    }
    let (mut col, mut row) = frame_start;
    let mut position = 0usize;
    let mut next_scroll = 0usize;
    loop {
        let remaining = scrolls.len().saturating_sub(next_scroll);
        let lift = remaining.min(usize::from(row));
        if lift == 0 {
            queue!(transaction, Print(&region), MoveTo(col, row))?;
            transaction.extend_from_slice(frame.get(position..).unwrap_or_default());
            queue!(transaction, Print("\x1b[r"))?;
            return Ok(());
        }
        let lift_rows = lift.min(usize::from(u16::MAX)) as u16;
        queue_lift(transaction, lift_rows)?;
        row = row.saturating_sub(lift_rows);
        let last = scrolls[next_scroll + lift - 1];
        let end = last.end.clamp(position, frame.len());
        queue!(transaction, Print(&region), MoveTo(col, row))?;
        transaction.extend_from_slice(&frame[position..end]);
        queue!(transaction, Print("\x1b[r"))?;
        position = end;
        col = last.col_after;
        row = bottom;
        next_scroll += lift;
    }
}

impl LiveReplTail {
    pub(in crate::cli) fn suspend(&mut self) -> Result<()> {
        // 全屏：清屏把光标交到顶上，选择器 / 提问面板 / 图片就当自己拿到了
        // 一块空屏。它们打的是普通 ANSI，在备用屏上一样显示，不必退出。
        if let Some(screen) = &mut self.screen {
            screen.suspend()?;
            self.rendered = false;
            return Ok(());
        }
        if !self.rendered {
            return Ok(());
        }
        let mut stdout = io::stdout();
        let (_, terminal_rows) = terminal::size().unwrap_or((80, 24));
        for offset in 0..self.tail_rows {
            let row = self.tail_start.saturating_add(offset);
            if row >= terminal_rows {
                break;
            }
            queue!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        }
        queue!(stdout, MoveTo(self.output_cursor.0, self.output_cursor.1))?;
        stdout.flush()?;
        self.rendered = false;
        Ok(())
    }

    pub(in crate::cli) fn resume(&mut self) -> Result<()> {
        if let Some(screen) = &mut self.screen {
            screen.resume(true);
            let cursor = self.output_cursor;
            return self.resume_at(cursor);
        }
        self.resume_at(cursor_position_or(self.output_cursor))
    }

    /// 重挂活动区。保守口径：假定中间可能有外部输出，整屏擦一次。
    pub(in crate::cli) fn resume_at(&mut self, cursor: (u16, u16)) -> Result<()> {
        self.resume_at_inner(cursor, false)
    }

    /// 同上，但这一帧是**自己写的**（流式输出、拖选重画）——屏幕没被别人动过，
    /// 走逐行 diff。热路径上省掉整屏擦是「光标不闪、拖选不卡」的关键。
    pub(in crate::cli) fn resume_at_own(&mut self, cursor: (u16, u16)) -> Result<()> {
        self.resume_at_inner(cursor, true)
    }

    fn resume_at_inner(&mut self, (output_col, output_row): (u16, u16), own: bool) -> Result<()> {
        let (cols, terminal_rows) = terminal::size().unwrap_or((80, 24));
        let terminal_rows = terminal_rows.max(1);
        let editor_rows = repl_input_rendered_rows(
            &self.editor.input,
            self.editor.raw_pasted_lines,
            false,
            usize::from(cols),
        );
        let mut queue_lines =
            queued_prompt_lines(&self.queued, self.editor.mode, usize::from(cols));
        let queue_gap = u16::from(!queue_lines.is_empty());
        let max_queue_rows = terminal_rows.saturating_sub(editor_rows).saturating_sub(3) as usize;
        if queue_lines.len() > max_queue_rows {
            let omitted = queue_lines.len() - max_queue_rows.saturating_sub(1);
            let mut clipped = vec![format!(
                "\x1b[2m… {}\x1b[0m",
                if is_zh() {
                    format!("已隐藏 {omitted} 行排队内容")
                } else {
                    format!("{omitted} queued lines hidden")
                }
            )];
            let keep = max_queue_rows.saturating_sub(1);
            clipped.extend(queue_lines.split_off(queue_lines.len().saturating_sub(keep)));
            queue_lines = clipped;
        }
        let job_lines = background_job_lines(&self.jobs, self.job_spinner, usize::from(cols));
        let job_rows = job_lines.len().min(u16::MAX as usize) as u16;
        let total_rows = 1u16
            .saturating_add(queue_lines.len().min(u16::MAX as usize) as u16)
            .saturating_add(queue_gap)
            .saturating_add(editor_rows)
            .saturating_add(job_rows);
        // Derived from what is on screen rather than stored: the tail was
        // pinned to the bottom exactly when its bottom edge sat on the last
        // usable row. `suspend()` leaves both values untouched, so they are
        // still the previous frame's truth here, and a terminal resize simply
        // falls back to natural placement.
        let was_anchored = self.tail_rows > 0
            && self.tail_start.saturating_add(self.tail_rows) == terminal_rows.saturating_sub(1);
        // 覆盖层开着：这一帧整个归它，正文和活动区都不画。
        if let Some(screen) = &mut self.screen {
            if screen.overlay_open() {
                screen.resize(cols, terminal_rows);
                screen.paint_overlay()?;
                return Ok(());
            }
        }
        // 候选面板得在**这一帧**就画出来。
        //
        // 原来是 `paint` 之后才算的，于是它永远慢一帧：打一个 `/` 什么都不出，
        // 再补个空格（多一次按键 = 多一帧）才蹦出来——用户实测报的「我要打
        // `/` 空格才会出现」就是这个。
        let hint_lines = if self.screen.is_some() {
            command_hint_lines(&self.editor.input, usize::from(cols))
        } else {
            Vec::new()
        };
        let placement = if let Some(screen) = &mut self.screen {
            // 全屏：正文归 Screen，活动区固定钉在视口底部。不用「腾地方」
            // 也不用算锚定——屏幕是自己的，底下永远有位置。
            //
            // resume 的语义就是「把屏幕拿回来」：suspend 过的话这里清掉标记，
            // 否则 paint 会以为外部输出还占着屏、直接跳过不画。
            screen.resume(!own);
            screen.resize(cols, terminal_rows);
            screen.set_command_hint(if screen.command_hint_dismissed() {
                Vec::new()
            } else {
                hint_lines
            });
            // `total_rows + 1`：活动区底下留一行空，和 inline 的观感一致。
            // 不留的话 footer 直接贴在屏幕最后一行上，挤得没有呼吸。
            let body = screen.paint(total_rows.saturating_add(1))?;
            LiveTailPlacement {
                output_row: body.saturating_sub(1),
                tail_start: body,
                overflow: 0,
                anchored: true,
            }
        } else {
            live_tail_placement(
                output_col,
                output_row,
                total_rows,
                terminal_rows,
                was_anchored,
            )
        };
        if placement.overflow > 0 {
            let mut stdout = io::stdout();
            queue!(stdout, MoveTo(0, terminal_rows.saturating_sub(1)))?;
            for _ in 0..placement.overflow {
                queue!(stdout, Print("\n"))?;
            }
            stdout.flush()?;
        }
        let output_row = placement.output_row;
        let tail_start = placement.tail_start;

        let mut stdout = io::stdout();
        queue!(stdout, MoveTo(0, tail_start), Clear(ClearType::CurrentLine))?;
        let mut row = tail_start.saturating_add(1);
        for line in &queue_lines {
            queue!(
                stdout,
                MoveTo(0, row),
                Clear(ClearType::CurrentLine),
                Print(line)
            )?;
            row = row.saturating_add(1);
        }
        if !queue_lines.is_empty() {
            queue!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
            row = row.saturating_add(1);
        }
        stdout.flush()?;

        let mut input_row = row;
        let mut rendered_rows = 0u16;
        let mut drawn_input = Vec::new();
        let footer_row = render_repl_input_with_footer(
            &mut stdout,
            &mut input_row,
            &mut rendered_rows,
            &mut drawn_input,
            self.editor.mode,
            &self.editor.input,
            self.editor.cursor,
            self.editor.raw_pasted_lines,
            &self.footer,
            false,
        )?;
        self.footer_offset = footer_row.map(|abs| abs.saturating_sub(tail_start));
        if let Some(screen) = &mut self.screen {
            screen.set_input_rows(drawn_input);
            // 反显盖在输入框**之上**：输入框刚画完，这会儿盖才不会被它冲掉。
            screen.paint_input_selection()?;
        }
        // The editor is back on screen: the cursor must be visible no
        // matter which path hid it (e.g. a question prompt suspended the
        // editor with the cursor hidden and then exited early). This is
        // the single convergence point for every editor redraw, so an
        // unconditional Show here prevents a permanently invisible cursor.
        self.input_cursor = if self.screen.is_some() {
            // 全屏下不问终端（那会吞掉正在打的字），按布局算——反正输入区
            // 是我们自己摆的，算得出来。
            let prefix = input_prompt_bar(self.editor.mode);
            let (col, row_offset) = repl_cursor_position_for_cols(
                &prefix,
                &self.editor.input,
                self.editor.cursor,
                usize::from(cols),
            );
            // `input_row` 是输入区**顶上那根空竖条**的行，正文从它下一行才开始，
            // 所以要 +1。少这一行的表现是输入法的预编辑框浮在文字上一行。
            (col, input_row.saturating_add(1).saturating_add(row_offset))
        } else {
            cursor_position_or(self.input_cursor)
        };
        // 状态行在屏幕上的位置：点它要能对上是哪一个后台任务。
        self.job_strip_start = input_row.saturating_add(rendered_rows);
        self.job_strip_rows = job_rows;
        if !job_lines.is_empty() {
            let mut stdout = io::stdout();
            let mut job_row = input_row.saturating_add(rendered_rows);
            for line in &job_lines {
                queue!(
                    stdout,
                    MoveTo(0, job_row),
                    Clear(ClearType::CurrentLine),
                    Print(line)
                )?;
                job_row = job_row.saturating_add(1);
            }
            queue!(stdout, MoveTo(self.input_cursor.0, self.input_cursor.1))?;
            stdout.flush()?;
        }
        execute!(io::stdout(), crossterm::cursor::Show)?;
        self.output_cursor = (output_col, output_row);
        self.tail_start = tail_start;
        self.tail_rows = total_rows;
        self.rendered = true;
        Ok(())
    }

    pub(in crate::cli) fn apply_output_frame(&mut self, frame: &[u8]) -> Result<()> {
        if frame.is_empty() {
            return Ok(());
        }
        // 全屏：字节交给终端模拟器，它按光标动作落到对的行上——spinner 的
        // 原地刷新、命令块的实时输出都靠这个，输出方一行不用改。
        if let Some(screen) = &mut self.screen {
            screen.feed(frame);
            let cursor = self.output_cursor;
            return self.resume_at_own(cursor);
        }
        if !self.rendered {
            io::stdout().write_all(frame)?;
            io::stdout().flush()?;
            self.output_cursor = cursor_position_or(self.output_cursor);
            return Ok(());
        }

        let (columns, terminal_rows) = terminal::size().unwrap_or((80, 24));
        let terminal_rows = terminal_rows.max(1);
        let unbounded = terminal_frame_layout(frame, self.output_cursor, columns, None);
        let natural_tail = unbounded
            .cursor
            .1
            .saturating_add(u16::from(unbounded.cursor.0 > 0));
        let occupied_tail = unbounded
            .occupied_bottom
            .map(|row| row.saturating_add(1))
            .unwrap_or(0);
        let desired_tail = natural_tail.max(occupied_tail);
        let max_tail = max_live_tail_start(terminal_rows, self.tail_rows);
        let next_tail = live_tail_next_start(self.tail_start, desired_tail, max_tail);
        let shift = i32::from(next_tail) - i32::from(self.tail_start);
        let frame_margin = if shift < 0 {
            self.tail_start
        } else {
            next_tail
        };
        let output_bottom = live_frame_output_bottom(frame_margin, unbounded);
        let leading_scroll = output_bottom
            .map(|bottom| self.output_cursor.1.saturating_sub(bottom))
            .unwrap_or(0);
        let frame_start = if let Some(bottom) = output_bottom.filter(|_| leading_scroll > 0) {
            (0, bottom)
        } else {
            self.output_cursor
        };
        let (bounded, scrolls) =
            terminal_frame_layout_with_scrolls(frame, frame_start, columns, output_bottom);

        let mut transaction = Vec::with_capacity(frame.len().saturating_add(96));
        if shift > 0 {
            queue!(
                transaction,
                MoveTo(0, self.tail_start.saturating_add(1)),
                Print(format!("\x1b[{shift}L"))
            )?;
        }
        let lifted = output_bottom.filter(|_| {
            crate::terminal::kitty::images_emitted() && (leading_scroll > 0 || !scrolls.is_empty())
        });
        if let Some(bottom) = lifted {
            queue_lifted_frame(
                &mut transaction,
                frame,
                frame_start,
                bottom,
                leading_scroll,
                &scrolls,
                terminal_rows,
            )?;
        } else {
            if let Some(bottom) = output_bottom {
                queue!(
                    transaction,
                    Print(format!("\x1b[1;{}r", bottom.saturating_add(1)))
                )?;
            }
            if let Some(bottom) = output_bottom.filter(|_| leading_scroll > 0) {
                queue!(transaction, MoveTo(0, bottom))?;
                for _ in 0..leading_scroll {
                    queue!(transaction, Print("\n"))?;
                }
            }
            queue!(transaction, MoveTo(frame_start.0, frame_start.1))?;
            transaction.extend_from_slice(frame);
            queue!(transaction, Print("\x1b[r"))?;
        }
        if shift < 0 {
            queue!(
                transaction,
                MoveTo(0, next_tail.saturating_add(1)),
                Print(format!("\x1b[{}M", -shift))
            )?;
        }
        let input_row = (i32::from(self.input_cursor.1) + shift)
            .clamp(0, i32::from(terminal_rows.saturating_sub(1))) as u16;
        queue!(transaction, MoveTo(self.input_cursor.0, input_row))?;
        if std::env::var_os("MIYU_TAIL_TRACE").is_some() {
            trace_tail_redraw(
                self.tail_start,
                next_tail,
                shift,
                self.tail_rows,
                self.output_cursor,
                output_bottom,
                leading_scroll,
                terminal_rows,
                &transaction,
            );
        }
        let mut stdout = io::stdout();
        stdout.write_all(&transaction)?;
        stdout.flush()?;

        self.output_cursor = bounded.cursor;
        self.tail_start = next_tail;
        self.input_cursor.1 = input_row;
        Ok(())
    }

    pub(in crate::cli) fn apply_renderer_frame(
        &mut self,
        renderer: &mut render::StreamRenderer,
    ) -> Result<()> {
        // 这一轮里跑着的子代理烧掉的量先记在 Σ 上——它们的审计会话要跑完才落盘，
        // 而一个子代理能跑好几分钟，那几分钟里 Σ 纹丝不动。
        self.set_live_turn_tokens(renderer.running_subagent_tokens());
        let frame = renderer.take_output_frame();
        self.apply_output_frame(&frame)
    }

    /// 短提示交给通知条。接下了就返回真。
    ///
    pub(in crate::cli) fn toast_note(&mut self, text: &str) -> bool {
        self.toast_note_at(text, false)
    }

    /// `near_input` = 这条提示讲的是输入框的事，得待在输入框旁边。
    pub(in crate::cli) fn toast_note_at(&mut self, text: &str, near_input: bool) -> bool {
        let plain = super::screen::toast::plain(text);
        let taken = self.screen.as_mut().is_some_and(|screen| {
            if near_input {
                screen.toast_near_input(plain.trim())
            } else {
                screen.toast(plain.trim())
            }
        });
        if taken {
            let cursor = self.output_cursor;
            let _ = self.resume_at_own(cursor);
        }
        taken
    }

    /// 又打字了：候选面板可以重新弹出来。
    pub(in crate::cli) fn allow_command_hint(&mut self) {
        if let Some(screen) = &mut self.screen {
            screen.allow_command_hint();
        }
    }

    /// 面板开着时跟着内容刷新。后台任务的日志自己在长，没人碰键盘也得动。
    /// 有没有开着面板（空闲轮询要不要放快，好让面板里的转轮画得齐）。
    pub(in crate::cli) fn overlay_open(&self) -> bool {
        self.screen
            .as_ref()
            .is_some_and(super::screen::Screen::overlay_open)
    }

    pub(in crate::cli) fn tick_overlay(&mut self) -> Result<()> {
        if self
            .screen
            .as_ref()
            .is_some_and(super::screen::Screen::overlay_open)
        {
            let cursor = self.output_cursor;
            self.resume_at_own(cursor)?;
        }
        Ok(())
    }

    /// 通知到点了就收掉，顺手重画。空闲 tick 调它。
    pub(in crate::cli) fn expire_toast(&mut self) -> Result<()> {
        let expired = self
            .screen
            .as_mut()
            .is_some_and(super::screen::Screen::expire_toast);
        if expired {
            let cursor = self.output_cursor;
            self.resume_at_own(cursor)?;
        }
        Ok(())
    }

    pub(in crate::cli) fn redraw(&mut self) -> Result<()> {
        let output_cursor = self.output_cursor;
        // 全屏下重画就是重画，不必先把屏幕让出去——inline 那边先 suspend
        // 是为了擦掉钉在 scrollback 里的旧活动区，全屏没有这个包袱。
        //
        // 走**自己那条**：`resume_at` 是"外面刚往屏上打过东西"的路，它会置
        // `needs_clear`、整屏擦一次。而 `redraw` 是每一次按键都要调的——AI 正在
        // 流式输出时打字，就成了每敲一个字整屏重绘一遍，屏幕跟着抖（用户原话
        // 「输入框会疯狂鬼畜跳动」）。重画自己的画面从来不需要先擦。
        if self.screen.is_some() {
            return self.resume_at_own(output_cursor);
        }
        self.suspend()?;
        self.resume_at(output_cursor)
    }

    pub(in crate::cli) fn clear_screen(&mut self) -> Result<()> {
        // 全屏：和终端 `clear` 一个意思——往正文里补一屏空行把视口顶空，
        // **内容没删**，往回翻还在。
        if let Some(screen) = &mut self.screen {
            screen.dismiss_toast();
            screen.push_blank_screen();
            let cursor = self.output_cursor;
            return self.resume_at(cursor);
        }
        self.suspend()?;
        let mut stdout = io::stdout();
        execute!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;
        self.output_cursor = (0, 0);
        self.tail_start = 0;
        self.tail_rows = 0;
        self.resume_at((0, 0))
    }
}
