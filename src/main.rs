//! 可执行入口。真正的模块树与启动流程都在 `lib.rs`，这里只负责把错误打出来
//! 并给出退出码。

#[tokio::main(flavor = "current_thread")]
async fn main() {
    limit_malloc_arenas();
    if let Err(error) = miyu::run().await {
        eprintln!("{}: {error:#}", miyu::error_label());
        std::process::exit(1);
    }
}

/// glibc 默认按 8×CPU 数开 malloc arena，而 daemon 常驻只有个位数线程，
/// 多余的 arena 只贡献地址空间碎片（实测 VmData 是 RssAnon 的 2.75×）。
/// 限到 2 个足够覆盖现有线程形态，锁争用可忽略。
fn limit_malloc_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
    }
}
