//! 进程级的小工具(09-16 从 runtime 下沉:配置目录缓存这种底层模块也要用,不能反向认识 runtime)。

use std::ffi::OsStr;

/// Apply the Windows `CREATE_NO_WINDOW` flag to a child process. Miyu's
/// daemon has no console of its own, so spawning a console helper without this
/// flag would flash a new terminal window for every tool invocation.
#[cfg(windows)]
fn hide_console(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_: &mut std::process::Command) {}

pub fn hidden_std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    hide_console(&mut command);
    command
}

pub fn hidden_tokio_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    hide_console(command.as_std_mut());
    command
}

pub fn process_alive(pid: u32) -> bool {
    crate::sys::process_alive(pid)
}

pub fn terminate_process_tree(pid: u32, force: bool) {
    crate::sys::terminate_process_tree(pid, force);
}

pub fn terminate_process(pid: u32, force: bool) {
    crate::sys::terminate_process(pid, force);
}

pub fn shell_command() -> (&'static str, &'static str) {
    crate::sys::shell_command()
}

/// 把 glibc arena 里已释放的整段内存还给操作系统。大块临时解析（models.dev
/// 目录、整回合请求体）释放后 glibc 默认把页留在 arena 里，RSS 不降。
/// 非 glibc 分配器下是无害空转。
#[cfg(target_os = "linux")]
pub fn trim_process_memory() {
    unsafe {
        libc::malloc_trim(0);
    }
}

/// 非 Linux 上没有 `malloc_trim`（那是 glibc 的扩展），空转即可。
///
/// 可见性必须和上面那支一致：`miyu-hosts` 的 `runtime/mod.rs` 把它再导出一层，
/// 写成 `pub(crate)` 的话在 macOS 上就是 E0603——而 Linux 侧编得过，所以这个
/// 错一直藏到 2026-09-22 加上 macOS CI 的第一次运行才现形。
#[cfg(not(target_os = "linux"))]
pub fn trim_process_memory() {}

/// 外部程序不在 `PATH` 上时，把 `os error 2` 换成一句能照着做的话。
///
/// 09-22 在 macOS 上实测：那台机器没有 ripgrep，模型调 `grep` / `glob` 收到的
/// 整句话就是 `No such file or directory (os error 2)`——连缺的是哪个程序都没说。
/// 模型拿到这种错只会去猜路径、换参数，白烧几轮。Linux 上看不见是因为 Arch 包
/// 把 `ripgrep` 写进了依赖，装 Miyu 就一起装上了。
///
/// 只认 `NotFound`：别的 io 错误（权限、管道断）原样往上抛，不要拿安装提示去盖
/// 一个不相干的故障。
pub fn missing_program(program: &str, error: &std::io::Error) -> Option<String> {
    if error.kind() != std::io::ErrorKind::NotFound {
        return None;
    }
    let install = match program {
        "rg" => Some(
            "brew install ripgrep (macOS), apt install ripgrep (Debian/Ubuntu), \
             dnf install ripgrep (Fedora), pacman -S ripgrep (Arch)",
        ),
        "chafa" => Some(
            "brew install chafa (macOS), apt install chafa (Debian/Ubuntu), \
             dnf install chafa (Fedora), pacman -S chafa (Arch)",
        ),
        _ => None,
    };
    Some(match install {
        Some(install) => format!("`{program}` is not on PATH. Install it: {install}."),
        None => format!("`{program}` is not on PATH."),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_constructor_preserves_program_and_arguments() {
        let mut command = hidden_std_command("miyu-hidden-child-test");
        command.args(["one", "two"]);
        assert_eq!(command.get_program(), "miyu-hidden-child-test");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("one"), OsStr::new("two")]
        );
    }

    #[tokio::test]
    async fn tokio_constructor_keeps_captured_output_working() {
        let mut command = if cfg!(windows) {
            let mut command = hidden_tokio_command("cmd");
            command.args(["/C", "echo hidden-child-ok"]);
            command
        } else {
            let mut command = hidden_tokio_command("sh");
            command.args(["-c", "printf hidden-child-ok"]);
            command
        };
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("hidden-child-ok"));
    }

    #[test]
    fn a_missing_program_is_named_with_a_way_to_install_it() {
        let error = std::io::Error::from(std::io::ErrorKind::NotFound);
        let hint = missing_program("rg", &error).expect("NotFound should produce a hint");
        assert!(hint.contains("`rg` is not on PATH"), "{hint}");
        assert!(hint.contains("brew install ripgrep"), "{hint}");
    }

    #[test]
    fn other_io_failures_keep_their_own_error() {
        // 权限被拒不是「没装」,拿安装提示去盖它会把人带偏。
        let error = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(missing_program("rg", &error).is_none());
    }

    #[test]
    fn an_unknown_program_still_says_which_one_is_missing() {
        let error = std::io::Error::from(std::io::ErrorKind::NotFound);
        let hint = missing_program("totally-unknown", &error).expect("still a hint");
        assert_eq!(hint, "`totally-unknown` is not on PATH.");
    }
}
