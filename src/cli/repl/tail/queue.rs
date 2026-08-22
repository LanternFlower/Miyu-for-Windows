//! 活动区里的排队消息与流式片段。
//!
//! 用户在回合跑着时敲的内容先进队列，显示在活动区下方。片段（chunk）攒着批量
//! 刷（`flush_pending_chunks`）——每来一个 token 就重绘一次，终端跟不上。

use crate::cli::repl::tail::*;

impl LiveReplTail {
    pub(in crate::cli) fn enqueue(&mut self, prompt: QueuedPrompt) -> Result<()> {
        let output_cursor = self.output_cursor;
        self.suspend()?;
        self.append_queued(prompt);
        self.resume_at(output_cursor)
    }

    pub(in crate::cli) fn append_queued(&mut self, prompt: QueuedPrompt) {
        self.queued.push(prompt);
        self.queued.sort_by_key(|prompt| prompt.seq);
    }

    pub(in crate::cli) fn queue_stream_chunk(&mut self, chunk: ChatStreamChunk) {
        if let Some(pending) = self
            .pending_chunks
            .last_mut()
            .filter(|pending| pending.kind == chunk.kind)
        {
            pending.text.push_str(&chunk.text);
        } else {
            self.pending_chunks.push(chunk);
        }
    }

    pub(in crate::cli) fn flush_pending_chunks(
        &mut self,
        renderer: &mut render::StreamRenderer,
    ) -> Result<()> {
        for chunk in std::mem::take(&mut self.pending_chunks) {
            renderer.write_chunk(chunk)?;
        }
        Ok(())
    }

    pub(in crate::cli) fn discard_pending_chunks(&mut self) {
        self.pending_chunks.clear();
    }

    /// 提交回显的绘制半程:同步块内只做本地写。ESC[6n 位置查询要等终端
    /// 应答,放在同步块里会把块的存活时间拉到毫秒级——kitty 的同步输出有
    /// 超时保护,超了就提前提交半成品帧,用户看到的就是光标闪到屏幕底部
    /// 再跳回输入框(08-20 点名)。查询挪到块外的 finalize,此时光标已隐藏,
    /// 查询期间无可见状态。
    pub(in crate::cli) fn commit_submission_render(
        &mut self,
        submission: &LiveSubmission,
    ) -> Result<()> {
        self.suspend()?;
        // suspend 已把光标 MoveTo(output_cursor):列已知,块内零查询。
        write_committed_user_messages_from(
            &[(submission.display_content.as_str(), self.editor.mode)],
            true,
            Some(self.output_cursor.0),
        )?;
        Ok(())
    }

    /// 提交回显的收尾半程:同步块外、光标隐藏状态下校准输出光标。
    pub(in crate::cli) fn commit_submission_finalize(&mut self) {
        self.output_cursor = cursor_position_or(self.output_cursor);
    }

    pub(in crate::cli) fn commit_empty_submission(&mut self) -> Result<()> {
        let mode = self.editor.mode;
        self.editor.clear();
        self.suspend()?;
        write_committed_user_messages(&[("", mode)], true)?;
        let output_cursor = cursor_position_or(self.output_cursor);
        self.output_cursor = output_cursor;
        self.resume_at(output_cursor)
    }

    /// Print a background-command wake reply into the scrollback while the
    /// REPL idles: dim header, then the assistant's report.
    pub(in crate::cli) fn show_background_report(
        &mut self,
        report: &BackgroundReport,
    ) -> Result<()> {
        self.suspend()?;
        let mut stdout = io::stdout();
        queue!(
            stdout,
            Print(format!(
                "\x1b[2m⚙ {}\x1b[0m\r\n\r\n",
                job_wake_headline(&report.headline)
            ))
        )?;
        for line in report.reply.lines() {
            queue!(
                stdout,
                Print(format!("{}\r\n", render::render_markdown_line(line)))
            )?;
        }
        queue!(stdout, Print("\r\n"))?;
        stdout.flush()?;
        self.output_cursor = cursor_position_or(self.output_cursor);
        let output_cursor = self.output_cursor;
        self.resume_at(output_cursor)
    }

    /// Remove queued bubbles without committing them as sent messages —
    /// the daemon dropped these prompts (explicit cancel), they were never
    /// answered and never entered the conversation.
    pub(in crate::cli) fn drop_queued(&mut self, prompt_ids: &[String]) -> Result<()> {
        let ids = prompt_ids.iter().collect::<std::collections::HashSet<_>>();
        if !self
            .queued
            .iter()
            .any(|prompt| ids.contains(&prompt.prompt_id))
        {
            return Ok(());
        }
        let output_cursor = self.output_cursor;
        self.suspend()?;
        self.queued
            .retain(|prompt| !ids.contains(&prompt.prompt_id));
        self.resume_at(output_cursor)
    }

    pub(in crate::cli) fn consume_queued(
        &mut self,
        prompt_ids: &[String],
        mode: AgentMode,
    ) -> Result<()> {
        self.suspend()?;
        let ids = prompt_ids.iter().collect::<std::collections::HashSet<_>>();
        let consumed = self
            .queued
            .iter()
            .filter(|prompt| ids.contains(&prompt.prompt_id))
            .map(|prompt| (prompt.display_content.as_str(), mode))
            .collect::<Vec<_>>();
        write_committed_user_messages(&consumed, true)?;
        self.queued
            .retain(|prompt| !ids.contains(&prompt.prompt_id));
        let output_cursor = cursor_position_or(self.output_cursor);
        self.output_cursor = output_cursor;
        self.resume_at(output_cursor)
    }

    pub(in crate::cli) fn reload_queue(&mut self, state: &StateStore) -> Result<()> {
        let output_cursor = self.output_cursor;
        self.suspend()?;
        self.queued = state.load_queued_prompts()?;
        self.resume_at(output_cursor)
    }
}
