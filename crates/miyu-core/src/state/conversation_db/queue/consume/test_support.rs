//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/conversation_db/queue/consume.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ConversationDb {
    pub fn consume_queued_prompts(
        &self,
        session_id: &str,
        turn_id: &str,
        prompts: &[(String, String)],
        preceding_assistant_content: Option<&str>,
        preceding_assistant_reasoning: Option<&str>,
        preceding_assistant_provider_id: Option<&str>,
        preceding_assistant_model: Option<&str>,
        queue_session_id: &str,
    ) -> Result<()> {
        let prompts = prompts
            .iter()
            .map(|(prompt_id, content)| (prompt_id.clone(), content.clone(), "[]".to_string()))
            .collect::<Vec<_>>();
        self.consume_queued_prompts_with_checkpoint(
            session_id,
            turn_id,
            &prompts,
            preceding_assistant_content,
            preceding_assistant_reasoning,
            preceding_assistant_provider_id,
            preceding_assistant_model,
            queue_session_id,
            None,
        )
    }
}
