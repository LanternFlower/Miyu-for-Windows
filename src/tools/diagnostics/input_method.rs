//! 输入法环境的探测。
//!
//! Linux 上「中文打不出来」的成因散在四五个地方：环境变量、GTK immodule 缓存、
//! 进程实际加载了哪个模块、Wayland 协议支持、locale。所以这里不下结论，只把
//! 每一处的**观测结果**摆出来——猜一个原因然后猜错，比列出证据更耽误事。

use crate::tools::diagnostics::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::tools::diagnostics) enum InputToolkit {
    ElectronChromium,
    ElectronX11,
    ElectronWayland,
    Gtk,
    Qt,
    Sdl,
    Java,
    X11Legacy,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::tools::diagnostics) enum DisplayMode {
    X11,
    XWayland,
    WaylandNative,
    Unknown,
}

#[derive(Debug, Serialize)]
pub(in crate::tools::diagnostics) struct InputMethodProfile {
    pub(in crate::tools::diagnostics) toolkit: InputToolkit,
    pub(in crate::tools::diagnostics) display_mode: DisplayMode,
    pub(in crate::tools::diagnostics) runtime_observed: bool,
    pub(in crate::tools::diagnostics) command_line: Option<String>,
    pub(in crate::tools::diagnostics) desktop_exec: Option<String>,
    pub(in crate::tools::diagnostics) target_env: Option<BTreeMap<String, String>>,
    pub(in crate::tools::diagnostics) loaded_input_modules: Vec<String>,
    pub(in crate::tools::diagnostics) available_input_modules: Vec<String>,
    pub(in crate::tools::diagnostics) immodule_cache: Vec<ImmoduleCacheEntry>,
    pub(in crate::tools::diagnostics) wayland_protocol: WaylandProtocolInfo,
    pub(in crate::tools::diagnostics) locale_info: LocaleInfo,
    pub(in crate::tools::diagnostics) path_status: InputMethodPathStatus,
}

#[derive(Debug, Clone, Serialize)]
pub(in crate::tools::diagnostics) struct ImmoduleCacheEntry {
    pub(in crate::tools::diagnostics) so_path: String,
    pub(in crate::tools::diagnostics) module_name: String,
    pub(in crate::tools::diagnostics) locales: String,
}

#[derive(Debug, Serialize)]
pub(in crate::tools::diagnostics) struct WaylandProtocolInfo {
    pub(in crate::tools::diagnostics) compositor_supports_text_input_v3: bool,
    pub(in crate::tools::diagnostics) fcitx5_wayland_frontend_loaded: bool,
    pub(in crate::tools::diagnostics) wayland_info_available: bool,
}

#[derive(Debug, Serialize)]
pub(in crate::tools::diagnostics) struct LocaleInfo {
    pub(in crate::tools::diagnostics) target_lang: Option<String>,
    pub(in crate::tools::diagnostics) target_lc_ctype: Option<String>,
    pub(in crate::tools::diagnostics) available_locales: Vec<String>,
    pub(in crate::tools::diagnostics) locale_valid: bool,
}

#[derive(Debug, Serialize)]
pub(in crate::tools::diagnostics) struct InputMethodPathStatus {
    pub(in crate::tools::diagnostics) paths: Vec<NamedPathCheck>,
    pub(in crate::tools::diagnostics) overall: String,
}

#[derive(Debug, Serialize)]
pub(in crate::tools::diagnostics) struct NamedPathCheck {
    pub(in crate::tools::diagnostics) name: String,
    pub(in crate::tools::diagnostics) status: String,
    pub(in crate::tools::diagnostics) evidence: Vec<String>,
    pub(in crate::tools::diagnostics) missing: Vec<String>,
}

