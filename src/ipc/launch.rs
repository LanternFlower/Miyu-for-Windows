//! daemon 的启动配置与口令管理。
//!
//! 启动配置是**两阶段提交**：先写候选（`save_daemon_launch_config`），daemon
//! 真的起来了再提交（`commit_daemon_launch_config`），起不来就丢弃。否则一次失
//! 败的启动会把好的配置覆盖掉。
//!
//! 口令文件走 `write_private_state`（0600 + 原子替换）。
//! `recover_legacy_daemon_launch_from_cmdline` 是给老版本留的：配置文件还没有
//! 的时候，从跑着的进程命令行里把端口和口令捞回来。

use crate::ipc::*;

pub const DEFAULT_WEB_PORT: u16 = 8300;

/// Access URLs for the WebUI: loopback plus every local IPv4 address.
/// Shared between the daemon (startup banner) and the CLI (`miyu web` /
/// `--status` output).
pub fn web_access_urls(port: u16) -> Vec<String> {
    web_access_urls_for(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), port)
}

/// Access URLs honoring the daemon's actual bind address: a loopback bind
/// yields only the localhost URL, a concrete interface bind yields only
/// that address, and an unspecified bind enumerates every local IPv4.
pub fn web_access_urls_for(bind: std::net::IpAddr, port: u16) -> Vec<String> {
    if bind.is_loopback() {
        return vec![format!("http://127.0.0.1:{port}")];
    }
    if !bind.is_unspecified() {
        return vec![format!("http://{bind}:{port}")];
    }
    let mut addresses = std::collections::BTreeSet::new();
    addresses.insert(std::net::Ipv4Addr::LOCALHOST);
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        for interface in interfaces {
            if let if_addrs::IfAddr::V4(address) = interface.addr {
                if !address.ip.is_unspecified() {
                    addresses.insert(address.ip);
                }
            }
        }
    }
    addresses
        .into_iter()
        .map(|address| format!("http://{address}:{port}"))
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DaemonLaunchConfig {
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_file: Option<PathBuf>,
    /// WebUI bind address; `None` keeps the historical 0.0.0.0 default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<std::net::IpAddr>,
}

impl Default for DaemonLaunchConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_WEB_PORT,
            password_file: None,
            bind: None,
        }
    }
}

