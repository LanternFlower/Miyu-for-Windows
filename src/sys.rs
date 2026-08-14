//! Cross-platform primitives. Miyu's core is Unix-first; this module keeps
//! every platform-specific primitive in one place, with Windows fallbacks
//! that preserve the Unix behavior wherever the OS allows it.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

/// Applies a Unix-style mode to `path`; a no-op where modes do not exist.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Returns the Unix-style mode of a permission set. On platforms without
/// modes every file reports `0o666`, so mode comparisons treat all files as
/// equal.
pub fn mode_of(permissions: &std::fs::Permissions) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.mode()
    }
    #[cfg(not(unix))]
    {
        let _ = permissions;
        0o666
    }
}

/// Applies a Unix-style mode to files opened with `options`; a no-op where
/// modes do not exist.
pub fn apply_mode(options: &mut OpenOptions, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    {
        let _ = (options, mode);
    }
}

// ---------------------------------------------------------------------------
// Symlinks
// ---------------------------------------------------------------------------

/// Creates `link` pointing at `target`. On Windows creating a symlink needs
/// elevated privileges or developer mode, so a failed link silently degrades
/// to copying the target into place instead.
pub fn symlink_or_copy(
    target: impl AsRef<Path>,
    link: impl AsRef<Path>,
) -> io::Result<()> {
    let target = target.as_ref();
    let link = link.as_ref();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{symlink_dir, symlink_file};

        // Without Developer Mode (or elevation) symlink creation fails with
        // either ERROR_ACCESS_DENIED or ERROR_PRIVILEGE_NOT_HELD; both mean
        // "fall back to a copy".
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
        if target.is_dir() {
            match symlink_dir(target, link) {
                Ok(()) => return Ok(()),
                Err(error)
                    if error.raw_os_error() != Some(ERROR_ACCESS_DENIED)
                        && error.raw_os_error() != Some(ERROR_PRIVILEGE_NOT_HELD) =>
                {
                    return Err(error)
                }
                Err(_) => {}
            }
            copy_dir_recursive(target, link)
        } else {
            match symlink_file(target, link) {
                Ok(()) => return Ok(()),
                Err(error)
                    if error.raw_os_error() != Some(ERROR_ACCESS_DENIED)
                        && error.raw_os_error() != Some(ERROR_PRIVILEGE_NOT_HELD) =>
                {
                    return Err(error)
                }
                Err(_) => {}
            }
            std::fs::copy(target, link).map(|_| ())
        }
    }
}

#[cfg(windows)]
fn copy_dir_recursive(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Advisory file locks
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod unix_locks {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    pub fn lock_exclusive(file: &File) -> io::Result<()> {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN
        ) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    pub fn unlock(file: &File) {
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(windows)]
mod windows_locks {
    use std::fs::File;
    use std::io;
    use std::mem;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    fn lock_region(file: &File, fail_immediately: bool) -> io::Result<bool> {
        let mut flags = LOCKFILE_EXCLUSIVE_LOCK;
        if fail_immediately {
            flags |= LOCKFILE_FAIL_IMMEDIATELY;
        }
        let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
        let result = unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut overlapped) };
        if result != 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if fail_immediately && error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    pub fn lock_exclusive(file: &File) -> io::Result<()> {
        lock_region(file, false).map(|_| ())
    }

    pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
        lock_region(file, true)
    }

    pub fn unlock(file: &File) {
        let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
        unsafe {
            UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped);
        }
    }
}

/// Blocks until an exclusive advisory lock can be taken on `file`.
pub fn lock_exclusive(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix_locks::lock_exclusive(file)
    }
    #[cfg(windows)]
    {
        windows_locks::lock_exclusive(file)
    }
}

/// Tries to take an exclusive advisory lock without blocking.
pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        unix_locks::try_lock_exclusive(file)
    }
    #[cfg(windows)]
    {
        windows_locks::try_lock_exclusive(file)
    }
}

/// Releases an exclusive advisory lock taken on `file`.
pub fn unlock(file: &File) {
    #[cfg(unix)]
    {
        unix_locks::unlock(file);
    }
    #[cfg(windows)]
    {
        windows_locks::unlock(file);
    }
}

/// Takes an exclusive advisory lock on a directory, mirroring `flock` on a
/// directory fd. Windows cannot lock directories directly, so a
/// delete-on-close lock file inside the directory is used instead.
pub fn lock_directory_exclusive(dir: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        let file = File::open(dir)?;
        unix_locks::lock_exclusive(&file)?;
        Ok(file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
        };

        let lock_path = dir.join(".miyu-migration.lock");
        let wide: Vec<u16> = lock_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null_mut(),
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_DELETE_ON_CLOSE,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_handle(handle) };
        windows_locks::lock_exclusive(&file)?;
        Ok(file)
    }
}

