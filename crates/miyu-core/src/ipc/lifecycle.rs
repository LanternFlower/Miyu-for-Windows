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
    #[allow(dead_code)] // IPC Ready 帧的 DTO 字段
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
    /// 家目录那把单例锁，跟 daemon 抢的是同一把。只有走 `acquire_direct_core`
    /// 的真实启动会带上它；`acquire_direct_core_at` 那条只验 `core.lock` 本身
    /// 的路径不带（测试用）。
    home: Option<HomeSingletonLease>,
}

pub struct WebCoreLease {
    pub(crate) lock_file: File,
    pub(crate) socket_path: PathBuf,
}

pub(crate) struct StarterLease {
    pub(crate) lock_file: File,
}

/// 家目录单例租约。活到 daemon 进程结束为止，进程没了内核自动放锁——
/// 崩溃、被 kill、断电都不会留下一把谁也拿不到的死锁。
///
/// `None` 是降级放行的那一种：文件系统不支持 flock（NFS、某些容器 FS）或
/// 家目录只读时，我们照样让 daemon 起来，只是没有这道闸。
pub struct HomeSingletonLease {
    lock_file: Option<File>,
}

/// 锁文件里记的那点东西：够让后来者说清楚「谁占着」并找到它的门。
/// 端口不记——那是 IPC `Ready` 帧的事，记两份早晚对不上。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HomeDaemonRecord {
    pub pid: u32,
    pub runtime_dir: PathBuf,
}