pub(in crate::tools::diagnostics) async fn linux_input_method_evidence(
    args: &CheckIssueArgs,
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
) {
    for name in ["fcitx5", "ibus-daemon"] {
        process_check(config, report, name).await;
    }
    command_exists_check(config, report, "fcitx5-remote").await;
    if command_path(config, "fcitx5-remote").await.is_some() {
        let output = run_command(config, "fcitx5-remote", &[], 2).await;
        report.checks.push(Check {
            id: "input_method.fcitx5_remote".to_string(),
            status: if output.status == Some(0) {
                CheckStatus::Ok
            } else {
                CheckStatus::Warn
            },
            detail: "fcitx5-remote status probe".to_string(),
            evidence: compact_evidence(&output),
        });
    }

    let wayland_protocol = probe_wayland_protocol(config, report).await;

    let available_modules = scan_available_input_modules();
    report.facts.insert(
        "input_method.available_modules".to_string(),
        json!(available_modules.clone()),
    );

    let immodule_cache = read_gtk_immodule_cache();
    report.facts.insert(
        "input_method.immodule_cache".to_string(),
        json!(immodule_cache.clone()),
    );

    let Some(target) = args.target.as_deref() else {
        report.missing_evidence.push("target app was not provided; cannot check app toolkit, target environment, loaded .so modules, or path status".to_string());
        return;
    };
    let mut pids = process_check(config, report, target).await;
    let mut launch_child = None;
    if pids.is_empty() && args.allow_launch_probe {
        let (probe_pids, child) = launch_probe_target(args, config, report, target).await;
        pids = probe_pids;
        launch_child = child;
    }
    if pids.is_empty() {
        report.missing_evidence.push(format!(
            "target app {target} is not running; runtime environment and loaded .so modules are unavailable"
        ));
        report.recommended_next_probes.push(format!(
            "start {target}, then rerun check_issue with area=input_method and target={target}"
        ));
    }
    let target_env = pids.first().and_then(|pid| read_process_input_env(*pid));
    let loaded_modules = read_loaded_input_modules(&pids);

    let locale_info = probe_locale_info(config, report, &target_env).await;

    let socket_display_mode = probe_display_mode_via_sockets(config, report, &pids).await;

    let profile = build_input_method_profile(
        config,
        report,
        target,
        &pids,
        target_env,
        loaded_modules,
        available_modules,
        immodule_cache,
        wayland_protocol,
        locale_info,
        socket_display_mode,
    )
    .await;
    report
        .facts
        .insert("input_method.profile".to_string(), json!(profile));
    recent_logs(
        args,
        config,
        report,
        &[target, "fcitx", "ibus", "qt", "gtk", "xwayland"],
    )
    .await;
    // 诊断采样结束后回收 launch probe 拉起的目标进程，避免弹出的应用常驻。
    if let Some(mut child) = launch_child {
        let _ = child.kill().await;
    }
}

pub(in crate::tools::diagnostics) async fn probe_wayland_protocol(
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
) -> WaylandProtocolInfo {
    let wayland_info_output = run_command(config, "wayland-info", &[], 3).await;
    let wayland_info_available = wayland_info_output.status.is_some();
    let compositor_supports_text_input_v3 = wayland_info_output
        .stdout
        .contains("zwp_text_input_manager_v3");
    report.checks.push(Check {
        id: "input_method.wayland_text_input_v3".to_string(),
        status: if compositor_supports_text_input_v3 {
            CheckStatus::Ok
        } else {
            CheckStatus::Unknown
        },
        detail: "wayland-info: compositor text-input-v3 protocol support".to_string(),
        evidence: compact_evidence(&wayland_info_output),
    });

    let fcitx5_pids = process_ids(config, "fcitx5").await;
    let fcitx5_maps = fcitx5_pids
        .first()
        .and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/maps")).ok())
        .unwrap_or_default();
    let fcitx5_wayland_frontend_loaded = fcitx5_maps
        .lines()
        .any(|line| line.contains("libwaylandim.so") || line.contains("libwayland.so"));

    WaylandProtocolInfo {
        compositor_supports_text_input_v3,
        fcitx5_wayland_frontend_loaded,
        wayland_info_available,
    }
}

