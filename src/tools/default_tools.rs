use super::{CommandOutputStream, ToolProgress, ToolRegistry, ToolSpec};
use crate::host_info::{parse_macos_system_version, read_small_file};
use crate::i18n::agent_text as t;
use crate::tools::patch_preview::write_with_patch_preview;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const MAX_READ_BYTES: u64 = 50 * 1024;
const MAX_READ_LINES: usize = 2_000;
const MAX_LINE_CHARS: usize = 2_000;
const MAX_COMMAND_OUTPUT_CHARS: usize = 20_000;
const SEARCH_TIMEOUT_SECONDS: u64 = 30;
/// 进度消息前缀:带它的内容是「本次调用的最终摘要」,由渲染层原样按行展示,
/// 而不是当成一闪而过的进度。回收站用它交代失败清单。
pub(crate) const TOOL_SUMMARY_PREFIX: &str = "__tool_summary__";

pub fn register(registry: &mut ToolRegistry, allow_command_execution: bool) {
    register_readonly(registry);
    register_run_command(registry, allow_command_execution);
    registry.register(ToolSpec::new_with_progress(
        "trash_path",
        t("Move files, directories, or symlinks to the system Trash instead of permanently deleting them. Pass every path in one call — one call per path floods the transcript. Use this when the user asks to delete/remove/clean up local paths; do not use rm unless explicitly requested.", "把文件、目录或符号链接移入系统回收站，而不是永久删除。要删多个时一次性全部传入，逐个调用会刷屏。用户要求删除/移除/清理本地路径时优先使用它；除非用户明确要求，不要使用 rm。"),
        json!({"type":"object","properties":{"paths":{"type":"array","items":{"type":"string"},"minItems":1,"description": t("Paths to move to Trash. Absolute, workspace-relative, and ~/ paths are all accepted.", "要移入回收站的路径列表。支持绝对路径、工作区相对路径和 ~/ 路径。")}},"required":["paths"],"additionalProperties":false}),
        |args, progress| async move { trash_paths(args, progress) },
    ).writes());
}

/// `run_command` 单独可注册:dev 模式只挂它(+后台任务管理),不连带
/// coreutils 可替代的读写全家(验收三轮裁剪)。
pub fn register_run_command(registry: &mut ToolRegistry, allow_command_execution: bool) {
    registry.register(ToolSpec::new_with_progress(
        "run_command",
        t("Run a shell command in the workspace when skills.allow_command_execution is enabled. Set background=true for long-running commands (builds, dev servers): it returns a job_id immediately; poll with job_status and stop with job_stop.", "当 skills.allow_command_execution 启用时，在工作区运行 shell 命令。长时命令（构建、dev server）用 background=true：立即返回 job_id，用 job_status 查询、job_stop 停止。"),
        json!({"type":"object","properties":{"command":{"type":"string","description": t("Command to run.", "要运行的命令。")},"timeout_seconds":{"type":"integer","description": t("Optional timeout in seconds (1-120, default 30). Ignored when background=true.", "可选超时时间，单位秒（1-120，默认 30）；background=true 时忽略。")},"background":{"type":"boolean","description": t("Run detached as a background command and return a short job_id immediately.", "作为后台命令分离运行，立即返回短 job_id。")},"title":{"type":"string","description": t("Short display title (<=16 chars) for the background command.", "后台命令的短标题（不超过 16 字），用于状态行显示，例如 release 构建。")}},"required":["command"],"additionalProperties":false}),
        move |args, progress| async move {
            run_command(args, allow_command_execution, progress).await
        },
    ).writes());
}

