//! 系统探针：命令、进程、包管理、日志。
//!
//! 所有输出都要过 `redact` 与 `mask_network_addresses` 再交出去——诊断报告会被
//! 贴到聊天里，里面可能带路径、IP、token。
//!
//! `safe_command_name` 限制能执行什么：目标名来自模型，直接拼进命令行就是任意
//! 命令执行。

use crate::tools::diagnostics::*;

#[derive(Debug)]
pub(in crate::tools::diagnostics) struct ProbeOutput {
    pub(in crate::tools::diagnostics) status: Option<i32>,
    pub(in crate::tools::diagnostics) stdout: String,
    pub(in crate::tools::diagnostics) stderr: String,
    pub(in crate::tools::diagnostics) timed_out: bool,
}

pub(in crate::tools::diagnostics) async fn linux_system_facts(config: &DiagnosticsPluginConfig, report: &mut EvidenceReport) {
    for key in [
        "SHELL",
        "TERM",
        "LANG",
        "XDG_SESSION_TYPE",
        "XDG_CURRENT_DESKTOP",
        "DESKTOP_SESSION",
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "GTK_IM_MODULE",
        "QT_IM_MODULE",
        "QT_IM_MODULES",
        "XMODIFIERS",
        "SDL_IM_MODULE",
    ] {
        fact_env(report, &format!("env.{key}"), key);
    }
    if let Some(text) = crate::host_info::os_release_text() {
        if let Some(name) = crate::host_info::os_release_value(&text, "PRETTY_NAME") {
            report
                .facts
                .insert("os.pretty_name".to_string(), json!(name));
        }
    }
    let uname = run_command(config, "uname", &["-a"], 2).await;
    if !uname.stdout.trim().is_empty() {
        report
            .facts
            .insert("kernel.uname".to_string(), json!(uname.stdout.trim()));
    }
}

pub(in crate::tools::diagnostics) async fn linux_basic_checks(config: &DiagnosticsPluginConfig, report: &mut EvidenceReport) {
    for command in ["systemctl", "journalctl", "loginctl", "ip", "df"] {
        command_exists_check(config, report, command).await;
    }
}

pub(in crate::tools::diagnostics) async fn linux_app_evidence(
    args: &CheckIssueArgs,
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
) {
    let Some(target) = args.target.as_deref() else {
        report
            .missing_evidence
            .push("target app was not provided".to_string());
        return;
    };
    command_exists_check(config, report, target).await;
    process_check(config, report, target).await;
    if let Some(path) = command_path(config, target).await {
        report
            .facts
            .insert("app.command_path".to_string(), json!(path.clone()));
        package_owner(config, report, &path).await;
        app_probe_version(config, report, target).await;
    }
    recent_logs(args, config, report, &[target, "error", "failed"]).await;
}

pub(in crate::tools::diagnostics) async fn linux_display_evidence(
    args: &CheckIssueArgs,
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
) {
    for service in [
        "xdg-desktop-portal.service",
        "pipewire.service",
        "wireplumber.service",
    ] {
        systemd_user_active_check(config, report, service).await;
    }
    process_check(config, report, "Xwayland").await;
    linux_gpu_evidence(config, report).await;
    recent_logs(
        args,
        config,
        report,
        &["portal", "pipewire", "wireplumber", "wayland", "xwayland"],
    )
    .await;
}

pub(in crate::tools::diagnostics) async fn linux_audio_evidence(
    args: &CheckIssueArgs,
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
) {
    for service in [
        "pipewire.service",
        "wireplumber.service",
        "pipewire-pulse.service",
    ] {
        systemd_user_active_check(config, report, service).await;
    }
    command_exists_check(config, report, "wpctl").await;
    if command_path(config, "wpctl").await.is_some() {
        let output = run_command(config, "wpctl", &["status"], 3).await;
        push_log_if_stdout(report, "wpctl status", &output);
    }
    recent_logs(
        args,
        config,
        report,
        &["pipewire", "wireplumber", "pulse", "audio"],
    )
    .await;
}

pub(in crate::tools::diagnostics) async fn linux_package_evidence(
    args: &CheckIssueArgs,
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
) {
    for command in ["pacman", "yay", "paru"] {
        command_exists_check(config, report, command).await;
    }
    report.facts.insert(
        "package.pacman_db_lock_exists".to_string(),
        json!(Path::new("/var/lib/pacman/db.lck").exists()),
    );
    recent_logs(
        args,
        config,
        report,
        &["pacman", "error", "failed", "warning"],
    )
    .await;
}