pub(in crate::tools::diagnostics) async fn probe_locale_info(
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
    target_env: &Option<BTreeMap<String, String>>,
) -> LocaleInfo {
    let target_lang = target_env.as_ref().and_then(|env| env.get("LANG").cloned());
    let target_lc_ctype = target_env
        .as_ref()
        .and_then(|env| env.get("LC_CTYPE").or_else(|| env.get("LC_ALL")).cloned());

    let locale_a_output = run_command(config, "locale", &["-a"], 2).await;
    let available_locales: Vec<String> = locale_a_output
        .stdout
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    report.facts.insert(
        "input_method.available_locales".to_string(),
        json!(available_locales.clone()),
    );

    let check_locale = target_lc_ctype
        .as_deref()
        .or(target_lang.as_deref())
        .unwrap_or("C");
    let locale_valid = check_locale != "C"
        && check_locale != "POSIX"
        && available_locales.iter().any(|loc| {
            loc == check_locale
                || loc.eq_ignore_ascii_case(check_locale)
                || check_locale
                    .split('.')
                    .next()
                    .is_some_and(|prefix| loc.split('.').next() == Some(prefix))
        });

    LocaleInfo {
        target_lang,
        target_lc_ctype,
        available_locales,
        locale_valid,
    }
}

pub(in crate::tools::diagnostics) async fn probe_display_mode_via_sockets(
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
    pids: &[u32],
) -> DisplayMode {
    if pids.is_empty() {
        return DisplayMode::Unknown;
    }
    let ss_output = run_command(config, "ss", &["-xp"], 3).await;
    let unix_text = std::fs::read_to_string("/proc/net/unix").unwrap_or_default();

    // /proc/net/unix 列序：Num RefCount Protocol Flags Type St Inode Path，Inode 是第 6 列。
    let x11_inodes: BTreeSet<String> = unix_text
        .lines()
        .filter(|line| line.contains("X11-unix"))
        .filter_map(|line| line.split_whitespace().nth(6).map(|s| s.to_string()))
        .collect();
    let wayland_inodes: BTreeSet<String> = unix_text
        .lines()
        .filter(|line| line.contains("wayland"))
        .filter_map(|line| line.split_whitespace().nth(6).map(|s| s.to_string()))
        .collect();

    let mut has_x11 = false;
    let mut has_wayland = false;
    for pid in pids.iter().take(8) {
        let pid_str = format!("pid={pid}");
        for line in ss_output.stdout.lines() {
            if !line.contains(&pid_str) {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            for part in &parts {
                if part.chars().all(|c| c.is_ascii_digit()) && !part.is_empty() {
                    if x11_inodes.contains(*part) {
                        has_x11 = true;
                    }
                    if wayland_inodes.contains(*part) {
                        has_wayland = true;
                    }
                }
            }
        }
    }

    report.facts.insert(
        "input_method.socket_display_mode".to_string(),
        json!({
            "has_x11_socket": has_x11,
            "has_wayland_socket": has_wayland,
        }),
    );

    match (has_x11, has_wayland) {
        (true, _) => DisplayMode::XWayland,
        (false, true) => DisplayMode::WaylandNative,
        (false, false) => DisplayMode::Unknown,
    }
}

pub(in crate::tools::diagnostics) fn read_gtk_immodule_cache() -> Vec<ImmoduleCacheEntry> {
    let mut entries = Vec::new();
    for cache_path in [
        "/usr/lib/gtk-3.0/3.0.0/immodules.cache",
        "/usr/lib/gtk-4.0/4.0.0/immodules.cache",
    ] {
        let Ok(text) = std::fs::read_to_string(cache_path) else {
            continue;
        };
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 5 {
                continue;
            }
            let so_path = parts[0].trim_matches('"').to_string();
            let module_name = parts[1].trim_matches('"').to_string();
            let locales = parts
                .get(4)
                .map(|s| s.trim_matches('"').to_string())
                .unwrap_or_default();
            entries.push(ImmoduleCacheEntry {
                so_path,
                module_name,
                locales,
            });
        }
    }
    entries
}

