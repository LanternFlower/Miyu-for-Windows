mod input_method;
mod probes;
use input_method::*;
use probes::*;

use super::{ToolRegistry, ToolSpec};
use crate::config::{AppConfig, DiagnosticsPluginConfig};
use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

pub fn register(registry: &mut ToolRegistry, config: AppConfig) {
    registry.register(ToolSpec::new(
        "check_issue",
        "Collect read-only diagnostic evidence for a concrete local issue. This tool gathers facts only; it does not diagnose, rank root causes, or recommend fixes.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Original user issue. Used for auto area/target inference." },
                "area": { "type": "string", "enum": ["auto", "system", "app", "input_method", "display", "audio", "package", "gpu", "network", "storage"], "description": "Evidence collection area." },
                "target": { "type": "string", "description": "Optional app, process, command, package, or subsystem target." },
                "symptom": { "type": "string", "description": "Optional symptom label." },
                "depth": { "type": "string", "enum": ["quick", "normal", "full"], "description": "Probe depth." },
                "recent_minutes": { "type": "integer", "description": "Recent log window in minutes, clamped to 1..1440." },
                "platform": { "type": "string", "enum": ["auto", "linux", "macos"], "description": "Platform override. Prefer auto." },
                "allow_launch_probe": { "type": "boolean", "description": "For app/input_method evidence only: explicitly allow launching target to sample runtime facts. Defaults to false." },
                "launch_timeout_seconds": { "type": "integer", "description": "Seconds to wait after launch probe before sampling pids. Defaults to 3, max 15." }
            },
            "required": [],
            "additionalProperties": false
        }),
        move |args| {
            let config = config.clone();
            async move { check_issue(args, config.plugins.diagnostics.clone()).await }
        },
    ));
}