/// 只读工具集。计划模式移除后这里不再注册 `run_command`——它在
/// `register` 里紧接着就会被可写版覆盖,留着只是一份读不到的死描述。
pub fn register_readonly(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new(
        "check_os_info",
        t("Check basic read-only OS, shell, desktop session, kernel, host, and package-manager context. For concrete Linux input method issues, load the linux-input-method-diagnose skill.", "查看只读基础系统信息，包括 OS、shell、桌面会话、内核、主机和包管理器上下文。排查具体 Linux 输入法问题时先加载 linux-input-method-diagnose 技能。"),
        json!({"type":"object","properties":{},"additionalProperties":false}),
        |_| async move { check_os_info() },
    ));
    registry.register(ToolSpec::new(
        "read_file",
        t("Read a UTF-8 text file by 1-based line offset, or list a directory page. Use absolute paths, workspace-relative paths, or ~/ paths. Large files are paged and binary files are refused.", "按 1 起始行号分页读取 UTF-8 文本文件，或分页列出目录。支持绝对路径、工作区相对路径和 ~/ 路径。大文件会分页，二进制文件会被拒绝。"),
        json!({"type":"object","properties":{"path":{"type":"string","description": t("File or directory path.", "文件或目录路径。")},"offset":{"type":"integer","description": t("Starting line, 1-based.", "起始行，1 起始。")},"limit":{"type":"integer","description": t("Maximum lines to read.", "最多读取行数。")}},"required":["path"],"additionalProperties":false}),
        |args| async move { read_file(args) },
    ));
    registry.register(ToolSpec::new(
        "glob",
        t("Find files by case-insensitive glob pattern under a directory. Defaults to workspace; use ~ or /home for user files, or / for protected global search.", "在目录下按大小写不敏感 glob 模式查找文件。默认工作区；查用户文件用 ~ 或 /home，受保护的全局搜索可用 /。"),
        json!({"type":"object","properties":{"path":{"type":"string","description": t("Directory to search. Defaults to workspace; use ~ or /home for user files, or / for protected global search.", "搜索目录，默认工作区；查用户文件用 ~ 或 /home，受保护的全局搜索可用 /。")},"pattern":{"type":"string","description": t("Case-insensitive glob pattern, for example *ai*test*.", "大小写不敏感 Glob 模式，例如 *ai*测试*。")},"max_results":{"type":"integer","description": t("Maximum results.", "最多结果数。")}},"required":["pattern"],"additionalProperties":false}),
        |args| async move { glob_files(args).await },
    ));
    registry.register(ToolSpec::new(
        "grep",
        t("Search file contents using ripgrep under a directory or single file. Defaults to workspace; use ~ or /home for user files, or / for protected global search. No matches are returned as an empty ok result.", "在目录或单个文件中用 ripgrep 搜索内容。默认工作区；查用户文件用 ~ 或 /home，受保护的全局搜索可用 /。无匹配会作为成功的空结果返回。"),
        json!({"type":"object","properties":{"path":{"type":"string","description": t("Directory or file to search. Defaults to workspace; use ~ or /home for user files, or / for protected global search.", "要搜索的目录或文件，默认工作区；查用户文件用 ~ 或 /home，受保护的全局搜索可用 /。")},"pattern":{"type":"string","description": t("Regex pattern.", "正则模式。")},"include":{"type":"string","description": t("Optional case-insensitive file glob filter.", "可选大小写不敏感文件 glob 过滤。")},"max_results":{"type":"integer","description": t("Maximum matches.", "最多匹配数。")}},"required":["pattern"],"additionalProperties":false}),
        |args| async move { grep_text(args).await },
    ));
}

fn check_os_info() -> Result<String> {
    let mut env = BTreeMap::new();
    for key in [
        "SHELL",
        "TERM",
        "LANG",
        "PATH",
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
    let proc_cmdline = read_small_file("/proc/cmdline");
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
        "kernel_cmdline": proc_cmdline,
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

fn package_manager_guess(
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

pub(crate) fn read_file(args: Value) -> Result<String> {
    let path = path_arg(&args, "path")?;
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_READ_LINES as u64)
        .clamp(1, MAX_READ_LINES as u64) as usize;
    if path.is_dir() {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let suffix = if entry.file_type()?.is_dir() { "/" } else { "" };
            entries.push(format!("{}{}", entry.file_name().to_string_lossy(), suffix));
        }
        entries.sort();
        let start = offset.saturating_sub(1);
        let selected = entries
            .iter()
            .skip(start)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next = (start + selected.len() < entries.len()).then_some(offset + selected.len());
        return Ok(serde_json::to_string_pretty(&json!({
            "type": "directory-page",
            "path": path.display().to_string(),
            "offset": offset,
            "limit": limit,
            "truncated": next.is_some(),
            "next": next,
            "entries": selected,
        }))?);
    }
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        bail!("not a regular file or directory: {}", path.display())
    }
    ensure_not_binary_file(&path)?;
    let file = std::fs::File::open(&path)?;
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut bytes = 0usize;
    let mut next = None;
    for (index, line) in reader.lines().enumerate() {
        let line_number = index + 1;
        if line_number < offset {
            continue;
        }
        if lines.len() >= limit || bytes >= MAX_READ_BYTES as usize {
            next = Some(line_number);
            break;
        }
        let mut line = line?;
        if line.chars().count() > MAX_LINE_CHARS {
            line = format!(
                "{}... (line truncated to {MAX_LINE_CHARS} chars)",
                line.chars().take(MAX_LINE_CHARS).collect::<String>()
            );
        }
        let rendered = format!("{line_number}: {line}");
        bytes += rendered.len() + 1;
        if bytes > MAX_READ_BYTES as usize {
            next = Some(line_number);
            break;
        }
        lines.push(rendered);
    }
    if lines.is_empty() && offset != 1 {
        bail!("offset {offset} is out of range")
    }
    // Pagination cursor before the bulky content: truncating consumers
    // (platform tool logs cap at 2400 chars) must still see truncated/next.
    Ok(serde_json::to_string_pretty(&json!({
        "type": "text-page",
        "path": path.display().to_string(),
        "offset": offset,
        "limit": limit,
        "truncated": next.is_some(),
        "next": next,
        "content": lines.join("\n"),
    }))?)
}