pub(in crate::tools::diagnostics) async fn build_input_method_profile(
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
    target: &str,
    pids: &[u32],
    target_env: Option<BTreeMap<String, String>>,
    loaded_modules: Vec<String>,
    available_modules: Vec<String>,
    immodule_cache: Vec<ImmoduleCacheEntry>,
    wayland_protocol: WaylandProtocolInfo,
    locale_info: LocaleInfo,
    socket_display_mode: DisplayMode,
) -> InputMethodProfile {
    let command_line = pids.first().and_then(|pid| read_proc_cmdline(*pid));
    let desktop_exec = linux_desktop_exec_for_target(target);
    let command_path = command_path(config, target).await;
    let package_probe = match command_path.as_deref() {
        Some(path) => package_probe_for_command(config, path, target).await,
        None => None,
    };
    if let Some(probe) = &package_probe {
        report
            .facts
            .insert("input_method.package_probe".to_string(), json!(probe));
    }
    let evidence_text = [
        target.to_string(),
        command_line.clone().unwrap_or_default(),
        desktop_exec.clone().unwrap_or_default(),
        command_path.unwrap_or_default(),
        package_probe.unwrap_or_default(),
    ]
    .join(" ");
    let raw_toolkit = infer_input_toolkit(&evidence_text);
    let display_mode = infer_display_mode(
        &evidence_text,
        target_env.as_ref(),
        socket_display_mode,
        &loaded_modules,
    );
    let toolkit = refine_electron_toolkit(raw_toolkit, display_mode);
    let path_status = input_method_path_status(
        toolkit,
        display_mode,
        !pids.is_empty() && command_line.is_some(),
        target_env.as_ref(),
        &loaded_modules,
        &available_modules,
        &immodule_cache,
        &wayland_protocol,
        &locale_info,
    );
    InputMethodProfile {
        toolkit,
        display_mode,
        runtime_observed: !pids.is_empty() && command_line.is_some(),
        command_line,
        desktop_exec,
        target_env,
        loaded_input_modules: loaded_modules,
        available_input_modules: available_modules,
        immodule_cache,
        wayland_protocol,
        locale_info,
        path_status,
    }
}

pub(in crate::tools::diagnostics) fn refine_electron_toolkit(toolkit: InputToolkit, display_mode: DisplayMode) -> InputToolkit {
    match toolkit {
        InputToolkit::ElectronChromium => match display_mode {
            DisplayMode::WaylandNative => InputToolkit::ElectronWayland,
            DisplayMode::X11 | DisplayMode::XWayland => InputToolkit::ElectronX11,
            DisplayMode::Unknown => InputToolkit::ElectronX11,
        },
        other => other,
    }
}

pub(in crate::tools::diagnostics) fn input_method_path_status(
    toolkit: InputToolkit,
    display_mode: DisplayMode,
    runtime_observed: bool,
    env: Option<&BTreeMap<String, String>>,
    loaded_modules: &[String],
    available_modules: &[String],
    immodule_cache: &[ImmoduleCacheEntry],
    wayland_protocol: &WaylandProtocolInfo,
    locale_info: &LocaleInfo,
) -> InputMethodPathStatus {
    let mut paths = Vec::new();
    let mut evidence = Vec::new();
    let mut missing = Vec::new();

    evidence.push(format!("toolkit={toolkit:?}"));
    evidence.push(format!("display_mode={display_mode:?}"));
    if !runtime_observed {
        missing.push("runtime process evidence".to_string());
    }
    if toolkit == InputToolkit::Unknown {
        missing.push("app toolkit/framework evidence".to_string());
    }
    paths.push(NamedPathCheck {
        name: "app_adapter".to_string(),
        status: if missing.is_empty() {
            "confirmed"
        } else {
            "unknown"
        }
        .to_string(),
        evidence,
        missing,
    });

    let relevant_paths = relevant_input_paths(toolkit);
    for path_name in &relevant_paths {
        let check = check_single_path(
            path_name,
            env,
            loaded_modules,
            available_modules,
            immodule_cache,
            wayland_protocol,
            locale_info,
        );
        paths.push(check);
    }

    let any_confirmed = paths.iter().skip(1).any(|p| p.status == "confirmed");
    let all_incomplete = paths
        .iter()
        .skip(1)
        .all(|p| p.status == "missing" || p.status == "unknown");
    let overall = if any_confirmed {
        "path_evidence_complete".to_string()
    } else if all_incomplete {
        "path_evidence_incomplete".to_string()
    } else {
        "path_evidence_partial".to_string()
    };

    InputMethodPathStatus { paths, overall }
}

