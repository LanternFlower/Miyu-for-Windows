//! Headless child-process constructors.
//!
//! Miyu's daemon is a GUI-less background process on Windows. Starting a
//! console program from it without `CREATE_NO_WINDOW` makes Windows create a
//! visible console for every tool call. Keep that platform detail here so
//! command-backed tools do not each grow their own flag handling.

use std::ffi::OsStr;

#[cfg(windows)]
fn hide_console(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_: &mut std::process::Command) {}

pub(crate) fn hidden_std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    hide_console(&mut command);
    command
}

pub(crate) fn hidden_tokio_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    hide_console(command.as_std_mut());
    command
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
}