fn edit_file(args: Value, progress: ToolProgress) -> Result<String> {
    let path = path_arg(&args, "path")?;
    ensure_editable_file_path(&path)?;
    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("start_line is required"))? as usize;
    let end_line = args
        .get("end_line")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("end_line is required"))? as usize;
    let replacement = args
        .get("replacement")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("replacement is required"))?;
    if start_line == 0 || end_line == 0 {
        bail!("line numbers must be 1-based")
    }
    if start_line > end_line {
        bail!("start_line must be less than or equal to end_line")
    }
    let original = std::fs::read_to_string(&path)?;
    let had_trailing_newline = original.ends_with('\n');
    let mut lines = original.lines().map(str::to_string).collect::<Vec<_>>();
    let old_line_count = lines.len();
    if start_line > old_line_count || end_line > old_line_count {
        bail!("line range {start_line}-{end_line} out of range: {old_line_count} lines")
    }
    let replacement = replacement.replace("\r\n", "\n").replace('\r', "\n");
    let replacement_lines = if replacement.is_empty() {
        Vec::new()
    } else {
        replacement.lines().map(str::to_string).collect::<Vec<_>>()
    };
    lines.splice(start_line - 1..end_line, replacement_lines);
    let mut updated = lines.join("\n");
    if had_trailing_newline && !updated.is_empty() {
        updated.push('\n');
    }
    write_with_patch_preview(
        &path,
        &original,
        &updated,
        &progress,
        serde_json::Map::from_iter([
            ("old_line_count".to_string(), json!(old_line_count)),
            ("new_line_count".to_string(), json!(lines.len())),
        ]),
    )
}

fn trash_paths(args: Value, progress: ToolProgress) -> Result<String> {
    trash_paths_with(
        args,
        &progress,
        |path| trash::delete(path).map_err(|err| anyhow::anyhow!("failed to move to trash: {err}")),
    )
}