#[derive(Debug, Clone)]
struct CheckIssueArgs {
    query: Option<String>,
    area: Area,
    target: Option<String>,
    symptom: Option<String>,
    depth: Depth,
    recent_minutes: u64,
    platform: PlatformArg,
    allow_launch_probe: bool,
    launch_timeout_seconds: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Area {
    System,
    App,
    InputMethod,
    Display,
    Audio,
    Package,
    Gpu,
    Network,
    Storage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Depth {
    Quick,
    Normal,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlatformArg {
    Auto,
    Linux,
    Macos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Platform {
    Linux,
    Macos,
    Unsupported,
}

#[derive(Debug, Serialize)]
struct EvidenceReport {
    ok: bool,
    kind: &'static str,
    platform: Platform,
    query: Option<String>,
    area: Area,
    target: Option<String>,
    symptom: Option<String>,
    depth: Depth,
    facts: BTreeMap<String, Value>,
    checks: Vec<Check>,
    logs: Vec<LogExcerpt>,
    missing_evidence: Vec<String>,
    safety_notes: Vec<String>,
    recommended_next_probes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Check {
    id: String,
    status: CheckStatus,
    detail: String,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Ok,
    Warn,
    Error,
    Unknown,
}

#[derive(Debug, Serialize)]
struct LogExcerpt {
    source: String,
    message: String,
}

async fn check_issue(args: Value, config: DiagnosticsPluginConfig) -> Result<String> {
    if !config.enabled {
        bail!("diagnostics plugin is disabled");
    }
    let args = parse_args(args)?;
    let platform = detect_platform(args.platform);
    let mut report = EvidenceReport {
        ok: true,
        kind: "diagnostic_evidence",
        platform,
        query: args.query.clone(),
        area: args.area,
        target: args.target.clone(),
        symptom: args.symptom.clone(),
        depth: args.depth,
        facts: BTreeMap::new(),
        checks: Vec::new(),
        logs: Vec::new(),
        missing_evidence: Vec::new(),
        safety_notes: vec![
            "check_issue uses fixed read-only probes and does not diagnose or apply fixes"
                .to_string(),
        ],
        recommended_next_probes: Vec::new(),
    };

    match platform {
        Platform::Linux => collect_linux_evidence(&args, &config, &mut report).await,
        Platform::Macos => collect_macos_evidence(&args, &config, &mut report).await,
        Platform::Unsupported => {
            report.ok = false;
            report.checks.push(Check {
                id: "platform.supported".to_string(),
                status: CheckStatus::Error,
                detail: "only linux and macos are supported by check_issue".to_string(),
                evidence: vec![std::env::consts::OS.to_string()],
            });
        }
    }
    Ok(serde_json::to_string_pretty(&report)?)
}

fn parse_args(args: Value) -> Result<CheckIssueArgs> {
    let query = optional_string(&args, "query", 500);
    let mut target = optional_string(&args, "target", 160);
    let symptom = optional_string(&args, "symptom", 200);
    let area_raw = args
        .get("area")
        .or_else(|| args.get("mode"))
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .trim();
    let area = if area_raw == "auto" {
        let inferred = infer_area(query.as_deref(), target.as_deref())?;
        if target.is_none() {
            target = infer_target(query.as_deref().unwrap_or_default());
        }
        inferred
    } else {
        parse_area(area_raw)?
    };
    Ok(CheckIssueArgs {
        query,
        area,
        target,
        symptom,
        depth: parse_depth(
            args.get("depth")
                .and_then(Value::as_str)
                .unwrap_or("normal"),
        )?,
        recent_minutes: args
            .get("recent_minutes")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 1440),
        platform: parse_platform_arg(
            args.get("platform")
                .and_then(Value::as_str)
                .unwrap_or("auto"),
        )?,
        allow_launch_probe: args
            .get("allow_launch_probe")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        launch_timeout_seconds: args
            .get("launch_timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(3)
            .clamp(1, 15),
    })
}

fn optional_string(args: &Value, name: &str, max_chars: usize) -> Option<String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(max_chars).collect())
}

fn infer_area(query: Option<&str>, target: Option<&str>) -> Result<Area> {
    let text = query.unwrap_or_default();
    let lower = text.to_ascii_lowercase();
    if contains_any(
        text,
        &["输入法", "打不了中文", "候选框", "拼音", "fcitx", "ibus"],
    ) || contains_any(&lower, &["ime", "input method", "fcitx", "ibus"])
    {
        Ok(Area::InputMethod)
    } else if contains_any(text, &["没声音", "声音", "麦克风", "耳机"])
        || contains_any(&lower, &["audio", "sound", "pipewire", "wireplumber"])
    {
        Ok(Area::Audio)
    } else if contains_any(text, &["屏幕分享", "黑屏", "截图", "录屏", "显示器"])
        || contains_any(
            &lower,
            &["display", "screen", "wayland", "xwayland", "portal"],
        )
    {
        Ok(Area::Display)
    } else if contains_any(text, &["更新", "安装包", "依赖", "包管理"])
        || contains_any(
            &lower,
            &["pacman", "yay", "paru", "aur", "apt", "dnf", "brew"],
        )
    {
        Ok(Area::Package)
    } else if contains_any(text, &["显卡", "驱动", "独显", "核显"])
        || contains_any(&lower, &["gpu", "nvidia", "amd", "mesa", "vulkan"])
    {
        Ok(Area::Gpu)
    } else if contains_any(text, &["网络", "联网", "断网", "网卡", "wifi"])
        || contains_any(&lower, &["network", "internet", "wifi", "dns"])
    {
        Ok(Area::Network)
    } else if contains_any(text, &["磁盘", "硬盘", "空间", "挂载", "btrfs"])
        || contains_any(&lower, &["disk", "storage", "mount", "filesystem"])
    {
        Ok(Area::Storage)
    } else if target.is_some()
        || contains_any(text, &["打不开", "启动不了", "闪退", "崩溃", "报错"])
        || contains_any(&lower, &["crash", "cannot start", "won't open", "not open"])
    {
        Ok(Area::App)
    } else if text.trim().is_empty() {
        bail!("area is auto but query is empty; provide query or structured area")
    } else {
        Ok(Area::System)
    }
}

fn infer_target(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    for (needle, target) in [
        ("opencode", "opencode"),
        ("linuxqq", "qq"),
        ("qq", "qq"),
        ("微信", "wechat"),
        ("wechat", "wechat"),
        ("steam", "steam"),
        ("firefox", "firefox"),
        ("chrome", "chrome"),
        ("chromium", "chromium"),
        ("vscode", "code"),
        ("code", "code"),
    ] {
        if lower.contains(needle) || text.contains(needle) {
            return Some(target.to_string());
        }
    }
    None
}

fn parse_area(value: &str) -> Result<Area> {
    match value.trim() {
        "system" => Ok(Area::System),
        "app" => Ok(Area::App),
        "input_method" => Ok(Area::InputMethod),
        "display" => Ok(Area::Display),
        "audio" => Ok(Area::Audio),
        "package" | "package_update" => Ok(Area::Package),
        "gpu" => Ok(Area::Gpu),
        "network" => Ok(Area::Network),
        "storage" => Ok(Area::Storage),
        _ => bail!("unsupported diagnostic area: {value}"),
    }
}

fn parse_depth(value: &str) -> Result<Depth> {
    match value.trim() {
        "quick" => Ok(Depth::Quick),
        "normal" => Ok(Depth::Normal),
        "full" => Ok(Depth::Full),
        _ => bail!("unsupported diagnostic depth: {value}"),
    }
}

fn parse_platform_arg(value: &str) -> Result<PlatformArg> {
    match value.trim() {
        "auto" => Ok(PlatformArg::Auto),
        "linux" => Ok(PlatformArg::Linux),
        "macos" => Ok(PlatformArg::Macos),
        _ => bail!("unsupported diagnostic platform: {value}"),
    }
}

fn detect_platform(arg: PlatformArg) -> Platform {
    match arg {
        PlatformArg::Linux => Platform::Linux,
        PlatformArg::Macos => Platform::Macos,
        PlatformArg::Auto => match std::env::consts::OS {
            "linux" => Platform::Linux,
            "macos" => Platform::Macos,
            _ => Platform::Unsupported,
        },
    }
}

async fn collect_linux_evidence(
    args: &CheckIssueArgs,
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
) {
    linux_system_facts(config, report).await;
    match args.area {
        Area::System => linux_basic_checks(config, report).await,
        Area::App => linux_app_evidence(args, config, report).await,
        Area::InputMethod => linux_input_method_evidence(args, config, report).await,
        Area::Display => linux_display_evidence(args, config, report).await,
        Area::Audio => linux_audio_evidence(args, config, report).await,
        Area::Package => linux_package_evidence(args, config, report).await,
        Area::Gpu => linux_gpu_evidence(config, report).await,
        Area::Network => linux_network_evidence(config, report).await,
        Area::Storage => linux_storage_evidence(config, report).await,
    }
}

async fn collect_macos_evidence(
    args: &CheckIssueArgs,
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
) {
    fact_env(report, "env.shell", "SHELL");
    fact_env(report, "env.term", "TERM");
    fact_env(report, "env.lang", "LANG");
    let sw_vers = run_command(config, "sw_vers", &[], 2).await;
    push_log_if_stdout(report, "sw_vers", &sw_vers);
    if matches!(args.area, Area::App | Area::InputMethod) {
        if let Some(target) = args.target.as_deref() {
            command_exists_check(config, report, target).await;
            process_check(config, report, target).await;
        } else {
            report
                .missing_evidence
                .push("target app was not provided".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_issue_infers_input_method_area() {
        let args = parse_args(json!({"query": "QQ 打不了中文", "target": "qq"})).unwrap();
        assert!(matches!(args.area, Area::InputMethod));
    }

    #[test]
    fn input_method_path_needs_runtime_evidence() {
        let status = input_method_path_status(
            InputToolkit::Unknown,
            DisplayMode::Unknown,
            false,
            None,
            &[],
            &[],
            &[],
            &WaylandProtocolInfo {
                compositor_supports_text_input_v3: false,
                fcitx5_wayland_frontend_loaded: false,
                wayland_info_available: false,
            },
            &LocaleInfo {
                target_lang: None,
                target_lc_ctype: None,
                available_locales: vec![],
                locale_valid: false,
            },
        );
        assert_eq!(status.overall, "path_evidence_incomplete");
    }
}
