//! id 生成器。
//!
//! `random_token` 是 URL 安全的随机串,`random_id` 给它加个前缀。和宿主端口、
//! 和 daemon 都没关系——`host_grants` 签令牌、`tools::vision::inline` 取寄存 ref、
//! `web` 生成 run id 都要它,是谁都够得着的最低层(09-16 从 `host_ports` 归位)。
//!
//! `host_ports` 与 `runtime` 各留一条再导出,`miyu_hosts::runtime::random_id` 这类
//! 老路径一字未改。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;

pub fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

pub fn random_id(prefix: &str, bytes: usize) -> String {
    format!("{prefix}_{}", random_token(bytes))
}
