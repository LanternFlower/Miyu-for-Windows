//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/platforms/plugins/message_history/store/types.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ConversationKey {
    pub(crate) fn conversation_kind(&self) -> &str {
        &self.conversation_kind
    }

    pub(crate) fn account_scope(&self) -> AccountKey {
        AccountKey {
            platform: self.platform.clone(),
            account_id: self.account_id.clone(),
        }
    }
}