/// 一次处理整批路径。
///
/// 逐条一次调用时,每条都要回一份带 `note`/`restore_hint` 的完整 JSON——删 12
/// 个就是 12 份几乎相同的文本刷屏,也白占模型上下文。改成整批之后,那两句提示
/// 整次只出现一次,终端上成功也只占一行。
///
/// 单条失败不中断后面的:删一批文件时,因为第 3 条没权限就把剩下 9 条也丢下,
/// 只会让模型再发一轮重试。失败项逐条收集,最后一并交代。
fn trash_paths_with(
    args: Value,
    progress: &ToolProgress,
    mut move_to_trash: impl FnMut(&Path) -> Result<()>,
) -> Result<String> {
    let inputs = paths_arg(&args)?;
    let total = inputs.len();
    let mut moved_paths = Vec::new();
    let mut failures = Vec::new();
    // 不逐条报进度:同文件系统上移入回收站就是一次 rename,十几条也在毫秒内
    // 走完,那行进度还没被画出来就被下一条覆盖了,白发一串消息。真正慢的是
    // 模型吐这串路径的时间,那段由 `preparing_phase` 的「准备删除」盖住。
    for input in &inputs {
        match trash_one(input, &mut move_to_trash) {
            Ok(path) => moved_paths.push(path),
            Err(error) => failures.push(json!({
                "path": input.display().to_string(),
                "error": error.to_string(),
            })),
        }
    }
    // 失败清单走终端的最终摘要通道:成功时一行不多,失败时逐条列出来。
    if !failures.is_empty() {
        let lines = failures
            .iter()
            .map(|failure| {
                format!(
                    "✗ {}  {}",
                    failure["path"].as_str().unwrap_or_default(),
                    failure["error"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        progress.report(format!("{TOOL_SUMMARY_PREFIX}{lines}"));
    }
    let moved = moved_paths.len();
    Ok(serde_json::to_string_pretty(&json!({
        "ok": moved > 0,
        "moved": moved,
        "failed": failures.len(),
        "total": total,
        "moved_paths": moved_paths,
        "failures": failures,
        "note": t("The paths were moved to Trash, not permanently deleted.", "这些路径已移入回收站，并未永久删除。"),
        "restore_hint": t("Open the system Trash and restore an item if needed.", "如需恢复，请打开系统回收站并还原对应项目。"),
    }))?)
}

/// 校验并移动单条路径,成功时返回它的原始绝对路径。
fn trash_one(
    input: &Path,
    move_to_trash: &mut impl FnMut(&Path) -> Result<()>,
) -> Result<String> {
    let resolved = resolve_existing_path_without_following_leaf(input)?;
    ensure_safe_trash_target(&resolved)?;
    std::fs::symlink_metadata(&resolved)?;
    let original_path = resolved.display().to_string();
    move_to_trash(&resolved)?;
    if std::fs::symlink_metadata(&resolved).is_ok() {
        bail!(
            "{}",
            t(
                "the path is still present after the move",
                "移动之后该路径依然存在"
            )
        );
    }
    Ok(original_path)
}

fn paths_arg(args: &Value) -> Result<Vec<PathBuf>> {
    let Some(values) = args.get("paths").and_then(Value::as_array) else {
        bail!(
            "{}",
            t(
                "paths must be an array of path strings",
                "paths 必须是一个路径字符串数组"
            )
        );
    };
    let paths = values
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(expand_path)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        bail!(
            "{}",
            t("paths must contain at least one path", "paths 至少要有一条路径")
        );
    }
    Ok(paths)
}

async fn glob_files(args: Value) -> Result<String> {
    let path = optional_path(&args).unwrap_or_else(super::workspace::effective_workdir);
    let search_path = prepare_search_path(&path)?;
    let pattern = required(&args, "pattern")?;
    let max_results = max_results(&args);
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(SEARCH_TIMEOUT_SECONDS),
        Command::new("rg")
            .arg("--no-config")
            .arg("--files")
            .arg("--no-messages")
            .arg("--hidden")
            .arg(format!("--iglob={pattern}"))
            .args(search_exclude_args(&search_path))
            .arg(".")
            .current_dir(&search_path)
            .stdin(Stdio::null())
            // 超时丢弃 future 时同步回收 rg,否则孤儿进程继续扫整盘。
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    search_output_limited(output, max_results)
}

async fn grep_text(args: Value) -> Result<String> {
    let path = optional_path(&args).unwrap_or_else(super::workspace::effective_workdir);
    let is_file = path.is_file();
    let search_root = if is_file {
        path.parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    } else {
        path.clone()
    };
    let search_root = prepare_search_path(&search_root)?;
    let pattern = required(&args, "pattern")?;
    let max_results = max_results(&args);
    let mut command = Command::new("rg");
    command
        .arg("--no-config")
        .arg("--line-number")
        .arg("--no-messages")
        .arg("--hidden")
        .args(search_exclude_args(&search_root))
        .arg(pattern);
    if let Some(include) = args
        .get("include")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        command.arg("--iglob").arg(include.trim());
    }
    if is_file {
        if let Some(name) = path.file_name() {
            command.arg(name);
        }
    } else {
        command.arg(".");
    }
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(SEARCH_TIMEOUT_SECONDS),
        command
            .current_dir(search_root)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    search_output_limited(output, max_results)
}

async fn run_command(args: Value, allowed: bool, progress: ToolProgress) -> Result<String> {
    if !allowed {
        bail!("{}", t("command execution is disabled; set skills.allow_command_execution=true in config.jsonc to enable run_command", "命令执行已禁用；请在 config.jsonc 中设置 skills.allow_command_execution=true 以启用 run_command"));
    }
    let command = required(&args, "command")?;
    if args
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let title = args.get("title").and_then(Value::as_str);
        return super::jobs::spawn_background(&command, title, &progress).await;
    }
    // 下限 1:timeout_seconds=0 会立即超时,命令根本没机会执行。
    let timeout = args
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 120);
    execute_command(&command, timeout, progress).await
}

