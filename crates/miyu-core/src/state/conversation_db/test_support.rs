//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/conversation_db/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl ConversationDb {
    /// 开库。库损坏时把 rusqlite 的裸报错换成一条能照着做的说明——
    /// 08-29 用户反馈:`miyu daemon start` / `web` / `dev` 全都只吐
    /// 「exited before becoming ready (exit status: 1)」,daemon.log 里也只有
    /// 一行 `database disk image is malformed`,既不说是哪个文件,也不说怎么
    /// 办;真正有用的那句在第三个文件 `miyu.<date>.log` 里。
    ///
    /// 损坏可能从 open/PRAGMA/版本读取/迁移里任意一处冒出来,所以在出口统一
    /// 认,不逐个 `?` 去猜。
    pub fn open(state_dir: &Path) -> Result<Self> {
        Self::open_at(state_dir, state_dir)
    }
}