/// Flushes `parent` so a rename just committed into it survives a crash.
/// A no-op on Windows, where directory handles cannot be synced this way.
pub fn sync_parent(parent: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Core IPC transport: Unix domain sockets on Unix, loopback TCP on Windows.
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub type IpcStream = tokio::net::UnixStream;
#[cfg(unix)]
pub type IpcListener = tokio::net::UnixListener;

#[cfg(windows)]
pub type IpcStream = tokio::net::TcpStream;
#[cfg(windows)]
pub type IpcListener = tokio::net::TcpListener;

/// Binds the core IPC endpoint. On Windows `path` is a plain file that
/// records the bound loopback port so clients can discover the endpoint.
pub fn ipc_bind(path: &Path) -> io::Result<IpcListener> {
    #[cfg(unix)]
    {
        tokio::net::UnixListener::bind(path)
    }
    #[cfg(windows)]
    {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        std::fs::write(path, port.to_string())?;
        listener.set_nonblocking(true)?;
        tokio::net::TcpListener::from_std(listener)
    }
}

/// Connects to the core IPC endpoint bound at `path`.
pub async fn ipc_connect(path: &Path) -> io::Result<IpcStream> {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(path).await
    }
    #[cfg(windows)]
    {
        let port = ipc_port(path)?;
        tokio::net::TcpStream::connect(("127.0.0.1", port)).await
    }
}

/// Synchronous probe used by startup guards: reports whether a live core is
/// accepting connections at `path`.
pub fn ipc_probe(path: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(path).is_ok()
    }
    #[cfg(windows)]
    {
        let Ok(port) = ipc_port(path) else {
            return false;
        };
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        std::net::TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok()
    }
}

/// A connected stream pair, mirroring `UnixStream::pair` for tests.
pub async fn ipc_pair() -> io::Result<(IpcStream, IpcStream)> {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::pair()
    }
    #[cfg(windows)]
    {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let client = tokio::net::TcpStream::connect(address).await?;
        let (server, _) = listener.accept().await?;
        Ok((client, server))
    }
}

#[cfg(windows)]
fn ipc_port(path: &Path) -> io::Result<u16> {
    let text = std::fs::read_to_string(path)?;
    text.trim().parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid IPC port file {}", path.display()),
        )
    })
}

// ---------------------------------------------------------------------------
// Stdin readiness
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdinWait {
    Readable,
    HungUp,
    Timeout,
}

#[cfg(unix)]
fn unix_wait_stdin(timeout: Duration) -> StdinWait {
    let mut pollfd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if ready == 1 {
        if pollfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            StdinWait::HungUp
        } else {
            StdinWait::Readable
        }
    } else {
        StdinWait::Timeout
    }
}