async fn execute_command(command: &str, timeout: u64, progress: ToolProgress) -> Result<String> {
    let (shell, shell_flag) = crate::sys::shell_command();
    let mut command_process = Command::new(shell);
    command_process
        .arg(shell_flag)
        .arg(command)
        // Explicit cwd: shell commands must run in the turn workspace, not
        // whatever the daemon process cwd happens to be.
        .current_dir(super::workspace::effective_workdir());
    // 工具桥环境(任务#12):脚本里 `miyu tool-call` 凭这些以本回合的
    // 会话身份/来源打回 daemon 执行结构化工具,内层调用照走 guard 管线。
    if let Some(session) = super::workspace::try_session() {
        command_process.env("MIYU_SESSION", &*session);
    }
    if let Ok(origin) = serde_json::to_string(&super::workspace::current_turn_origin()) {
        command_process.env("MIYU_TURN_ORIGIN", origin);
    }
    command_process.env(
        "MIYU_BRIDGE_DEPTH",
        super::workspace::current_bridge_depth().to_string(),
    );
    command_process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command_process.process_group(0);
    let mut child = command_process.spawn()?;
    let mut process_group = CommandProcessGroup::new(child.id());
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture command stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture command stderr"))?;

    let execution = tokio::time::timeout(std::time::Duration::from_secs(timeout), async {
        tokio::join!(
            child.wait(),
            read_command_output(stdout, progress.clone(), |progress, chunk| {
                progress.report_command_output(CommandOutputStream::Stdout, chunk);
            }),
            read_command_output(stderr, progress, |progress, chunk| {
                progress.report_command_output(CommandOutputStream::Stderr, chunk);
            }),
        )
    })
    .await;

    let (status, stdout, stderr) = match execution {
        Ok((status, stdout, stderr)) => {
            process_group.disarm();
            (status?, stdout?, stderr?)
        }
        Err(elapsed) => {
            process_group.terminate();
            let _ = child.start_kill();
            let _ = child.wait().await;
            process_group.disarm();
            return Err(elapsed.into());
        }
    };
    command_output(status, stdout, stderr)
}

struct CommandProcessGroup {
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl CommandProcessGroup {
    #[cfg_attr(not(unix), allow(unused_variables))]
    fn new(child_id: Option<u32>) -> Self {
        Self {
            #[cfg(unix)]
            pgid: child_id.and_then(|id| i32::try_from(id).ok()),
        }
    }

    fn terminate(&self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }

    fn disarm(&mut self) {
        #[cfg(unix)]
        {
            self.pgid = None;
        }
    }
}

impl Drop for CommandProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Cumulative cap for collected command output. Beyond it the stream is
/// still drained (so the child never blocks on a full pipe) but no longer
/// buffered or forwarded — unbounded collection plus a clone per chunk
/// into the progress channel is a memory hazard on runaway commands.
const MAX_COMMAND_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

async fn read_command_output(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    progress: ToolProgress,
    report: impl Fn(&ToolProgress, Vec<u8>),
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut truncated = false;
    let mut buffer = [0; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(output.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let take = read.min(remaining);
        if take < read {
            truncated = true;
        }
        let chunk = buffer[..take].to_vec();
        output.extend_from_slice(&chunk);
        report(&progress, chunk);
    }
    if truncated {
        output.extend_from_slice(
            crate::i18n::text(
                "\n[output truncated at the 8MB cap]",
                "\n[输出超出 8MB 上限，已截断]",
            )
            .as_bytes(),
        );
    }
    Ok(output)
}


fn command_output(
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> Result<String> {
    let stdout = clip_output_with_meta(&String::from_utf8_lossy(&stdout));
    let stderr = clip_output_with_meta(&String::from_utf8_lossy(&stderr));
    Ok(serde_json::to_string_pretty(&json!({
        "success": status.success(),
        "exit_code": status.code(),
        "stdout": stdout.text,
        "stderr": stderr.text,
        "truncated": stdout.truncated || stderr.truncated,
        "stdout_truncated": stdout.truncated,
        "stderr_truncated": stderr.truncated,
        "stdout_omitted_chars": stdout.omitted_chars,
        "stderr_omitted_chars": stderr.omitted_chars,
    }))?)
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ClippedOutput {
    text: String,
    truncated: bool,
    omitted_chars: usize,
}

fn clip_output_with_meta(value: &str) -> ClippedOutput {
    let value = value.trim();
    let total = value.chars().count();
    if total <= MAX_COMMAND_OUTPUT_CHARS {
        return ClippedOutput {
            text: value.to_string(),
            truncated: false,
            omitted_chars: 0,
        };
    }
    let omitted = total - MAX_COMMAND_OUTPUT_CHARS;
    let tail = value
        .chars()
        .skip(omitted)
        .collect::<String>()
        .trim_start_matches('\n')
        .to_string();
    ClippedOutput {
        text: format!(
            "...[{} {omitted} {}]\n{tail}",
            t("omitted", "已省略"),
            t("chars, showing tail", "字符，显示尾部")
        ),
        truncated: true,
        omitted_chars: omitted,
    }
}

fn command_output_limited(output: std::process::Output, max_lines: usize) -> Result<String> {
    let stdout_raw = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout_raw
        .lines()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n");
    let stdout = clip_output_with_meta(&stdout);
    let stderr = clip_output_with_meta(&String::from_utf8_lossy(&output.stderr));
    let line_truncated = stdout_raw.lines().nth(max_lines).is_some();
    Ok(serde_json::to_string_pretty(&json!({
        "success": output.status.success(),
        "exit_code": output.status.code(),
        "stdout": stdout.text,
        "stderr": stderr.text,
        "truncated": line_truncated || stdout.truncated || stderr.truncated,
        "stdout_truncated": line_truncated || stdout.truncated,
        "stderr_truncated": stderr.truncated,
        "stdout_omitted_chars": stdout.omitted_chars,
        "stderr_omitted_chars": stderr.omitted_chars,
        "max_results": max_lines
    }))?)
}

fn search_output_limited(output: std::process::Output, max_lines: usize) -> Result<String> {
    if output.status.code() == Some(1) && output.stdout.is_empty() {
        let stderr = clip_output_with_meta(&String::from_utf8_lossy(&output.stderr));
        return Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "exit_code": 0,
            "stdout": "",
            "stderr": stderr.text,
            "truncated": stderr.truncated,
            "stdout_truncated": false,
            "stderr_truncated": stderr.truncated,
            "stdout_omitted_chars": 0,
            "stderr_omitted_chars": stderr.omitted_chars,
            "max_results": max_lines,
            "matches": 0,
            "note": "no matches"
        }))?);
    }
    command_output_limited(output, max_lines)
}

