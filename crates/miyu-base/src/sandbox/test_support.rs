//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/sandbox/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub fn confine_std(command: &mut std::process::Command) {
    if let Some(policy) = current_sandbox() {
        for (key, value) in child_env(&policy, false) {
            command.env(key, value);
        }
        let rules = Rules::prepare(&policy);
        // pre_exec 是 Unix 专有 API；Windows 上没有这一步，后端是 unsupported
        //（apply 恒为 Ok），当场求值一次即可。
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: 同上。
            unsafe {
                command.pre_exec(move || rules.apply());
            }
        }
        #[cfg(not(unix))]
        let _ = rules.apply();
    }
}