pub(in crate::tools::diagnostics) fn relevant_input_paths(toolkit: InputToolkit) -> Vec<&'static str> {
    match toolkit {
        InputToolkit::Gtk | InputToolkit::Qt | InputToolkit::Sdl | InputToolkit::X11Legacy => {
            vec!["wayland_protocol", "toolkit_module", "xim"]
        }
        InputToolkit::ElectronX11 => vec!["gtk_module", "xim"],
        InputToolkit::ElectronWayland => vec!["wayland_protocol", "gtk_module"],
        InputToolkit::ElectronChromium => vec!["wayland_protocol", "gtk_module", "xim"],
        InputToolkit::Java => vec!["xim"],
        InputToolkit::Unknown => vec![
            "wayland_protocol",
            "gtk_module",
            "qt_module",
            "sdl_module",
            "xim",
        ],
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::tools::diagnostics) fn check_single_path(
    path_name: &str,
    env: Option<&BTreeMap<String, String>>,
    loaded_modules: &[String],
    available_modules: &[String],
    immodule_cache: &[ImmoduleCacheEntry],
    wayland_protocol: &WaylandProtocolInfo,
    locale_info: &LocaleInfo,
) -> NamedPathCheck {
    let loaded = |needles: &[&str]| loaded_module_evidence(loaded_modules, needles);
    let available = |needles: &[&str]| available_module_evidence(available_modules, needles);

    match path_name {
        "wayland_protocol" => {
            let mut ev = Vec::new();
            let mut miss = Vec::new();
            if wayland_protocol.compositor_supports_text_input_v3 {
                ev.push("compositor supports zwp_text_input_manager_v3".to_string());
            } else {
                miss.push("compositor text-input-v3 protocol support".to_string());
            }
            if wayland_protocol.fcitx5_wayland_frontend_loaded {
                ev.push("fcitx5 loaded libwaylandim.so (wayland frontend)".to_string());
            } else {
                miss.push("fcitx5 wayland frontend (libwaylandim.so)".to_string());
            }
            let status = if wayland_protocol.compositor_supports_text_input_v3
                && wayland_protocol.fcitx5_wayland_frontend_loaded
            {
                "confirmed"
            } else if wayland_protocol.compositor_supports_text_input_v3
                || wayland_protocol.fcitx5_wayland_frontend_loaded
            {
                "configured"
            } else {
                "missing"
            };
            NamedPathCheck {
                name: "wayland_protocol".to_string(),
                status: status.to_string(),
                evidence: ev,
                missing: miss,
            }
        }
        "gtk_module" | "toolkit_module" => {
            let mut ev = Vec::new();
            let mut miss = Vec::new();

            if let Some(env) = env {
                if path_name == "gtk_module" {
                    if let Some(value) = env.get("GTK_IM_MODULE").filter(|v| !v.trim().is_empty()) {
                        ev.push(format!("GTK_IM_MODULE={value}"));
                    }
                }
            }

            if let Some(item) = loaded(&["im-fcitx", "im-wayland", "im-xim", "im-ibus"]) {
                ev.push(item);
                NamedPathCheck {
                    name: path_name.to_string(),
                    status: "confirmed".to_string(),
                    evidence: ev,
                    missing: miss,
                }
            } else if let Some(item) = available(&["im-fcitx", "im-wayland", "im-xim", "im-ibus"]) {
                ev.push(format!("available_on_disk={item}"));
                let locale_match =
                    check_immodule_locale(path_name, "fcitx", immodule_cache, locale_info);
                if !locale_match.is_empty() {
                    ev.push(locale_match);
                }
                NamedPathCheck {
                    name: path_name.to_string(),
                    status: "configured".to_string(),
                    evidence: ev,
                    missing: miss,
                }
            } else {
                miss.push("GTK input module .so (neither loaded nor on disk)".to_string());
                NamedPathCheck {
                    name: path_name.to_string(),
                    status: "missing".to_string(),
                    evidence: ev,
                    missing: miss,
                }
            }
        }
        "qt_module" => {
            let mut ev = Vec::new();
            let mut miss = Vec::new();
            if let Some(env) = env {
                if let Some(value) = env.get("QT_IM_MODULE").filter(|v| !v.trim().is_empty()) {
                    ev.push(format!("QT_IM_MODULE={value}"));
                }
                if let Some(value) = env.get("QT_IM_MODULES").filter(|v| !v.trim().is_empty()) {
                    ev.push(format!("QT_IM_MODULES={value}"));
                }
            }
            if let Some(item) = loaded(&["platforminputcontext", "libfcitx", "libibus"]) {
                ev.push(item);
                NamedPathCheck {
                    name: "qt_module".to_string(),
                    status: "confirmed".to_string(),
                    evidence: ev,
                    missing: miss,
                }
            } else if let Some(item) = available(&["platforminputcontext", "fcitx"]) {
                ev.push(format!("available_on_disk={item}"));
                NamedPathCheck {
                    name: "qt_module".to_string(),
                    status: "configured".to_string(),
                    evidence: ev,
                    missing: miss,
                }
            } else {
                miss.push("Qt platforminputcontext .so evidence".to_string());
                NamedPathCheck {
                    name: "qt_module".to_string(),
                    status: "missing".to_string(),
                    evidence: ev,
                    missing: miss,
                }
            }
        }
        "sdl_module" => {
            let mut ev = Vec::new();
            let mut miss = Vec::new();
            if let Some(env) = env {
                if let Some(value) = env.get("SDL_IM_MODULE").filter(|v| !v.trim().is_empty()) {
                    ev.push(format!("SDL_IM_MODULE={value}"));
                }
            }
            if let Some(item) = loaded(&["libfcitx", "libibus", "sdl"]) {
                ev.push(item);
                NamedPathCheck {
                    name: "sdl_module".to_string(),
                    status: "confirmed".to_string(),
                    evidence: ev,
                    missing: miss,
                }
            } else {
                miss.push("SDL input bridge .so evidence".to_string());
                NamedPathCheck {
                    name: "sdl_module".to_string(),
                    status: "missing".to_string(),
                    evidence: ev,
                    missing: miss,
                }
            }
        }
        "xim" => {
            let mut ev = Vec::new();
            let mut miss = Vec::new();
            if let Some(env) = env {
                if let Some(value) = env.get("XMODIFIERS").filter(|v| !v.trim().is_empty()) {
                    ev.push(format!("XMODIFIERS={value}"));
                }
            }
            let xim_env_ok = env_has(env.unwrap_or(&BTreeMap::new()), "XMODIFIERS", "@im=fcitx");
            if !xim_env_ok {
                miss.push("XMODIFIERS=@im=fcitx not set in target env".to_string());
            }
            if !locale_info.locale_valid {
                let loc = locale_info
                    .target_lc_ctype
                    .as_deref()
                    .or(locale_info.target_lang.as_deref())
                    .unwrap_or("C");
                miss.push(format!(
                    "locale '{loc}' is C/POSIX or not in locale -a; XIM may not activate"
                ));
            }
            if let Some(item) = loaded(&["im-xim", "libx11", "libxim"]) {
                ev.push(item);
                NamedPathCheck {
                    name: "xim".to_string(),
                    status: if xim_env_ok && locale_info.locale_valid {
                        "confirmed"
                    } else {
                        "configured"
                    }
                    .to_string(),
                    evidence: ev,
                    missing: miss,
                }
            } else if let Some(item) = available(&["im-xim"]) {
                ev.push(format!("available_on_disk={item}"));
                let locale_match =
                    check_immodule_locale("gtk_module", "xim", immodule_cache, locale_info);
                if !locale_match.is_empty() {
                    ev.push(locale_match);
                }
                NamedPathCheck {
                    name: "xim".to_string(),
                    status: if xim_env_ok && locale_info.locale_valid {
                        "configured"
                    } else {
                        "missing"
                    }
                    .to_string(),
                    evidence: ev,
                    missing: miss,
                }
            } else {
                miss.push("im-xim.so not found on disk".to_string());
                NamedPathCheck {
                    name: "xim".to_string(),
                    status: "missing".to_string(),
                    evidence: ev,
                    missing: miss,
                }
            }
        }
        _ => NamedPathCheck {
            name: path_name.to_string(),
            status: "unknown".to_string(),
            evidence: vec![],
            missing: vec!["unknown path name".to_string()],
        },
    }
}

