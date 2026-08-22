//! daemon 的启动、探活与关停。
//!
//! `ensure_daemon` 是幂等的：已经在跑就直接用，没在跑就拉起来并等它就绪。
//! 抢启动靠 `acquire_starter` 的文件锁——两个终端同时开会各起一个 daemon。
//!
//! 判活不能只看 PID 文件（`daemon_process_matches` 还要比对进程身份），因为
//! PID 会被复用；`restart_stale_daemon` 处理的是版本对不上的旧 daemon。

use crate::ipc::*;

pub const DAEMON_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub struct DaemonInfo {
    pub pid: u32,
    pub web_port: u16,
    pub web_public: bool,
    pub web_bind: Option<std::net::IpAddr>,
    pub build_id: String,
    pub protocol_version: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct DaemonProcessIdentity {
    pub(crate) pid: u32,
    #[cfg(target_os = "linux")]
    pub(crate) start_time: Option<u64>,
}

pub struct DirectCoreLease {
    pub(crate) lock_file: File,
}

pub struct WebCoreLease {
    pub(crate) lock_file: File,
    pub(crate) socket_path: PathBuf,
}

pub(crate) struct StarterLease {
    pub(crate) lock_file: File,
}

impl Drop for DirectCoreLease {
    fn drop(&mut self) {
        sys::unlock(&self.lock_file);
    }
}

impl Drop for WebCoreLease {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
        sys::unlock(&self.lock_file);
    }
}

impl Drop for StarterLease {
    fn drop(&mut self) {
        sys::unlock(&self.lock_file);
    }
}

pub fn acquire_direct_core(paths: &MiyuPaths) -> Result<DirectCoreLease> {
    prepare_runtime_dir(paths)?;
    acquire_direct_core_at(paths.ipc_lock())
}

pub fn acquire_web_core(paths: &MiyuPaths) -> Result<WebCoreLease> {
    prepare_runtime_dir(paths)?;
    let lock_file = acquire_lock(paths.ipc_lock())?;
    let socket_path = paths.ipc_socket();
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }
    Ok(WebCoreLease {
        lock_file,
        socket_path,
    })
}

pub(crate) fn prepare_runtime_dir(paths: &MiyuPaths) -> Result<()> {
    let runtime_dir = paths.runtime_dir();
    std::fs::create_dir_all(&runtime_dir)?;
    sys::set_mode(&runtime_dir, 0o700)?;
    Ok(())
}

pub(crate) fn acquire_direct_core_at(lock_path: PathBuf) -> Result<DirectCoreLease> {
    Ok(DirectCoreLease {
        lock_file: acquire_lock(lock_path)?,
    })
}

pub(crate) fn acquire_lock(lock_path: PathBuf) -> Result<File> {
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    match sys::try_lock_exclusive(&lock_file) {
        Ok(true) => {}
        Ok(false) => bail!(
            "{}",
            crate::i18n::text(
                "another Miyu core (the daemon or another direct REPL) holds this home; stop it (miyu daemon stop) or drop MIYU_DIRECT to attach to the daemon",
                "另一个 Miyu 核心(daemon 或另一个直连 REPL)正占用本机身份;直连模式与它互斥——先 miyu daemon stop,或去掉 MIYU_DIRECT 改为连接 daemon"
            )
        ),
        Err(error) => bail!("failed to lock Miyu core state: {error}"),
    }
    Ok(lock_file)
}

pub async fn connect(path: &Path) -> Result<IpcStream> {
    sys::ipc_connect(path)
        .await
        .with_context(|| format!("connecting to Miyu core at {}", path.display()))
}

pub async fn daemon_info(paths: &MiyuPaths) -> Option<DaemonInfo> {
    let socket = paths.ipc_socket();
    let frame = ping_daemon(&socket, PROTOCOL_VERSION).await?;
    match frame {
        Frame::Ready {
            pid,
            web_port,
            web_public,
            web_bind,
            build_id,
        } => Some(DaemonInfo {
            pid,
            web_port,
            web_public,
            web_bind,
            build_id,
            protocol_version: PROTOCOL_VERSION,
        }),
        Frame::Error { message, .. } => {
            let protocol_version = expected_protocol_version(&message)?;
            let Frame::Ready {
                pid,
                web_port,
                web_public,
                web_bind,
                build_id,
            } = ping_daemon(&socket, protocol_version).await?
            else {
                return None;
            };
            Some(DaemonInfo {
                pid,
                web_port,
                web_public,
                web_bind,
                build_id,
                protocol_version,
            })
        }
        _ => None,
    }
}

