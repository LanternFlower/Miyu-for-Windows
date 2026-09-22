//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/config/provider.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ModelTiersConfig {
    /// Whether the role carries an explicit value (as opposed to its default).
    pub fn role_is_explicit(&self, role: AuxRole) -> bool {
        self.roles.contains_key(role.key())
    }
}
