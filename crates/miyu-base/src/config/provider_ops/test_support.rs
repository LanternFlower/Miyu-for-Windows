//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/config/provider_ops.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl AppConfig {
    pub fn active_context_window(&self) -> Result<Option<usize>> {
        Ok(self
            .active_context_window_with_source()?
            .map(|(window, _)| window))
    }

    /// 内置 Claude Code 特殊供应商是否启用(订阅中转的总开关)。
    pub fn claude_code_enabled(&self) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.is_claude_code() && provider.enabled)
    }

    /// 内置 Codex 特殊供应商是否启用(codex CLI 中转的总开关)。
    pub fn codex_enabled(&self) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.is_codex() && provider.enabled)
    }

    /// 内置 Antigravity 特殊供应商是否启用(agy CLI 中转的总开关)。
    pub fn antigravity_enabled(&self) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.is_antigravity() && provider.enabled)
    }
}