pub(crate) async fn ping_daemon(path: &Path, protocol_version: u16) -> Option<Frame> {
    let mut stream = tokio::time::timeout(Duration::from_millis(250), connect(path))
        .await
        .ok()?
        .ok()?;
    send(
        &mut stream,
        &Request {
            version: protocol_version,
            command: Command::Ping,
        },
    )
    .await
    .ok()?;
    tokio::time::timeout(Duration::from_millis(250), receive::<Frame>(&mut stream))
        .await
        .ok()?
        .ok()?
}

pub async fn ensure_daemon(
    paths: &MiyuPaths,
    requested: Option<&DaemonLaunchConfig>,
) -> Result<DaemonInfo> {
    let mut active_paths = paths.clone();
    let mut pending_launch = requested.cloned();
    let mut current = daemon_info(&active_paths).await;
    if current.is_none() {
        let previous_paths = active_paths.clone();
        active_paths = match MiyuPaths::new().context("refreshing Miyu paths before daemon startup")
        {
            Ok(paths) => paths,
            Err(error) => {
                if let Some(launch) = &pending_launch {
                    abandon_daemon_launch_candidate(&previous_paths, launch);
                }
                return Err(error);
            }
        };
        if let Some(launch) = &mut pending_launch {
            remap_managed_password(launch, &previous_paths, &active_paths);
        }
        current = daemon_info(&active_paths).await;
    }
    if let Some(info) = current {
        if info.build_id == BUILD_ID {
            if let Some(launch) = &pending_launch {
                abandon_daemon_launch_candidate(&active_paths, launch);
            }
            return Ok(info);
        }
        if pending_launch.is_none() {
            pending_launch = recover_daemon_launch_if_missing(&active_paths, info.pid)?;
        }
        let previous_paths = active_paths.clone();
        if let Err(error) = restart_stale_daemon(&active_paths, &info).await {
            if let Some(launch) = &pending_launch {
                abandon_daemon_launch_candidate(&active_paths, launch);
            }
            return Err(error);
        }
        active_paths = match MiyuPaths::new().context("refreshing Miyu paths after daemon shutdown")
        {
            Ok(paths) => paths,
            Err(error) => {
                if let Some(launch) = &pending_launch {
                    abandon_daemon_launch_candidate(&previous_paths, launch);
                }
                return Err(error);
            }
        };
        if let Some(launch) = &mut pending_launch {
            remap_managed_password(launch, &previous_paths, &active_paths);
        }
    }
    let _starter = loop {
        let starter = acquire_starter(&active_paths)?;
        let Some(info) = daemon_info(&active_paths).await else {
            break starter;
        };
        if info.build_id == BUILD_ID {
            if let Some(launch) = &pending_launch {
                abandon_daemon_launch_candidate(&active_paths, launch);
            }
            return Ok(info);
        }
        if pending_launch.is_none() {
            pending_launch = recover_daemon_launch_if_missing(&active_paths, info.pid)?;
        }
        let previous_paths = active_paths.clone();
        if let Err(error) = restart_stale_daemon(&active_paths, &info).await {
            if let Some(launch) = &pending_launch {
                abandon_daemon_launch_candidate(&active_paths, launch);
            }
            return Err(error);
        }
        drop(starter);
        active_paths = match MiyuPaths::new().context("refreshing Miyu paths after daemon shutdown")
        {
            Ok(paths) => paths,
            Err(error) => {
                if let Some(launch) = &pending_launch {
                    abandon_daemon_launch_candidate(&previous_paths, launch);
                }
                return Err(error);
            }
        };
        if let Some(launch) = &mut pending_launch {
            remap_managed_password(launch, &previous_paths, &active_paths);
        }
    };
    let launch = pending_launch
        .map(Ok)
        .unwrap_or_else(|| load_daemon_launch_config(&active_paths))?;
    let mut child = match start_daemon_process(&active_paths, &launch) {
        Ok(child) => child,
        Err(error) => {
            abandon_daemon_launch_candidate(&active_paths, &launch);
            return Err(error);
        }
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(info) = daemon_info(&active_paths).await {
            if let Err(error) = commit_daemon_launch_config(&active_paths, &launch) {
                let _ = child.kill();
                let _ = child.wait();
                abandon_daemon_launch_candidate(&active_paths, &launch);
                return Err(error);
            }
            spawn_daemon_reaper(child);
            return Ok(info);
        }
        match child.try_wait().context("checking Miyu daemon process") {
            Ok(Some(status)) => {
                abandon_daemon_launch_candidate(&active_paths, &launch);
                bail!("Miyu daemon exited before becoming ready ({status})");
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                abandon_daemon_launch_candidate(&active_paths, &launch);
                return Err(error);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            abandon_daemon_launch_candidate(&active_paths, &launch);
            bail!("Miyu daemon did not become ready within 8 seconds");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Shuts down a daemon left over from an older build so the caller can spawn
/// one matching the current binary.
pub(crate) async fn restart_stale_daemon(paths: &MiyuPaths, info: &DaemonInfo) -> Result<()> {
    shutdown_daemon(paths, info)
        .await
        .context("waiting for the outdated Miyu daemon to stop")
}

pub async fn shutdown_daemon(paths: &MiyuPaths, info: &DaemonInfo) -> Result<()> {
    let process = daemon_process_identity(info.pid);
    let mut stream = connect(&paths.ipc_socket()).await?;
    send(
        &mut stream,
        &Request {
            version: info.protocol_version,
            command: Command::Shutdown,
        },
    )
    .await?;
    let _ = receive::<Frame>(&mut stream).await;
    wait_for_daemon_exit(process, DAEMON_SHUTDOWN_TIMEOUT).await
}

pub fn daemon_process_identity(pid: u32) -> DaemonProcessIdentity {
    DaemonProcessIdentity {
        pid,
        #[cfg(target_os = "linux")]
        start_time: linux_process_state(pid).map(|(_, start_time)| start_time),
    }
}

pub async fn wait_for_daemon_exit(process: DaemonProcessIdentity, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !daemon_process_matches(process) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "Miyu daemon PID {} did not stop within {} seconds",
                process.pid,
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn daemon_process_matches(process: DaemonProcessIdentity) -> bool {
    let Some((state, start_time)) = linux_process_state(process.pid) else {
        return false;
    };
    state != 'Z'
        && process
            .start_time
            .is_none_or(|expected| expected == start_time)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn daemon_process_matches(process: DaemonProcessIdentity) -> bool {
    if process.pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(process.pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub(crate) fn daemon_process_matches(process: DaemonProcessIdentity) -> bool {
    sys::process_alive(process.pid)
}

#[cfg(target_os = "linux")]
pub(crate) fn linux_process_state(pid: u32) -> Option<(char, u64)> {
    if pid == 0 {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let state = fields.first()?.chars().next()?;
    // `fields[0]` is procfs field 3 (state); starttime is field 22.
    let start_time = fields.get(19)?.parse().ok()?;
    Some((state, start_time))
}

pub(crate) fn acquire_starter(paths: &MiyuPaths) -> Result<StarterLease> {
    prepare_runtime_dir(paths)?;
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.daemon_start_lock())?;
    sys::lock_exclusive(&lock_file)?;
    Ok(StarterLease { lock_file })
}

pub(crate) fn start_daemon_process(
    paths: &MiyuPaths,
    launch: &DaemonLaunchConfig,
) -> Result<std::process::Child> {
    std::fs::create_dir_all(paths.logs_dir())?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.logs_dir().join("daemon.log"))?;
    // The daemon is this very binary re-executed with a hidden subcommand,
    // so a single installed file is always sufficient.
    let executable = crate::paths::miyu_executable()
        .context("resolving the Miyu executable to spawn the daemon")?;
    let mut command = std::process::Command::new(executable);
    command.arg("__daemon");
    append_daemon_process_args(&mut command, launch);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008 | 0x0800_0000);
    }
    command.spawn().context("starting Miyu daemon")
}

pub(crate) fn spawn_daemon_reaper(mut child: std::process::Child) {
    // Reap the daemon when it eventually exits: long-lived parents (the
    // REPL) would otherwise accumulate a zombie per spawned daemon.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

pub(crate) fn append_daemon_process_args(command: &mut std::process::Command, launch: &DaemonLaunchConfig) {
    command.arg("--port").arg(launch.port.to_string());
    if let Some(path) = &launch.password_file {
        command.arg("--password-file").arg(path);
    }
    if let Some(bind) = &launch.bind {
        command.arg("--bind").arg(bind.to_string());
    }
}