pub(in crate::tools::diagnostics) fn check_immodule_locale(
    _path_name: &str,
    module_name: &str,
    immodule_cache: &[ImmoduleCacheEntry],
    locale_info: &LocaleInfo,
) -> String {
    let target_locale = locale_info
        .target_lc_ctype
        .as_deref()
        .or(locale_info.target_lang.as_deref())
        .unwrap_or("C");
    let locale_prefix = target_locale
        .split(|c: char| c == '.' || c == '_')
        .next()
        .unwrap_or("");
    let locale_lang = locale_prefix.split('_').next().unwrap_or("");

    for entry in immodule_cache {
        if !entry.module_name.contains(module_name) {
            continue;
        }
        let locales = &entry.locales;
        if locales.contains('*') {
            return format!(
                "immodule_cache: {} matches any locale (*)",
                entry.module_name
            );
        }
        let matches = locales.split(':').any(|loc| {
            loc == locale_prefix
                || loc == target_locale
                || loc == locale_lang
                || (loc.len() == 2 && locale_lang == loc)
        });
        return if matches {
            format!(
                "immodule_cache: {} locale '{}' matches target '{}'",
                entry.module_name, locales, target_locale
            )
        } else {
            format!(
                "immodule_cache: {} locale '{}' does NOT match target '{}'",
                entry.module_name, locales, target_locale
            )
        };
    }
    String::new()
}

