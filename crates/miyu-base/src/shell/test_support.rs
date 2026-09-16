//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/shell/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]

pub fn looks_like_natural_language(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return false;
    }
    !trimmed.contains('\n') && !trimmed.contains('\r')
}