fn prepare_search_path(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if path == Path::new("/usr") || path == Path::new("/var") || path == Path::new("/etc") {
        bail!(
            "refusing broad system search path: {}; use / for protected global search or choose a specific subdirectory",
            path.display()
        );
    }
    Ok(path)
}

fn search_exclude_args(search_root: &Path) -> Vec<String> {
    let mut args = vec!["--glob=!**/.git/**".to_string()];
    if search_root == Path::new("/") {
        args.extend(
            [
                "--glob=!dev/**",
                "--glob=!proc/**",
                "--glob=!sys/**",
                "--glob=!run/**",
                "--glob=!tmp/**",
                "--glob=!var/cache/**",
                "--glob=!var/lib/**",
                "--glob=!var/log/**",
                "--glob=!usr/**",
                "--glob=!nix/**",
                "--glob=!snap/**",
                "--glob=!flatpak/**",
            ]
            .into_iter()
            .map(ToString::to_string),
        );
    }
    args
}

fn ensure_not_binary_file(path: &Path) -> Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut buffer = [0u8; 8192];
    let read = file.read(&mut buffer)?;
    let sample = &buffer[..read];
    if sample.contains(&0) {
        bail!("cannot read binary file: {}", path.display())
    }
    let non_printable = sample
        .iter()
        .filter(|byte| **byte < 9 || (**byte > 13 && **byte < 32))
        .count();
    if !sample.is_empty() && non_printable * 10 > sample.len() * 3 {
        bail!("cannot read binary file: {}", path.display())
    }
    Ok(())
}

fn ensure_editable_file_path(path: &Path) -> Result<()> {
    let canonical = path.canonicalize()?;
    if !canonical.is_file() {
        bail!("not a regular file: {}", path.display())
    }
    Ok(())
}

fn resolve_existing_path_without_following_leaf(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("refusing to trash a root path: {}", path.display()))?;
    let parent = parent.canonicalize()?;
    let resolved = parent.join(filename);
    std::fs::symlink_metadata(&resolved)?;
    Ok(resolved)
}

