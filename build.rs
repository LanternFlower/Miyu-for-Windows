//! 根包的构建脚本只干一件事:算本次构建的唯一 id(`MIYU_BUILD_ID`)。
//!
//! 它要对整棵源码树 `rerun-if-changed`(改任何一层都得换 id,CLI 才认得出老 daemon),
//! 所以必须住在依赖树最顶端:放在 miyu-base 里就意味着改 hosts 一行也从 base 起
//! 全量重编,拆 crate 白拆。资源烘焙(默认提示词 / o200k / jieba)在
//! `crates/miyu-base/build.rs`,那份只对资源文件本身 rerun。
use std::env;

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=crates");
    println!("cargo:rerun-if-changed=web");
    println!("cargo:rerun-if-env-changed=MIYU_BUILD_ID");
    let build_id = env::var("MIYU_BUILD_ID").unwrap_or_else(|_| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
            .to_string()
    });
    assert!(
        !build_id.is_empty()
            && build_id.len() <= 128
            && build_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
        "MIYU_BUILD_ID must contain 1 to 128 ASCII letters, digits, dots, underscores or hyphens"
    );
    println!("cargo:rustc-env=MIYU_BUILD_ID={build_id}");
}
