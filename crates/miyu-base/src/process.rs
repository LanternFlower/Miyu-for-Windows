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

#[cfg(not(target_os = "linux"))]
pub fn trim_process_memory() {}

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
}