fn ensure_safe_trash_target(path: &Path) -> Result<()> {
    let cwd = super::workspace::effective_workdir().canonicalize()?;
    let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    let dangerous = [
        Path::new("/"),
        Path::new("/bin"),
        Path::new("/boot"),
        Path::new("/dev"),
        Path::new("/etc"),
        Path::new("/home"),
        Path::new("/opt"),
        Path::new("/proc"),
        Path::new("/root"),
        Path::new("/run"),
        Path::new("/sbin"),
        Path::new("/sys"),
        Path::new("/tmp"),
        Path::new("/usr"),
        Path::new("/var"),
    ];
    if dangerous.iter().any(|item| path == *item) {
        bail!(
            "refusing to trash dangerous system path: {}",
            path.display()
        )
    }
    if path == cwd {
        bail!(
            "refusing to trash current workspace root: {}",
            path.display()
        )
    }
    if let Some(home) = home {
        if path == home {
            bail!("refusing to trash home directory: {}", path.display())
        }
        let trash_dir = home.join(".local/share/Trash");
        if path == trash_dir || path.starts_with(&trash_dir) {
            bail!(
                "refusing to trash the Trash directory itself: {}",
                path.display()
            )
        }
    }
    Ok(())
}

fn path_kind(metadata: &std::fs::Metadata) -> &'static str {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_dir() {
        "directory"
    } else if file_type.is_file() {
        "file"
    } else {
        "other"
    }
}

fn max_results(args: &Value) -> usize {
    args.get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 500) as usize
}

fn path_arg(args: &Value, key: &str) -> Result<PathBuf> {
    let value = required(args, key)?;
    Ok(expand_path(&value))
}

fn optional_path(args: &Value) -> Option<PathBuf> {
    args.get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(expand_path)
}

fn expand_path(value: &str) -> PathBuf {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
            return home.join(rest);
        }
    }
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        super::workspace::effective_workdir().join(path)
    }
}

