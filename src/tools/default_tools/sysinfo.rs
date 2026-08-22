//! 主机环境探测。
//!
//! 发行版、包管理器。`package_manager_guess` 是启发式的——同一个发行版可能装了
//! 好几个包管理器，猜错只是建议不对，不会造成实际后果。

use crate::tools::default_tools::*;

pub(in crate::tools) fn check_os_info() -> Result<String> {
    let mut env = BTreeMap::new();
    // PATH 与 kernel_cmdline 一并退场(08-17):两者合计占单次返回的三成
    // (实测 1,866 字符里 550+),而绝大多数问题用不上;真要看走 run_command。
    for key in [
        "SHELL",
        "TERM",
        "LANG",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_TYPE",
        "DESKTOP_SESSION",
        "WAYLAND_DISPLAY",
        "DISPLAY",
    ] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                env.insert(key, value);
            }
        }
    }
    // Shared with the `<host-environment/>` prompt block so the two never
    // disagree about what OS this is; `os_release_text` also covers the
    // `/usr/lib/os-release` fallback that image-based distros rely on.
    let os_release = crate::host_info::os_release_text();
    let arch_release = read_small_file("/etc/arch-release").is_some();
    let debian_version = read_small_file("/etc/debian_version");
    let fedora_release = read_small_file("/etc/fedora-release");
    let proc_version = read_small_file("/proc/version");
    let macos_system_version = crate::host_info::macos_system_version_text();
    let macos = parse_macos_system_version(macos_system_version.as_deref());
    let package_manager_guess = package_manager_guess(
        &os_release,
        arch_release,
        debian_version.is_some(),
        fedora_release.is_some(),
        macos_system_version.is_some(),
    );
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "platform": std::env::consts::OS,
        "os_release": os_release,
        "arch_release": arch_release,
        "debian_version": debian_version,
        "fedora_release": fedora_release,
        "macos": macos,
        "kernel_version": proc_version,
        "arch": std::env::consts::ARCH,
        "os": std::env::consts::OS,
        "family": std::env::consts::FAMILY,
        "username": std::env::var("USER").ok().or_else(|| std::env::var("USERNAME").ok()),
        "hostname": read_small_file("/etc/hostname").map(|value| value.trim().to_string()),
        "env": env,
        "package_manager_guess": package_manager_guess,
        "notes": [
            "This tool is read-only and does not execute shell commands.",
            "This only reports basic OS context. For concrete Linux input method issues, load the linux-input-method-diagnose skill."
        ],
    }))?)
}

pub(in crate::tools) fn package_manager_guess(
    os_release: &Option<String>,
    arch_release: bool,
    debian_version: bool,
    fedora_release: bool,
    macos: bool,
) -> Vec<&'static str> {
    let lower = os_release
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut managers = Vec::new();
    if arch_release || lower.contains("id=arch") || lower.contains("id_like=arch") {
        managers.push("pacman");
    }
    if debian_version
        || lower.contains("id=debian")
        || lower.contains("id=ubuntu")
        || lower.contains("id_like=debian")
    {
        managers.push("apt");
    }
    if fedora_release || lower.contains("id=fedora") || lower.contains("id_like=fedora") {
        managers.push("dnf");
    }
    if macos || std::env::consts::OS == "macos" {
        if Path::new("/opt/homebrew").exists() || Path::new("/usr/local/Homebrew").exists() {
            managers.push("brew");
        }
        if Path::new("/opt/local").exists() {
            managers.push("port");
        }
        if !managers
            .iter()
            .any(|manager| matches!(*manager, "brew" | "port"))
        {
            managers.push("brew");
        }
    }
    if managers.is_empty() {
        managers.push("unknown");
    }
    managers
}