pub(in crate::tools::diagnostics) fn read_process_input_env(pid: u32) -> Option<BTreeMap<String, String>> {
    let raw = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    let mut picked = BTreeMap::new();
    for item in raw.split(|byte| *byte == 0) {
        let entry = String::from_utf8_lossy(item);
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        if matches!(
            key,
            "GTK_IM_MODULE"
                | "QT_IM_MODULE"
                | "QT_IM_MODULES"
                | "XMODIFIERS"
                | "SDL_IM_MODULE"
                | "GLFW_IM_MODULE"
                | "XDG_SESSION_TYPE"
                | "WAYLAND_DISPLAY"
                | "DISPLAY"
                | "LANG"
                | "LC_ALL"
                | "LC_CTYPE"
        ) {
            picked.insert(key.to_string(), redact(value));
        }
    }
    Some(picked)
}

pub(in crate::tools::diagnostics) fn read_proc_cmdline(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let parts = raw
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| redact(String::from_utf8_lossy(part)))
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(" "))
}

pub(in crate::tools::diagnostics) fn read_loaded_input_modules(pids: &[u32]) -> Vec<String> {
    let mut modules = BTreeSet::new();
    for pid in pids.iter().take(8) {
        let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/maps")) else {
            continue;
        };
        for line in text.lines() {
            if let Some(path) = input_module_path_from_maps_line(line) {
                modules.insert(format!("pid {pid}: {path}"));
            }
        }
    }
    modules.into_iter().take(80).collect()
}

pub(in crate::tools::diagnostics) fn input_module_path_from_maps_line(line: &str) -> Option<String> {
    let path = line.split_whitespace().last()?;
    let lower = path.to_ascii_lowercase();
    let is_input_module = lower.contains("/immodules/")
        || lower.contains("im-fcitx")
        || lower.contains("im-xim")
        || lower.contains("im-ibus")
        || lower.contains("im-wayland")
        || lower.contains("platforminputcontext")
        || lower.contains("libibus")
        || lower.contains("libfcitx");
    (is_input_module && (lower.ends_with(".so") || lower.contains(".so."))).then(|| redact(path))
}

pub(in crate::tools::diagnostics) fn scan_available_input_modules() -> Vec<String> {
    let mut modules = BTreeSet::new();
    for root in ["/usr/lib", "/usr/lib64", "/app/lib"] {
        scan_available_input_modules_under(Path::new(root), 0, &mut modules);
    }
    modules.into_iter().take(120).collect()
}

