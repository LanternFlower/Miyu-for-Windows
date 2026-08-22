//! 终端画面的记账与重绘。
//!
//! `TerminalFrameTracker` 解析我们自己发出去的转义序列（走 vte），据此推算终端
//! 现在停在第几行——**不能每次都去问终端**（ESC[6n 要等回答，在流式输出里会
//! 卡住）。
//!
//! `lift_external_output_into_page` 处理外部输出（命令、图片）插进来的情况：
//! 那些内容不归活动区管，但会把活动区顶走，得把记账补上。

use crate::cli::repl::tail::*;

impl LiveReplTail {
    /// 把刚打完的外部输出整屏上滚，直到它完全落进正文页内。
    ///
    /// kitty 图形协议原文：设了页边距之后，只有「完全落在页内」的图片才
    /// 跟着滚动，越界的会被裁剪并留在原地。图片是顺着光标往下打的，底部
    /// 常常正压在活动区上——恰好越界。此后每一次受限区滚动它都不动，文字
    /// 走了图不走，一次留一条残影；一次会话滚几百次，屏幕上就堆成一叠重
    /// 复的切片。
    ///
    /// 原本是个死锁：那个本该把图抬出活动区的滚动，正是 kitty 拒绝对图生
    /// 效的滚动。所以要趁这一刻整屏滚一次——整屏滚没有页边距，图片一定跟
    /// 着走。这里也正是唯一能这么做的时机：活动区已经 suspend、那几行是
    /// 空的，被一起带上去不会留下痕迹（在别处整屏滚会把画着 ┃ 的输入框推
    /// 进正文）。
    pub(in crate::cli) fn lift_external_output_into_page(&mut self) -> Result<()> {
        let (_, terminal_rows) = terminal::size().unwrap_or((80, 24));
        let terminal_rows = terminal_rows.max(1);
        let page_bottom = self.tail_start.saturating_sub(1);
        let cursor_row = cursor_row_or(self.output_cursor.1);
        let overflow = cursor_row.saturating_sub(page_bottom);
        if std::env::var_os("MIYU_TAIL_TRACE").is_some() {
            use std::io::Write as _;
            let path = crate::platform_dirs::PlatformDirs::new()
                .map(|dirs| dirs.home_dir().join(".miyu/cache/logs/tail-trace.log"))
                .unwrap_or_else(|| std::path::PathBuf::from("tail-trace.log"));
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(
                    file,
                    "lift: 光标行={cursor_row} 页底={page_bottom} 需抬升={overflow}                      tail_start={} 屏高={terminal_rows}",
                    self.tail_start
                );
            }
        }
        if overflow == 0 {
            return Ok(());
        }
        let mut stdout = io::stdout();
        queue!(stdout, MoveTo(0, terminal_rows.saturating_sub(1)))?;
        for _ in 0..overflow {
            queue!(stdout, Print("\n"))?;
        }
        // 停在滚完后内容真正的末尾:调用方随后会重新查询光标来定位活动区。
        queue!(stdout, MoveTo(0, page_bottom))?;
        stdout.flush()?;
        self.output_cursor = (0, page_bottom);
        Ok(())
    }

    pub(in crate::cli) fn suspend(&mut self) -> Result<()> {
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
        self.resume_at(cursor_position_or(self.output_cursor))
    }

    pub(in crate::cli) fn resume_at(&mut self, (output_col, output_row): (u16, u16)) -> Result<()> {
        let (cols, terminal_rows) = terminal::size().unwrap_or((80, 24));
        let terminal_rows = terminal_rows.max(1);
        let editor_rows = repl_input_rendered_rows(
            &self.editor.input,
            self.editor.is_pasted,
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
        let placement = live_tail_placement(
            output_col,
            output_row,
            total_rows,
            terminal_rows,
            was_anchored,
        );
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
        let footer_row = render_repl_input_with_footer(
            &mut stdout,
            &mut input_row,
            &mut rendered_rows,
            self.editor.mode,
            &self.editor.input,
            self.editor.cursor,
            self.editor.is_pasted,
            &self.footer,
            false,
        )?;
        self.footer_offset = footer_row.map(|abs| abs.saturating_sub(tail_start));
        // The editor is back on screen: the cursor must be visible no
        // matter which path hid it (e.g. a question prompt suspended the
        // editor with the cursor hidden and then exited early). This is
        // the single convergence point for every editor redraw, so an
        // unconditional Show here prevents a permanently invisible cursor.
        self.input_cursor = cursor_position_or(self.input_cursor);
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
        let bounded = terminal_frame_layout(frame, frame_start, columns, output_bottom);

        let mut transaction = Vec::with_capacity(frame.len().saturating_add(96));
        if shift > 0 {
            queue!(
                transaction,
                MoveTo(0, self.tail_start.saturating_add(1)),
                Print(format!("\x1b[{shift}L"))
            )?;
        }
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
        let frame = renderer.take_output_frame();
        self.apply_output_frame(&frame)
    }

    pub(in crate::cli) fn redraw(&mut self) -> Result<()> {
        let output_cursor = self.output_cursor;
        self.suspend()?;
        self.resume_at(output_cursor)
    }

    pub(in crate::cli) fn clear_screen(&mut self) -> Result<()> {
        self.suspend()?;
        let mut stdout = io::stdout();
        execute!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;
        self.output_cursor = (0, 0);
        self.tail_start = 0;
        self.tail_rows = 0;
        self.resume_at((0, 0))
    }
}
