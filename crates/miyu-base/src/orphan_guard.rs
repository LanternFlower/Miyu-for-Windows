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

/// macOS 版：没有 `PR_SET_PDEATHSIG`，改用 kqueue 盯着父进程的 `NOTE_EXIT`。
///
/// 09-22 补上的。原来这儿是直接返回 `false`，理由写的是「孤儿 daemon 是在 Linux
/// 上实测到的，没有证据说别处也漏，不照着猜写一套」——那条判断当时是对的，但
/// 09-22 在真 Mac 上跑测具时抓到了一个没人收尸的 `stub_llm.py`，证据有了。
///
/// macOS 上还更糟一层：`timeout` 命令不存在（那是 GNU coreutils 的），测具惯用的
/// `timeout 400 python3 …` 那套外部护栏在 Mac 上整个失效，孤儿只会比 Linux 多。
///
/// 内核只认「登记那一刻还活着」的进程，所以登记前后各查一次 `getppid()`：
/// 中间那条缝里父进程没了的话，事件永远不会来，得自己了断。这和 Linux 那支
/// `armed && getppid() == 1` 是同一个道理。
///
/// 真机 A/B（Mac Studio M4 Max，macOS 26.6）：父进程 `kill -9` 之后等 5 秒——
/// 改动前 daemon 原地活着（孤儿），改动后跟着退。两臂用 `EVFILT_PROC` 在
/// `orphan_guard.rs` 里的出现次数（0 对 1）自证跑的是不同的代码。
#[cfg(target_os = "macos")]
pub fn tie_lifetime_to_launcher() -> bool {
    if std::env::var_os(DETACHED_ENV).is_some() {
        return false;
    }
    let parent = unsafe { libc::getppid() };
    if parent <= 1 {
        // 挂上之前就已经是孤儿了。
        std::process::exit(0);
    }
    // SAFETY: 只建一个内核事件队列，不碰本进程内存。
    let queue = unsafe { libc::kqueue() };
    if queue < 0 {
        return false;
    }
    // SAFETY: `kevent` 是纯 POD，零初始化之后逐字段填。
    let mut change: libc::kevent = unsafe { std::mem::zeroed() };
    change.ident = parent as usize;
    change.filter = libc::EVFILT_PROC;
    change.flags = libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT;
    change.fflags = libc::NOTE_EXIT;
    // SAFETY: 传入一条变更、不取事件，超时给空指针表示立即返回。
    let registered =
        unsafe { libc::kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    if registered < 0 {
        // ESRCH 就是「登记这一瞬父进程没了」。
        unsafe { libc::close(queue) };
        if unsafe { libc::getppid() } <= 1 {
            std::process::exit(0);
        }
        return false;
    }
    if unsafe { libc::getppid() } <= 1 {
        unsafe { libc::close(queue) };
        std::process::exit(0);
    }
    std::thread::Builder::new()
        .name("orphan-guard".into())
        .spawn(move || {
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            loop {
                // SAFETY: 阻塞等一条事件；超时给空指针表示无限等。
                let taken = unsafe {
                    libc::kevent(queue, std::ptr::null(), 0, &mut event, 1, std::ptr::null())
                };
                if taken > 0 {
                    break;
                }
                if taken == 0 {
                    continue;
                }
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                // 队列出了别的错就别在这儿挂死;按「父进程没了」处理,宁可早退
                // 也不要留一个永远不会被回收的孤儿。
                break;
            }
            // 走 kill 而不是 raise:raise 是投给当前线程的,而优雅退出的处理器
            // 装在进程上。
            unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
        })
        .is_ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn tie_lifetime_to_launcher() -> bool {
    // 其余平台没有实测证据,不照着猜写一套(和 macOS 09-22 之前一样的口径)。
    false
}
