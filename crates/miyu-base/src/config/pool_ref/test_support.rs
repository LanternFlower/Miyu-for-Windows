//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/config/pool_ref.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ModelPoolRef {
    pub fn explicit_models_mut(&mut self) -> Option<&mut Vec<ActiveProviderModelConfig>> {
        match self {
            Self::Models(models) if !models.is_empty() => Some(models),
            _ => None,
        }
    }
}