pub(in crate::tools::diagnostics) async fn linux_gpu_evidence(config: &DiagnosticsPluginConfig, report: &mut EvidenceReport) {
    command_exists_check(config, report, "lspci").await;
    if command_path(config, "lspci").await.is_some() {
        let output = run_command(config, "lspci", &["-nnk"], 4).await;
        let gpu = extract_lspci_gpu_blocks(&output.stdout);
        if !gpu.is_empty() {
            report.facts.insert("gpu.lspci".to_string(), json!(gpu));
        }
    }
    command_exists_check(config, report, "nvidia-smi").await;
}

pub(in crate::tools::diagnostics) async fn linux_network_evidence(config: &DiagnosticsPluginConfig, report: &mut EvidenceReport) {
    for command in ["ip", "resolvectl", "ping"] {
        command_exists_check(config, report, command).await;
    }
    if command_path(config, "ip").await.is_some() {
        let output = run_command(config, "ip", &["-brief", "addr"], 3).await;
        push_log(
            report,
            "ip -brief addr",
            &mask_network_addresses(&output.stdout),
        );
    }
    if command_path(config, "resolvectl").await.is_some() {
        let output = run_command(config, "resolvectl", &["status"], 3).await;
        push_log_if_stdout(report, "resolvectl status", &output);
    }
}

pub(in crate::tools::diagnostics) async fn linux_storage_evidence(config: &DiagnosticsPluginConfig, report: &mut EvidenceReport) {
    command_exists_check(config, report, "df").await;
    if command_path(config, "df").await.is_some() {
        let output = run_command(config, "df", &["-hT"], 3).await;
        push_log_if_stdout(report, "df -hT", &output);
    }
    command_exists_check(config, report, "btrfs").await;
}

pub(in crate::tools::diagnostics) async fn command_exists_check(
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
    name: &str,
) {
    let path = command_path(config, name).await;
    report.checks.push(Check {
        id: format!("command.{name}.exists"),
        status: if path.is_some() {
            CheckStatus::Ok
        } else {
            CheckStatus::Unknown
        },
        detail: if path.is_some() {
            format!("{name} is available")
        } else {
            format!("{name} is not available")
        },
        evidence: path.into_iter().collect(),
    });
}

pub(in crate::tools::diagnostics) async fn process_check(
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
    name: &str,
) -> Vec<u32> {
    let output = run_command(config, "pgrep", &["-af", name], 2).await;
    let matches = filtered_process_matches(&output.stdout, name);
    report.checks.push(Check {
        id: format!("process.{name}.running"),
        status: if matches.is_empty() {
            CheckStatus::Unknown
        } else {
            CheckStatus::Ok
        },
        detail: if matches.is_empty() {
            format!("no process matching {name} was found")
        } else {
            format!("process matching {name} is running")
        },
        evidence: if matches.is_empty() {
            Vec::new()
        } else {
            vec![clip(&matches.join("\n"), 1_000)]
        },
    });
    matches
        .iter()
        .filter_map(|line| line.split_whitespace().next()?.parse::<u32>().ok())
        .collect()
}

pub(in crate::tools::diagnostics) async fn launch_probe_target(
    args: &CheckIssueArgs,
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
    target: &str,
) -> (Vec<u32>, Option<tokio::process::Child>) {
    if !safe_command_name(target) {
        report
            .missing_evidence
            .push("launch probe skipped because target command name is not safe".to_string());
        return (Vec::new(), None);
    }
    let before = process_ids(config, target).await;
    let spawn = Command::new(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let Ok(child) = spawn else {
        report
            .missing_evidence
            .push(format!("failed to launch {target} for runtime sampling"));
        return (Vec::new(), None);
    };
    tokio::time::sleep(Duration::from_secs(args.launch_timeout_seconds)).await;
    let after = process_ids(config, target).await;
    let new_pids = after
        .iter()
        .copied()
        .filter(|pid| !before.contains(pid))
        .collect::<Vec<_>>();
    report.facts.insert(
        "launch_probe".to_string(),
        json!({"target": target, "launched_pid": child.id(), "pids_before": before, "pids_after": after, "new_pids": new_pids}),
    );
    let pids = if new_pids.is_empty() { after } else { new_pids };
    (pids, Some(child))
}

pub(in crate::tools::diagnostics) async fn process_ids(config: &DiagnosticsPluginConfig, name: &str) -> Vec<u32> {
    let output = run_command(config, "pgrep", &["-af", name], 2).await;
    filtered_process_matches(&output.stdout, name)
        .iter()
        .filter_map(|line| line.split_whitespace().next()?.parse::<u32>().ok())
        .collect()
}

pub(in crate::tools::diagnostics) fn filtered_process_matches(output: &str, name: &str) -> Vec<String> {
    let name_lower = name.to_ascii_lowercase();
    let mut matches = output
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains(&name_lower)
                && !lower.contains("pgrep -af")
                && !lower.contains("/usr/bin/bash -c")
                && !lower.contains("/bin/sh -c")
                && !line_starts_with_pid(line, std::process::id())
        })
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    matches.sort();
    matches
}