/// 抢不到锁时的结局。`record` 可能是 `None`：锁被持有但内容还没写完
/// （对方正卡在抢到锁到写完之间的那一瞬），此时只知道「有人占着」。
pub struct HomeDaemonBusy {
    pub record: Option<HomeDaemonRecord>,
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

impl Drop for HomeSingletonLease {
    fn drop(&mut self) {
        if let Some(lock_file) = &self.lock_file {
            sys::unlock(lock_file);
        }
    }
}

fn write_home_record(file: &File, record: &HomeDaemonRecord) {
    use std::io::{Seek, SeekFrom, Write};
    let Ok(payload) = serde_json::to_vec(record) else {
        return;
    };
    let mut handle = file;
    let _ = handle.set_len(0);
    let _ = handle.seek(SeekFrom::Start(0));
    let _ = handle.write_all(&payload);
    let _ = handle.flush();
}

fn read_home_record(path: &Path) -> Option<HomeDaemonRecord> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// 抢「这个家目录的 daemon」这把锁。
///
/// 抢不到就说明同一个家目录已经有 daemon 在跑了——哪怕它算出来的
/// `runtime_dir` 跟我们的不是同一个。这正是要挡的那一种：`MIYU_HOME` 设没
/// 设会让同一个家目录算出两个运行时目录（未设是字面量 `miyu`，设了是路径
/// 哈希），两把锁互相看不见，于是 09-21 本机同时跑着两个 daemon。
///
/// 锁机制本身不可用（家目录在 NFS 上、只读挂载、flock 被内核拒绝）一律
/// **放行**：单例是省资源，不是开机前提，不能因为拿不到锁就把用户锁在门外。
pub fn acquire_home_singleton(paths: &MiyuPaths) -> Result<HomeSingletonLease, HomeDaemonBusy> {
    let path = paths.daemon_singleton_lock();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(file) = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
    else {
        return Ok(HomeSingletonLease { lock_file: None });
    };
    // 走 sys 垫片：Unix 是 flock(2)、Windows 是 LockFileEx。上游这段直接写
    // libc::flock，Windows 上编不过，两边语义由垫片统一。
    match sys::try_lock_exclusive(&file) {
        Ok(true) => {
            write_home_record(
                &file,
                &HomeDaemonRecord {
                    pid: std::process::id(),
                    runtime_dir: paths.runtime_dir(),
                },
            );
            Ok(HomeSingletonLease {
                lock_file: Some(file),
            })
        }
        // 「抢不到」才是有人占着。
        Ok(false) => Err(HomeDaemonBusy {
            record: read_home_record(&path),
        }),
        // 锁机制本身不可用（家目录在 NFS 上、只读挂载、flock 被内核拒绝）一律
        // 放行：单例是省资源，不是开机前提。
        Err(_) => Ok(HomeSingletonLease { lock_file: None }),
    }
}

/// 别的进程正占着这个家目录的 daemon 锁吗？占着就把它登记的东西给出来。
///
/// 顺序要紧：**先试锁，再读内容**。锁没被持有时文件里躺着的多半是上一任
/// daemon 的陈迹（进程没了内核放锁，文件内容原样留着），照着它去连会连到
/// 一个早就不在的 pid。
pub fn home_daemon_in_charge(paths: &MiyuPaths) -> Option<HomeDaemonRecord> {
    let path = paths.daemon_singleton_lock();
    let file = OpenOptions::new().read(true).write(true).open(&path).ok()?;
    match sys::try_lock_exclusive(&file) {
        // 拿到锁 => 没人在管；放掉再读内容（内容多半是上一任的陈迹）。
        Ok(true) => {
            sys::unlock(&file);
            None
        }
        _ => read_home_record(&path),
    }
}

pub fn acquire_direct_core(paths: &MiyuPaths) -> Result<DirectCoreLease> {
    prepare_runtime_dir(paths)?;
    // 先抢家目录那把,再抢 `core.lock`。
    //
    // 两把都要的原因:`core.lock` 住在 `runtime_dir()` 底下,而那个目录名取决
    // 于 `MIYU_HOME` 设没设(09-21 双 daemon 的同一个根)。只认 `core.lock` 的
    // 话,从没设环境变量的 shell 起的直连 REPL,跟一个设了环境变量起来的
    // daemon 会各锁各的,两边同时开着同一份数据——而「直连与 daemon 互斥」
    // 正是这把锁要保证的事。
    let home = match acquire_home_singleton(paths) {
        Ok(lease) => lease,
        Err(busy) => {
            let detail = match &busy.record {
                Some(record) => {
                    format!("（pid {} · {}）", record.pid, record.runtime_dir.display())
                }
                None => String::new(),
            };
            bail!(
                "{}{detail}",
                miyu_base::i18n::text(
                    "another Miyu core already owns this home directory; direct mode is exclusive with it — stop it (miyu daemon stop) or drop MIYU_DIRECT to attach to the daemon",
                    "这个家目录已经被另一个 Miyu 核心占着；直连模式与它互斥——先 miyu daemon stop，或去掉 MIYU_DIRECT 改为连接 daemon"
                )
            );
        }
    };
    let mut lease = acquire_direct_core_at(paths.ipc_lock())?;
    lease.home = Some(home);
    Ok(lease)
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
        home: None,
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
            miyu_base::i18n::text(
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
        if info.build_id == build_id() {
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
        if info.build_id == build_id() {
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
    // 探不到 daemon,但这个家目录的单例锁有人占着:那是一个跑在**别的**
    // runtime_dir 底下的 daemon。`runtime_dir()` 的名字取决于 `MIYU_HOME`
    // 设没设(未设是字面量 `miyu`,设了是路径哈希),所以从设了环境变量的
    // shell 起的 daemon,跟从没设的 shell 起的 CLI,彼此看不见对方的
    // socket——09-21 本机就这么同时跑着两个 daemon。
    //
    // 这时候起新的没有任何意义:它抢不到家目录锁,会立刻让位退出,用户只
    // 会等到一个「启动超时」。不如在这里就把话说清楚。
    if let Some(record) = home_daemon_in_charge(&active_paths) {
        if record.runtime_dir != active_paths.runtime_dir() {
            // 跟其它「不启动」的出口一样,把这次没用上的托管密码文件收掉,
            // 否则 `--port` 起的那一次会在 web-passwords 下留个孤儿。
            if let Some(launch) = &pending_launch {
                abandon_daemon_launch_candidate(&active_paths, launch);
            }
            bail!(
                "{}\n  pid={} runtime={}\n  {}",
                miyu_base::i18n::text(
                    "a Miyu daemon already serves this home directory, but it listens under a different runtime directory, so this environment cannot reach it",
                    "这个家目录已经有 Miyu daemon 在跑了,但它的运行时目录跟当前环境算出来的不是同一个,所以连不上它"
                ),
                record.pid,
                record.runtime_dir.display(),
                miyu_base::i18n::text(
                    "MIYU_HOME being set in one shell and unset in another is what splits them; run `miyu daemon restart` to hand the home over to this environment, or line MIYU_HOME up across both",
                    "根源是 MIYU_HOME 在一边设了、另一边没设;执行 `miyu daemon restart` 把这个家目录交给当前环境,或者把两边的 MIYU_HOME 对齐"
                )
            );
        }
    }
    let launch = pending_launch
        .map(Ok)
        .unwrap_or_else(|| load_daemon_launch_config(&active_paths))?;
    // daemon 的 stdout/stderr 全进 daemon.log(见 start_daemon_process)。
    // 它是所有启动共用的追加文件,所以失败时不能 tail 固定行数——daemon 若
    // 死在任何输出之前,尾部拿到的是上一次**成功**启动的 "Miyu WebUI: …",
    // 用户会以为起来了。记下 spawn 前的字节长度,只读这之后新增的部分。
    let log_offset = daemon_log_len(&active_paths);
    let mut child = match start_daemon_process(&active_paths, &launch) {
        Ok(child) => child,
        Err(error) => {
            abandon_daemon_launch_candidate(&active_paths, &launch);
            return Err(error);
        }
    };
    let ready_timeout = daemon_ready_timeout();
    let deadline = tokio::time::Instant::now() + ready_timeout;
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
                bail!(
                    "Miyu daemon exited before becoming ready ({status}){}",
                    daemon_log_since(&active_paths, log_offset)
                );
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
            bail!(
                "Miyu daemon did not become ready within {} seconds (override with {DAEMON_READY_TIMEOUT_ENV}){}",
                ready_timeout.as_secs(),
                daemon_log_since(&active_paths, log_offset)
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// daemon 就绪窗口的环境变量覆盖(秒)。默认 8 秒:daemon 自己的启动路径已经
/// 把慢步骤(MCP 列举)限在 3 秒内放行(见 `startup_context`),8 秒够用;
/// 留这个口子给机器特别慢、或想看清 daemon 到底卡在哪一步的人。
pub const DAEMON_READY_TIMEOUT_ENV: &str = "MIYU_DAEMON_READY_TIMEOUT_SECS";
const DEFAULT_DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(8);

fn daemon_ready_timeout() -> Duration {
    std::env::var(DAEMON_READY_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_DAEMON_READY_TIMEOUT)
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

fn daemon_log_path(paths: &MiyuPaths) -> PathBuf {
    paths.logs_dir().join("daemon.log")
}

pub(in crate::ipc) fn daemon_log_len(paths: &MiyuPaths) -> u64 {
    std::fs::metadata(daemon_log_path(paths))
        .map(|meta| meta.len())
        .unwrap_or(0)
}

/// 本次启动往 daemon.log 里写了什么。只读 `offset` 之后新增的字节——共用
/// 追加文件,读全文或 tail 固定行数都会把别人的输出算到这次头上。
///
/// 08-29 用户反馈:所有需要 daemon 的命令都只吐 "exit status: 1",真正的原因
/// (`database disk image is malformed`)躺在日志里没人看得到。
pub(in crate::ipc) fn daemon_log_since(paths: &MiyuPaths, offset: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    const MAX_BYTES: usize = 4096;
    let path = daemon_log_path(paths);
    let appended = (|| -> std::io::Result<String> {
        let mut file = std::fs::File::open(&path)?;
        let len = file.metadata()?.len();
        if len <= offset {
            return Ok(String::new());
        }
        // 只保留末尾一段:启动噪音可能很长,用户要的是最后那几句。
        let start = offset.max(len.saturating_sub(MAX_BYTES as u64));
        file.seek(SeekFrom::Start(start))?;
        let mut buffer = Vec::new();
        file.take(MAX_BYTES as u64).read_to_end(&mut buffer)?;
        Ok(String::from_utf8_lossy(&buffer).into_owned())
    })()
    .unwrap_or_default();
    let appended = appended.trim();
    if appended.is_empty() {
        // 一个字都没写就死了。指路比沉默强。
        return format!("\n(daemon 没有留下任何输出；日志：{})", path.display());
    }
    format!("\ndaemon 日志（{}）：\n{appended}", path.display())
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
    let executable = miyu_base::paths::miyu_executable()
        .context("resolving the Miyu executable to spawn the daemon")?;
    let mut command = std::process::Command::new(executable);
    command.arg("__daemon");
    // 这是「故意要活过启动者」的那一种 daemon：下面 setsid 已经把它挪出本会话，
    // 终端关了也该继续跑。带上标记，daemon 入口看到就不把命捆在我们身上
    // （见 `miyu_base::orphan_guard`）。测具直接 spawn `__daemon` 的那 66 处
    // 没有这个标记，于是谁起的谁死它就跟着走，不再攒孤儿（用户 09-20）。
    command.env(miyu_base::orphan_guard::DETACHED_ENV, "1");
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

pub(crate) fn append_daemon_process_args(
    command: &mut std::process::Command,
    launch: &DaemonLaunchConfig,
) {
    command.arg("--port").arg(launch.port.to_string());
    // password_file 是旧字段:09-11 起口令不走命令行,daemon 也不再认这个参数。
    if let Some(bind) = &launch.bind {
        command.arg("--bind").arg(bind.to_string());
    }
}