pub(in crate::tools::diagnostics) fn scan_available_input_modules_under(dir: &Path, depth: usize, modules: &mut BTreeSet<String>) {
    if depth > 5 || modules.len() >= 120 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten().take(300) {
        let path = entry.path();
        let text = path.display().to_string();
        let lower = text.to_ascii_lowercase();
        if path.is_dir() {
            if lower.contains("gtk")
                || lower.contains("immodules")
                || lower.contains("qt")
                || lower.contains("fcitx")
                || lower.contains("ibus")
            {
                scan_available_input_modules_under(&path, depth + 1, modules);
            }
        } else if input_module_file_name(&lower) {
            modules.insert(redact(&text));
        }
    }
}

pub(in crate::tools::diagnostics) fn input_module_file_name(lower_path: &str) -> bool {
    (lower_path.contains("/immodules/")
        || lower_path.contains("im-fcitx")
        || lower_path.contains("im-xim")
        || lower_path.contains("im-ibus")
        || lower_path.contains("im-wayland")
        || lower_path.contains("platforminputcontext"))
        && (lower_path.ends_with(".so") || lower_path.contains(".so."))
}

pub(in crate::tools::diagnostics) fn infer_input_toolkit(text: &str) -> InputToolkit {
    let lower = text.to_ascii_lowercase();
    if lower.contains("qt_im_module")
        || lower.contains("platforminputcontext")
        || lower.contains("libqt")
    {
        InputToolkit::Qt
    } else if lower.contains("electron")
        || lower.contains("chromium")
        || lower.contains("chrome-sandbox")
        || lower.contains("steamwebhelper")
        || lower.contains("--ozone-platform")
        || lower.contains("linuxqq")
    {
        InputToolkit::ElectronChromium
    } else if lower.contains("gtk") || lower.contains("gdk") || lower.contains("immodules") {
        InputToolkit::Gtk
    } else if lower.contains("sdl") {
        InputToolkit::Sdl
    } else if lower.contains("java") {
        InputToolkit::Java
    } else if lower.contains("x11") || lower.contains("xlib") {
        InputToolkit::X11Legacy
    } else {
        InputToolkit::Unknown
    }
}

pub(in crate::tools::diagnostics) fn infer_display_mode(
    text: &str,
    env: Option<&BTreeMap<String, String>>,
    socket_mode: DisplayMode,
    loaded_modules: &[String],
) -> DisplayMode {
    let lower = text.to_ascii_lowercase();
    let has_ozone_wayland = lower.contains("--ozone-platform=wayland");

    if has_ozone_wayland {
        return DisplayMode::WaylandNative;
    }

    if socket_mode == DisplayMode::XWayland || socket_mode == DisplayMode::X11 {
        return DisplayMode::XWayland;
    }
    if socket_mode == DisplayMode::WaylandNative {
        return DisplayMode::WaylandNative;
    }

    let has_im_wayland = loaded_modules
        .iter()
        .any(|m| m.to_ascii_lowercase().contains("im-wayland"));
    if has_im_wayland {
        return DisplayMode::WaylandNative;
    }

    if let Some(env) = env {
        let has_wayland = env.get("WAYLAND_DISPLAY").is_some();
        let has_display = env.get("DISPLAY").is_some();
        return match (has_wayland, has_display) {
            (true, false) => DisplayMode::WaylandNative,
            (false, true) => DisplayMode::X11,
            (true, true) => DisplayMode::XWayland,
            _ => DisplayMode::Unknown,
        };
    }

    DisplayMode::Unknown
}

pub(in crate::tools::diagnostics) fn env_has(env: &BTreeMap<String, String>, key: &str, expected: &str) -> bool {
    env.get(key)
        .map(|value| value == expected || value.split(';').any(|item| item.trim() == expected))
        .unwrap_or(false)
}

pub(in crate::tools::diagnostics) fn loaded_module_evidence(loaded_modules: &[String], needles: &[&str]) -> Option<String> {
    loaded_modules.iter().find_map(|module| {
        let lower = module.to_ascii_lowercase();
        needles
            .iter()
            .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
            .then(|| format!("runtime_loaded_module={module}"))
    })
}

pub(in crate::tools::diagnostics) fn available_module_evidence(available_modules: &[String], needles: &[&str]) -> Option<String> {
    available_modules.iter().find_map(|module| {
        let lower = module.to_ascii_lowercase();
        needles
            .iter()
            .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
            .then(|| module.to_string())
    })
}