#[cfg(windows)]
mod windows_stdin {
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_CHAR, FILE_TYPE_PIPE};
    use windows_sys::Win32::System::Console::{
        GetNumberOfConsoleInputEvents, GetStdHandle, STD_INPUT_HANDLE,
    };
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    const ERROR_BROKEN_PIPE: i32 = 109;

    pub fn handle() -> Option<HANDLE> {
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                None
            } else {
                Some(handle)
            }
        }
    }

    /// Non-blocking stdin readiness. `hung_up` reports a pipe whose writer
    /// side closed; consoles never report hangup.
    fn probe() -> (bool, bool) {
        let Some(handle) = handle() else {
            return (false, true);
        };
        unsafe {
            match GetFileType(handle) {
                FILE_TYPE_CHAR => {
                    let mut events = 0u32;
                    let readable =
                        GetNumberOfConsoleInputEvents(handle, &mut events) != 0 && events > 0;
                    (readable, false)
                }
                FILE_TYPE_PIPE => {
                    let mut available = 0u32;
                    let ok = PeekNamedPipe(
                        handle,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    );
                    if ok != 0 {
                        (available > 0, false)
                    } else {
                        let error = std::io::Error::last_os_error();
                        (false, error.raw_os_error() == Some(ERROR_BROKEN_PIPE))
                    }
                }
                // Disk files and anything else: reads return data or EOF.
                _ => (true, false),
            }
        }
    }

    pub fn wait(timeout: Duration) -> super::StdinWait {
        let deadline = Instant::now() + timeout;
        loop {
            let (readable, hung_up) = probe();
            if hung_up {
                return super::StdinWait::HungUp;
            }
            if readable {
                return super::StdinWait::Readable;
            }
            if Instant::now() >= deadline {
                return super::StdinWait::Timeout;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn hung_up() -> bool {
        probe().1
    }
}

/// Waits up to `timeout` for stdin to become readable. Returns whether it is
/// readable, hung up, or timed out.
pub fn wait_stdin(timeout: Duration) -> StdinWait {
    #[cfg(unix)]
    {
        unix_wait_stdin(timeout)
    }
    #[cfg(windows)]
    {
        windows_stdin::wait(timeout)
    }
}

/// Reports whether stdin has hung up (Unix PTY hangup, Windows broken pipe).
pub fn stdin_hung_up() -> bool {
    #[cfg(unix)]
    {
        unix_wait_stdin(Duration::ZERO) == StdinWait::HungUp
    }
    #[cfg(windows)]
    {
        windows_stdin::hung_up()
    }
}

/// Reads up to `max_bytes` from stdin, giving up after `timeout` without
/// leaving a thread stuck in a blocking read.
pub fn read_stdin_with_timeout(max_bytes: usize, timeout: Duration) -> io::Result<Vec<u8>> {
    use std::io::IsTerminal;

    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Ok(Vec::new());
    }
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::fd::AsRawFd;

        let fd = stdin.as_raw_fd();
        let mut buf: Vec<u8> = Vec::new();
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || buf.len() >= max_bytes {
                break;
            }
            let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
            if ready <= 0 {
                break;
            }
            let mut chunk = [0u8; 8192];
            let count = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if count == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..count as usize]);
        }
        buf.truncate(max_bytes);
        Ok(buf)
    }
    #[cfg(windows)]
    {
        use std::io::Read;

        let deadline = Instant::now() + timeout;
        let mut buf: Vec<u8> = Vec::new();
        let mut handle = stdin.lock();
        loop {
            if buf.len() >= max_bytes {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match windows_stdin::wait(remaining.min(Duration::from_millis(100))) {
                StdinWait::Readable => {
                    let mut chunk = [0u8; 8192];
                    match handle.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(count) => buf.extend_from_slice(&chunk[..count]),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                        Err(error) => return Err(error),
                    }
                }
                StdinWait::HungUp | StdinWait::Timeout => break,
            }
        }
        buf.truncate(max_bytes);
        Ok(buf)
    }
}

/// Discards pending console input; a no-op where it is not supported.
pub fn flush_stdin() {
    #[cfg(windows)]
    {
        if let Some(handle) = windows_stdin::handle() {
            unsafe {
                windows_sys::Win32::System::Console::FlushConsoleInputBuffer(handle);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

/// Reports whether a process with `pid` exists.
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid == 0 {
            return false;
        }
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        if pid == 0 {
            return false;
        }
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }
}

/// Sends a graceful (`force = false`) or forceful (`force = true`)
/// termination to `pid` and its descendants.
pub fn terminate_process_tree(pid: u32, force: bool) {
    #[cfg(unix)]
    {
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        unsafe {
            libc::killpg(pid as i32, signal);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = std::process::Command::new("taskkill");
        command.arg("/T").arg("/PID").arg(pid.to_string());
        if force {
            command.arg("/F");
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let _ = command.status();
    }
}

/// Sends a graceful (`force = false`) or forceful (`force = true`)
/// termination to the single process `pid`.
///
/// `taskkill` only delivers a graceful close to GUI windows, so on Windows a
/// console process is always terminated forcefully regardless of `force`.
pub fn terminate_process(pid: u32, force: bool) {
    #[cfg(unix)]
    {
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        unsafe {
            libc::kill(pid as libc::pid_t, signal);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = force;
        let mut command = std::process::Command::new("taskkill");
        command.arg("/PID").arg(pid.to_string()).arg("/F");
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let _ = command.status();
    }
}

/// The shell used to run foreground and background commands, with the flag
/// that makes it execute the following argument.
#[cfg(unix)]
pub fn shell_command() -> (&'static str, &'static str) {
    ("sh", "-lc")
}

#[cfg(windows)]
pub fn shell_command() -> (&'static str, &'static str) {
    ("cmd", "/C")
}

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

/// Reports whether `path` is an executable file.
pub fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        metadata.is_file() && mode_of(&metadata.permissions()) & 0o111 != 0
    }
    #[cfg(windows)]
    {
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        let pathext = std::env::var_os("PATHEXT")
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            return false;
        };
        pathext
            .to_string_lossy()
            .split(';')
            .any(|candidate| candidate.eq_ignore_ascii_case(&format!(".{extension}")))
    }
}