pub fn stage_managed_web_password(paths: &MiyuPaths, password: &str) -> Result<PathBuf> {
    validate_web_password(password)?;
    let path = paths.managed_web_password_dir().join(format!(
        "password-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    write_private_state(&path, password.as_bytes())
        .with_context(|| format!("saving WebUI password to {}", path.display()))?;
    Ok(path)
}

pub fn stage_web_password_file(paths: &MiyuPaths, source: &Path) -> Result<PathBuf> {
    let contents = std::fs::read_to_string(source)
        .with_context(|| format!("reading WebUI password file: {}", source.display()))?;
    stage_managed_web_password(paths, contents.trim_end_matches(['\r', '\n']))
}

pub(crate) fn validate_web_password(password: &str) -> Result<()> {
    if password.is_empty() {
        bail!("WebUI password cannot be empty");
    }
    if password.chars().count() > 1_024 {
        bail!("WebUI password cannot exceed 1,024 characters");
    }
    Ok(())
}

pub(crate) fn try_load_daemon_launch_config(paths: &MiyuPaths) -> Result<Option<DaemonLaunchConfig>> {
    let path = paths.daemon_launch_state_file();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .with_context(|| format!("parsing daemon launch state at {}", path.display()))
}

pub(crate) fn load_daemon_launch_config(paths: &MiyuPaths) -> Result<DaemonLaunchConfig> {
    let mut config = try_load_daemon_launch_config(paths)?.unwrap_or_default();
    let Some(password_file) = config.password_file.as_ref() else {
        return Ok(config);
    };
    if password_file.exists() {
        return Ok(config);
    }
    let Some(name) = password_file.file_name() else {
        return Ok(config);
    };
    let migrated = paths.managed_web_password_dir().join(name);
    if password_file
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|parent| parent == "web-passwords")
        && migrated.exists()
    {
        config.password_file = Some(migrated);
    } else if name == "web-password" {
        let old_managed = paths.state_dir.join(name);
        if old_managed.exists() {
            config.password_file = Some(stage_web_password_file(paths, &old_managed)?);
        }
    }
    Ok(config)
}

pub(crate) fn daemon_launch_config_with_port(
    paths: &MiyuPaths,
    port: u16,
) -> Result<DaemonLaunchConfig> {
    let mut config = load_daemon_launch_config(paths)?;
    config.port = port;
    Ok(config)
}

pub(crate) fn save_daemon_launch_config(paths: &MiyuPaths, config: &DaemonLaunchConfig) -> Result<()> {
    let path = paths.daemon_launch_state_file();
    let mut bytes = serde_json::to_vec(config)?;
    bytes.push(b'\n');
    write_private_state(&path, &bytes)
        .with_context(|| format!("saving daemon launch state to {}", path.display()))
}

pub(crate) fn commit_daemon_launch_config(paths: &MiyuPaths, config: &DaemonLaunchConfig) -> Result<()> {
    let previous = try_load_daemon_launch_config(paths)?;
    save_daemon_launch_config(paths, config)?;
    if let Some(old_password) = previous.and_then(|value| value.password_file) {
        if config.password_file.as_ref() != Some(&old_password) {
            remove_managed_password(paths, &old_password);
        }
    }
    Ok(())
}

pub(crate) fn abandon_daemon_launch_candidate(paths: &MiyuPaths, config: &DaemonLaunchConfig) {
    let persisted_password = try_load_daemon_launch_config(paths)
        .ok()
        .flatten()
        .and_then(|value| value.password_file);
    if let Some(candidate) = &config.password_file {
        if persisted_password.as_ref() != Some(candidate) {
            remove_managed_password(paths, candidate);
        }
    }
}

pub(crate) fn remove_managed_password(paths: &MiyuPaths, path: &Path) {
    if path.parent() == Some(paths.managed_web_password_dir().as_path()) {
        let _ = std::fs::remove_file(path);
    }
}

pub(crate) fn remap_managed_password(
    config: &mut DaemonLaunchConfig,
    previous: &MiyuPaths,
    current: &MiyuPaths,
) {
    let Some(path) = config.password_file.as_ref() else {
        return;
    };
    if path.parent() == Some(previous.managed_web_password_dir().as_path()) {
        if let Some(name) = path.file_name() {
            config.password_file = Some(current.managed_web_password_dir().join(name));
        }
    }
}

pub(crate) fn write_private_state(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().context("Miyu state file has no parent")?;
    std::fs::create_dir_all(parent)?;
    sys::set_mode(parent, 0o700)?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        rand::random::<u64>()
    ));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        sys::apply_mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        std::io::Write::write_all(&mut file, contents)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        let directory_sync = sys::sync_parent(parent);
        finish_private_state_commit(parent, directory_sync)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn finish_private_state_commit(parent: &Path, directory_sync: std::io::Result<()>) -> Result<()> {
    if let Err(error) = directory_sync {
        tracing::warn!(
            directory = %parent.display(),
            error = %error,
            "Miyu state file was committed, but syncing its parent directory failed"
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) enum LegacyPassword {
    File(PathBuf),
    Inline(String),
}

#[cfg(target_os = "linux")]
pub(crate) struct LegacyDaemonArgs {
    pub(crate) port: u16,
    pub(crate) password: Option<LegacyPassword>,
}

#[cfg(target_os = "linux")]
pub(crate) fn parse_legacy_daemon_cmdline(cmdline: &[u8]) -> Result<LegacyDaemonArgs> {
    let args = cmdline
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>();
    let mut parsed = LegacyDaemonArgs {
        port: DEFAULT_WEB_PORT,
        password: None,
    };
    let mut index = 0;
    while index < args.len() {
        let arg = args[index];
        if arg == b"--port" {
            index += 1;
            let value = args
                .get(index)
                .context("legacy daemon --port has no value")?;
            parsed.port = parse_legacy_port(value)?;
        } else if let Some(value) = arg.strip_prefix(b"--port=") {
            parsed.port = parse_legacy_port(value)?;
        } else if arg == b"--password-file" {
            index += 1;
            let value = args
                .get(index)
                .context("legacy daemon --password-file has no value")?;
            parsed.password = Some(LegacyPassword::File(PathBuf::from(OsString::from_vec(
                value.to_vec(),
            ))));
        } else if let Some(value) = arg.strip_prefix(b"--password-file=") {
            parsed.password = Some(LegacyPassword::File(PathBuf::from(OsString::from_vec(
                value.to_vec(),
            ))));
        } else if arg == b"--password" {
            index += 1;
            let value = args
                .get(index)
                .context("legacy daemon --password has no value")?;
            parsed.password = Some(LegacyPassword::Inline(parse_legacy_password(value)?));
        } else if let Some(value) = arg.strip_prefix(b"--password=") {
            parsed.password = Some(LegacyPassword::Inline(parse_legacy_password(value)?));
        }
        index += 1;
    }
    Ok(parsed)
}

#[cfg(target_os = "linux")]
pub(crate) fn parse_legacy_port(value: &[u8]) -> Result<u16> {
    std::str::from_utf8(value)
        .context("legacy daemon port is not UTF-8")?
        .parse()
        .context("legacy daemon port is invalid")
}

#[cfg(target_os = "linux")]
pub(crate) fn parse_legacy_password(value: &[u8]) -> Result<String> {
    String::from_utf8(value.to_vec()).context("legacy daemon password is not UTF-8")
}

#[cfg(target_os = "linux")]
pub(crate) fn recover_legacy_daemon_launch(paths: &MiyuPaths, pid: u32) -> Result<DaemonLaunchConfig> {
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
        .context("reading legacy Miyu daemon arguments")?;
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd"))
        .context("reading legacy Miyu daemon working directory")?;
    recover_legacy_daemon_launch_from_cmdline(paths, &cmdline, Some(&cwd))
}

#[cfg(target_os = "linux")]
pub(crate) fn recover_legacy_daemon_launch_from_cmdline(
    paths: &MiyuPaths,
    cmdline: &[u8],
    cwd: Option<&Path>,
) -> Result<DaemonLaunchConfig> {
    let parsed = parse_legacy_daemon_cmdline(cmdline)?;
    let password_file = match parsed.password {
        Some(LegacyPassword::File(path)) => {
            let path = if path.is_relative() {
                cwd.context("legacy daemon password file is relative but its cwd is unavailable")?
                    .join(path)
            } else {
                path
            };
            Some(stage_web_password_file(paths, &path)?)
        }
        Some(LegacyPassword::Inline(password)) => {
            Some(stage_managed_web_password(paths, &password)?)
        }
        None => None,
    };
    Ok(DaemonLaunchConfig {
        port: parsed.port,
        password_file,
        // Legacy daemons predate --bind, so they were listening on 0.0.0.0.
        bind: None,
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn recover_legacy_daemon_launch(_paths: &MiyuPaths, _pid: u32) -> Result<DaemonLaunchConfig> {
    Ok(DaemonLaunchConfig::default())
}

pub fn recover_daemon_launch_if_missing(
    paths: &MiyuPaths,
    pid: u32,
) -> Result<Option<DaemonLaunchConfig>> {
    if try_load_daemon_launch_config(paths)?.is_some() {
        Ok(None)
    } else {
        recover_legacy_daemon_launch(paths, pid).map(Some)
    }
}

pub fn discard_daemon_launch_candidate(paths: &MiyuPaths, config: &DaemonLaunchConfig) {
    abandon_daemon_launch_candidate(paths, config);
}
