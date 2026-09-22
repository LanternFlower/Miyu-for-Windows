//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/queue.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl StateStore {
    pub fn enqueue_prompt_for_target(
        &self,
        target: &RunningTurnQueueTarget,
        prompt_id: &str,
        content: &str,
        display_content: &str,
        attachments: &[QueuedPromptAttachment],
    ) -> Result<QueuedPrompt> {
        self.enqueue_prompt_for_target_with_uploads(
            target,
            prompt_id,
            content,
            display_content,
            attachments,
            &[],
        )
    }

    pub fn consume_queued_prompts(
        &self,
        turn_id: &str,
        prompts: &[(String, String)],
        preceding_assistant_content: Option<&str>,
        preceding_assistant_reasoning: Option<&str>,
    ) -> Result<()> {
        self.conv_db.consume_queued_prompts(
            &self.session(),
            turn_id,
            prompts,
            preceding_assistant_content,
            preceding_assistant_reasoning,
            None,
            None,
            &self.queue_session_id,
        )
    }
}