pub(in crate::tools::diagnostics) fn line_starts_with_pid(line: &str, pid: u32) -> bool {
    line.split_whitespace()
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(pid)
}

pub(in crate::tools::diagnostics) fn linux_desktop_exec_for_target(target: &str) -> Option<String> {
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    dirs.push(PathBuf::from("/usr/share/applications"));
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let exec = text.lines().find_map(|line| line.strip_prefix("Exec="));
            if path
                .file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(target))
                || exec.is_some_and(|line| command_mentions_target(line, target))
            {
                return exec.map(redact);
            }
        }
    }
    None
}

pub(in crate::tools::diagnostics) fn command_mentions_target(line: &str, target: &str) -> bool {
    line.split(|ch: char| ch.is_whitespace() || ch == '/' || ch == '=')
        .any(|part| part == target)
}

pub(in crate::tools::diagnostics) async fn systemd_user_active_check(
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
    service: &str,
) {
    let output = run_command(config, "systemctl", &["--user", "is-active", service], 2).await;
    report.checks.push(Check {
        id: format!("systemd_user.{service}.active"),
        status: if output.status == Some(0) {
            CheckStatus::Ok
        } else {
            CheckStatus::Warn
        },
        detail: format!("systemctl --user is-active {service}"),
        evidence: compact_evidence(&output),
    });
}

pub(in crate::tools::diagnostics) async fn app_probe_version(
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
    command: &str,
) {
    if !safe_command_name(command) {
        return;
    }
    let output = run_command(config, command, &["--version"], 2).await;
    push_log_if_stdout(report, &format!("{command} --version"), &output);
}

pub(in crate::tools::diagnostics) async fn package_owner(config: &DiagnosticsPluginConfig, report: &mut EvidenceReport, path: &str) {
    if command_path(config, "pacman").await.is_none() {
        return;
    }
    let output = run_command(config, "pacman", &["-Qo", path], 3).await;
    push_log_if_stdout(report, "pacman -Qo", &output);
}

pub(in crate::tools::diagnostics) async fn package_probe_for_command(
    config: &DiagnosticsPluginConfig,
    command_path: &str,
    target: &str,
) -> Option<String> {
    let owner = timeout(
        Duration::from_secs(5),
        Command::new("pacman")
            .args(["-Qo", command_path])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()
    .filter(|output| output.status.success())?;
    let owner_text = String::from_utf8_lossy(&owner.stdout);
    let package = package_name_from_pacman_owner(&owner_text)?;
    if !safe_command_name(&package) {
        return None;
    }
    let output = timeout(
        Duration::from_secs(10),
        Command::new("pacman")
            .args(["-Ql", &package])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()
    .filter(|output| output.status.success())?;
    let mut lines = vec![format!("package={package}"), format!("target={target}")];
    lines.extend(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| package_probe_line(line))
            .take(80)
            .map(ToString::to_string),
    );
    Some(redact(&clip(
        &lines.join("\n"),
        config.max_stdout_chars.min(4_000),
    )))
}

pub(in crate::tools::diagnostics) fn package_name_from_pacman_owner(text: &str) -> Option<String> {
    let parts = text.split_whitespace().collect::<Vec<_>>();
    if let Some(index) = parts.iter().position(|part| *part == "by" || *part == "由") {
        return parts.get(index + 1).map(|value| value.to_string());
    }
    None
}

pub(in crate::tools::diagnostics) fn package_probe_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("libgtk")
        || lower.contains("libgdk")
        || lower.contains("libqt")
        || lower.contains("platforminputcontext")
        || lower.contains("immodules")
        || lower.contains("electron")
        || lower.contains("chrome")
        || lower.ends_with(".desktop")
        || lower.contains("/bin/")
}

