//! 组一轮模型请求的消息数组:Responses 续传时只发增量、续传上下文插回原位、
//! 发出去之前配平 tool_calls / tool 结果。09-17 从 `chat_with_tools` 里抽出。

use super::round_state::RoundState;
use crate::agent::*;

impl Agent {
    pub(super) fn build_round_request(
        &self,
        current_turn_id: &str,
        messages: &[ChatMessage],
        st: &RoundState,
    ) -> Result<Vec<ChatMessage>> {
        let mut request_messages = if st.responses_continuation.is_some() {
            messages
                .get(st.continuation_input_start..)
                .context("Responses continuation input cursor is out of bounds")?
                .to_vec()
        } else {
            messages.to_vec()
        };
        if let Some((context_index, context_messages)) = st.continuation_context.as_ref() {
            let offset = context_index
                .checked_sub(st.continuation_input_start)
                .context("Responses continuation context cursor is out of bounds")?;
            if offset > request_messages.len() {
                bail!("Responses continuation context cursor is out of bounds");
            }
            request_messages.splice(offset..offset, context_messages.to_vec());
        }
        // 发出去之前配平 tool_calls / tool 结果:任一回放或续传路径漏了一条
        // tool 结果,严格网关(deepseek)会 400 且会话永久不可用。补占位兜底,
        // 补过就留痕,以便回溯真正漏结果的路径(理论上不该触发)。
        let balance_repairs =
            crate::agent::context::enforce_tool_call_result_balance(&mut request_messages);
        if balance_repairs > 0 {
            tracing::warn!(
                session_id = %self.state.session_id(),
                turn_id = %current_turn_id,
                repaired = balance_repairs,
                "补齐了缺失的 tool 结果:存在未配平的 assistant tool_calls,已兜底防 400"
            );
        }
        Ok(request_messages)
    }
}
