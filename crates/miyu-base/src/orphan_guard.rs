//! 谁起的 daemon，谁死它就跟着死——除非是 Miyu 自己的启动器起的。
//!
//! 09-20 实录：用户后台攒了 19 个 7-8 MB 的 `miyu __daemon` 僵进程，最老的
//! 4 天。逐个核对 `MIYU_HOME` 之后发现**一个都不是真 daemon**，全是测具的沙箱
//! daemon（`/tmp/claude-1000/.../scratchpad/*`、`/tmp/miyu-*`、`manual-tui-home`
//! 这些）。真 daemon 换二进制重启是干净的。
//!
//! 它们不退的原因不是赖着——给一个发 `SIGTERM` 立刻就干净退出了——而是**没人
//! 给它们发**：测具跑成
//!
//! ```text
//! timeout 400 python3 testkit/tui/xxx.py
//! ```
//!
//! 超时的时候 `timeout` 把 SIGTERM 发给 python，而 python 默认的 SIGTERM 处理
//! 是直接终止解释器、**不跑 `finally`**，于是脚本里那句
//! `daemon.terminate()` 永远没机会执行，沙箱 daemon 被过继给 systemd 继续跑。
//! 测具里起 daemon 的地方有 66 处，逐个加信号处理既改不完也挡不住 SIGKILL。
//!
//! 所以把这件事放到 daemon 自己身上：`PR_SET_PDEATHSIG` 让内核在**启动它的那个
//! 进程**死掉时替我们发 SIGTERM。父进程怎么死的都算数，SIGKILL 也算。
//!
//! 反过来，真 daemon 本来就该活得比启动它的终端久，所以
//! `ipc::lifecycle::start_daemon_process`（`miyu daemon start` / `ensure_daemon`
//! 唯一的出口，它自己会 `setsid`）会给子进程挂上 [`DETACHED_ENV`]，看到这个标记
//! 就不捆。判据用显式标记而不是「是不是 session leader」：有的测具自己也
//! `preexec_fn=os.setsid`（`testkit/voice/e2e.py`），那条判据会错。

/// Miyu 自己的启动器留给 daemon 的标记：这个 daemon 是故意要活过启动者的。
pub const DETACHED_ENV: &str = "MIYU_DAEMON_DETACHED";

/// 把本进程的命捆在启动它的进程上。daemon 入口调一次，越早越好。
///
/// 已经带着 [`DETACHED_ENV`] 的（真 daemon）原样返回 `false`，什么都不做。
#[cfg(target_os = "linux")]
pub fn tie_lifetime_to_launcher() -> bool {
    if std::env::var_os(DETACHED_ENV).is_some() {
        return false;
    }
    // SAFETY: prctl 只改本进程自己的一个标志位，不碰内存。
    let armed = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) } == 0;
    // 挂上之前父进程就已经没了的话，内核不会补发信号——这时候自己了断，
    // 否则又是一个孤儿（测具起完就被 timeout 打死的那一瞬正好落在这里）。
    if armed && unsafe { libc::getppid() } == 1 {
        std::process::exit(0);
    }
    armed
}

#[cfg(not(target_os = "linux"))]
pub fn tie_lifetime_to_launcher() -> bool {
    // PR_SET_PDEATHSIG 是 Linux 专有的。别的平台先不做——孤儿 daemon 是在
    // Linux 上实测到的，没有证据说别处也漏，不照着猜写一套。
    false
}