pub(in crate::tools::diagnostics) async fn recent_logs(
    args: &CheckIssueArgs,
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
    needles: &[&str],
) {
    if args.depth == Depth::Quick || command_path(config, "journalctl").await.is_none() {
        return;
    }
    let since = format!("-{}min", args.recent_minutes);
    let output = run_command(
        config,
        "journalctl",
        &["--user", "--since", &since, "--no-pager", "-n", "200"],
        5,
    )
    .await;
    let text = output
        .stdout
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            needles
                .iter()
                .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
        })
        .take(80)
        .collect::<Vec<_>>()
        .join("\n");
    push_log(report, "journalctl --user recent filtered", &text);
}

pub(in crate::tools::diagnostics) async fn command_path(config: &DiagnosticsPluginConfig, command: &str) -> Option<String> {
    if !safe_command_name(command) {
        return None;
    }
    let output = run_command(config, "which", &[command], 2).await;
    (output.status == Some(0))
        .then(|| {
            output
                .stdout
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .filter(|value| !value.is_empty())
}

pub(in crate::tools::diagnostics) async fn run_command(
    config: &DiagnosticsPluginConfig,
    command: &str,
    args: &[&str],
    timeout_seconds: u64,
) -> ProbeOutput {
    if !safe_command_name(command) {
        return ProbeOutput {
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
        };
    }
    let result = timeout(
        Duration::from_secs(timeout_seconds.min(config.command_timeout_seconds).max(1)),
        Command::new(command)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match result {
        Ok(Ok(output)) => ProbeOutput {
            status: output.status.code(),
            stdout: clip(
                &String::from_utf8_lossy(&output.stdout),
                config.max_stdout_chars,
            ),
            stderr: clip(
                &String::from_utf8_lossy(&output.stderr),
                config.max_stderr_chars,
            ),
            timed_out: false,
        },
        Ok(Err(err)) => ProbeOutput {
            status: None,
            stdout: String::new(),
            stderr: err.to_string(),
            timed_out: false,
        },
        Err(_) => ProbeOutput {
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
        },
    }
}

pub(in crate::tools::diagnostics) fn fact_env(report: &mut EvidenceReport, key: &str, env: &str) {
    if let Ok(value) = std::env::var(env) {
        if !value.trim().is_empty() {
            report.facts.insert(key.to_string(), json!(redact(&value)));
        }
    }
}

pub(in crate::tools::diagnostics) fn push_log_if_stdout(report: &mut EvidenceReport, source: &str, output: &ProbeOutput) {
    if !output.stdout.trim().is_empty() {
        push_log(report, source, &output.stdout);
    }
}

pub(in crate::tools::diagnostics) fn push_log(report: &mut EvidenceReport, source: &str, message: &str) {
    if !message.trim().is_empty() {
        report.logs.push(LogExcerpt {
            source: source.to_string(),
            message: clip(message, 2_000),
        });
    }
}

pub(in crate::tools::diagnostics) fn compact_evidence(output: &ProbeOutput) -> Vec<String> {
    let mut evidence = Vec::new();
    if let Some(status) = output.status {
        evidence.push(format!("exit={status}"));
    }
    if !output.stdout.trim().is_empty() {
        evidence.push(format!("stdout={}", clip(&output.stdout, 800)));
    }
    if !output.stderr.trim().is_empty() {
        evidence.push(format!("stderr={}", clip(&output.stderr, 800)));
    }
    if output.timed_out {
        evidence.push("timed_out=true".to_string());
    }
    evidence
}

pub(in crate::tools::diagnostics) fn extract_lspci_gpu_blocks(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("vga")
                || lower.contains("3d controller")
                || lower.contains("display controller")
                || lower.contains("kernel driver in use")
        })
        .take(80)
        .map(redact)
        .collect()
}

pub(in crate::tools::diagnostics) fn mask_network_addresses(text: &str) -> String {
    text.split_whitespace()
        .map(|part| {
            if part.contains('.') && part.chars().any(|ch| ch.is_ascii_digit()) {
                "<ipv4>".to_string()
            } else if part.contains(':') && part.chars().any(|ch| ch.is_ascii_hexdigit()) {
                "<ipv6-or-mac>".to_string()
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(in crate::tools::diagnostics) fn redact(value: impl AsRef<str>) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        value.as_ref().to_string()
    } else {
        value.as_ref().replace(&home, "$HOME")
    }
}

pub(in crate::tools::diagnostics) fn clip(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        format!(
            "{}...",
            value
                .chars()
                .take(max_chars.saturating_sub(3))
                .collect::<String>()
        )
    }
}

pub(in crate::tools::diagnostics) fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

pub(in crate::tools::diagnostics) fn safe_command_name(value: &str) -> bool {
    !value.trim().is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '+'))
}