fn required(args: &Value, key: &str) -> Result<String> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        bail!("{}: {key}", t("required argument missing", "缺少必需参数"))
    } else {
        Ok(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ripgrep_available() -> bool {
        std::process::Command::new("rg")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn fake_trash(args: Value) -> Result<String> {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        trash_paths_with(
            args,
            &ToolProgress::new(tx),
            |path| {
                if std::fs::symlink_metadata(path)?.file_type().is_dir() {
                    std::fs::remove_dir_all(path)?;
                } else {
                    std::fs::remove_file(path)?;
                }
                Ok(())
            },
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_execution_streams_stdout_and_stderr() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let output = execute_command("printf 'out'; printf 'err' >&2", 5, ToolProgress::new(tx))
            .await
            .unwrap();
        let output: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(output["stdout"], "out");
        assert_eq!(output["stderr"], "err");

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let crate::tools::ToolProgressEvent::CommandOutput { stream, chunk } = event {
                match stream {
                    CommandOutputStream::Stdout => stdout.extend(chunk),
                    CommandOutputStream::Stderr => stderr.extend(chunk),
                }
            }
        }
        assert_eq!(stdout, b"out");
        assert_eq!(stderr, b"err");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn command_timeout_kills_descendant_processes() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let result = execute_command("sleep 30 & echo $!; wait", 1, ToolProgress::new(tx)).await;
        assert!(result.is_err());

        let mut stdout = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let crate::tools::ToolProgressEvent::CommandOutput {
                stream: CommandOutputStream::Stdout,
                chunk,
            } = event
            {
                stdout.extend(chunk);
            }
        }
        let pid = String::from_utf8(stdout)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        let mut gone = false;
        for _ in 0..20 {
            if unsafe { libc::kill(pid, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(gone, "descendant process {pid} survived command timeout");
    }

    #[test]
    fn edit_file_replaces_lines() {
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(cwd).unwrap();
        let path = temp.path().join("sample.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let result = edit_file(
            json!({
                "path": path.display().to_string(),
                "start_line": 2,
                "end_line": 2,
                "replacement": "TWO\nTWO-B"
            }),
            ToolProgress::default(),
        );
        let data: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert!(data.get("diff").is_none());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "one\nTWO\nTWO-B\nthree\n"
        );
    }

    #[test]
    fn edit_file_allows_existing_files_outside_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sample.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        edit_file(
            json!({
                "path": path.display().to_string(),
                "start_line": 1,
                "end_line": 2,
                "replacement": "table"
            }),
            ToolProgress::default(),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "table\n");
    }

    #[test]
    fn read_file_paginates_text() {
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(cwd).unwrap();
        let path = temp.path().join("sample.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let result = read_file(json!({
            "path": path.display().to_string(),
            "offset": 2,
            "limit": 1,
        }))
        .unwrap();
        let data: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(data["type"], "text-page");
        assert_eq!(data["content"], "2: two");
        assert_eq!(data["truncated"], true);
        assert_eq!(data["next"], 3);
    }

    #[test]
    fn read_file_rejects_binary() {
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(cwd).unwrap();
        let path = temp.path().join("sample.bin");
        std::fs::write(&path, [0, 1, 2, 3]).unwrap();
        assert!(read_file(json!({"path": path.display().to_string()})).is_err());
    }

    #[tokio::test]
    async fn glob_files_matches_filename_case_insensitively() {
        if !ripgrep_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(cwd).unwrap();
        let path = temp.path().join("ai测试题.txt");
        std::fs::write(&path, "content").unwrap();
        let result = glob_files(json!({
            "path": temp.path().display().to_string(),
            "pattern": "*Ai*测试*",
        }))
        .await
        .unwrap();
        let data: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(data["success"], true);
        assert!(data["stdout"].as_str().unwrap().contains("ai测试题.txt"));
    }

    #[tokio::test]
    async fn grep_no_matches_is_successful_empty_result() {
        if !ripgrep_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(cwd).unwrap();
        std::fs::write(temp.path().join("sample.txt"), "hello").unwrap();
        let result = grep_text(json!({
            "path": temp.path().display().to_string(),
            "pattern": "definitely-not-present",
        }))
        .await
        .unwrap();
        let data: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(data["success"], true);
        assert_eq!(data["exit_code"], 0);
        assert_eq!(data["stdout"], "");
        assert_eq!(data["note"], "no matches");
    }

    #[test]
    fn root_search_uses_protective_excludes() {
        let root = Path::new("/");
        assert!(prepare_search_path(root).is_ok());
        let args = search_exclude_args(root).join(" ");
        assert!(args.contains("--glob=!proc/**"));
        assert!(args.contains("--glob=!usr/**"));
    }

    #[test]
    fn trash_path_rejects_workspace_root() {
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert!(ensure_safe_trash_target(&cwd).is_err());
    }

    #[test]
    fn trash_moves_files_and_directories_in_one_call() {
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(cwd).unwrap();
        let file = temp.path().join("trash-me.txt");
        std::fs::write(&file, "bye").unwrap();
        let dir = temp.path().join("trash-dir");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("child.txt"), "bye").unwrap();

        let result = fake_trash(json!({"paths": [
            file.display().to_string(),
            dir.display().to_string(),
        ]}))
        .unwrap();
        let data: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(data["ok"], true);
        assert_eq!(data["moved"], 2);
        assert_eq!(data["failed"], 0);
        assert_eq!(data["total"], 2);
        assert!(!file.exists());
        assert!(!dir.exists());
        // 提示语整次一份,不再逐条重复——这是返回体积的大头。
        assert!(data["note"].is_string());
        assert_eq!(data["failures"].as_array().unwrap().len(), 0);
    }

    /// 一条失败不该带走整批:因为第 2 条不存在就放弃第 3 条,只会让模型再发一轮。
    #[test]
    fn trash_reports_each_failure_and_keeps_going() {
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(cwd).unwrap();
        let first = temp.path().join("a.txt");
        let last = temp.path().join("b.txt");
        std::fs::write(&first, "a").unwrap();
        std::fs::write(&last, "b").unwrap();
        let missing = temp.path().join("nope.txt");

        let result = fake_trash(json!({"paths": [
            first.display().to_string(),
            missing.display().to_string(),
            last.display().to_string(),
        ]}))
        .unwrap();
        let data: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(data["moved"], 2);
        assert_eq!(data["failed"], 1);
        assert_eq!(data["ok"], true, "还有成功的就不算整体失败");
        assert!(!first.exists());
        assert!(!last.exists(), "失败项之后的路径仍要处理");
        let failures = data["failures"].as_array().unwrap();
        assert_eq!(failures.len(), 1);
        assert!(failures[0]["path"]
            .as_str()
            .unwrap()
            .contains("nope.txt"));
        assert!(!failures[0]["error"].as_str().unwrap().is_empty());
    }

    #[test]
    fn trash_rejects_an_empty_or_missing_path_list() {
        assert!(fake_trash(json!({"paths": []})).is_err());
        assert!(fake_trash(json!({"paths": ["", "   "]})).is_err());
        assert!(fake_trash(json!({"path": "/tmp/x"})).is_err());
    }
}
