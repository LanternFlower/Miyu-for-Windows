//! 工具调用的可读摘要。
//!
//! 一行摘要要说清「在对什么做什么」,所以 `tool_subject` 按工具类型挑出最有信息量
//! 的那个参数(读文件挑路径、搜索挑关键词);挑不出来时 `tool_peek` 退回命令文本、
//! 再退回把参数里的标量值串起来——**绝不**原样甩 JSON。
//!
//! `redact_sensitive_inline` / `redact_bearer_token` 是必需的:工具参数里可能带
//! token 或密钥,而终端内容会被截图、会进日志。
//!
//! 这里的判断全是「哪件工具的哪个参数最有信息量」,是工具层的事实;渲染层、WebUI、
//! 子代理进度都只是消费者。原先住在 `render/tool_display.rs`,于是工具层要用就得
//! 反过来 use 渲染层(09-16 归位)。`render/tool_display.rs` 与 `render/stream/timeline.rs`
//! 各留了再导出,`render::tool_subject` 这类老路径一字未改。

use super::{is_command_tool, readable_tool_name, tool_event_base_name};
use miyu_base::i18n::text as t;
use miyu_base::terminal::{clip_progress_line, sanitize_terminal_text};
use serde_json::Value;

/// 一步的窥视：先按工具自己的规矩摘主题（命令、路径、检索词），命令工具退回
/// 命令文本，都摘不出来就把参数里的值串起来——**绝不**原样甩 JSON。
///
/// `{"action": "info", "package_name": "zzq"}` 这种在面板里读起来是一团括号引号
/// （用户实测：子代理浮层的参数窥视是裸 JSON）；值串成 `info · zzq` 才是人话。
pub fn tool_peek(name: &str, arguments: &str) -> Option<String> {
    if let Some(subject) = tool_subject(name, arguments) {
        return Some(subject);
    }
    if is_command_tool(tool_event_base_name(name)) {
        if let Some(command) = command_peek(arguments) {
            return Some(command);
        }
    }
    args_peek(arguments)
}

/// 参数对象里的标量值按出现顺序串起来，`·` 隔开。数组、嵌套对象跳过；空的
/// 就是没有。单个值裁到 48 列，总长交给调用方再裁。
pub fn args_peek(arguments: &str) -> Option<String> {
    let arguments = arguments.trim();
    let args = serde_json::from_str::<Value>(arguments).ok()?;
    let object = args.as_object()?;
    // `serde_json` 的对象是按键名排序的；按模型写出来的次序串才读得顺
    //（`info · zzq` 而不是 `zzq · info`），所以按键在原文里出现的位置排。
    let mut entries: Vec<(usize, &Value)> = object
        .iter()
        .map(|(key, value)| {
            let at = arguments.find(&format!("\"{key}\"")).unwrap_or(usize::MAX);
            (at, value)
        })
        .collect();
    entries.sort_by_key(|(at, _)| *at);
    let mut parts: Vec<String> = Vec::new();
    for (_, value) in entries {
        let text = match value {
            Value::String(text) => text.split_whitespace().collect::<Vec<_>>().join(" "),
            Value::Number(number) => number.to_string(),
            Value::Bool(flag) => flag.to_string(),
            _ => continue,
        };
        if text.is_empty() {
            continue;
        }
        parts.push(miyu_base::terminal::clip_to_display_width(&text, 48));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

pub fn tool_subject(name: &str, arguments: &str) -> Option<String> {
    let args = serde_json::from_str::<Value>(arguments).ok()?;
    let name = tool_event_base_name(name);
    let value = match name {
        // —— claude 原生工具(中转侧闭环执行,REPL 摘要行同样要有 ↳ 主题) ——
        "Bash" => {
            let command = string_arg(&args, &["command"])?;
            Some(
                if args.get("run_in_background").and_then(Value::as_bool) == Some(true) {
                    format!("[后台] {command}")
                } else {
                    command
                },
            )
        }
        "Read" | "Edit" | "Write" | "NotebookEdit" => {
            string_arg(&args, &["file_path", "path", "notebook_path"])
        }
        "WebFetch" => string_arg(&args, &["url"]).and_then(|url| safe_url_subject(&url)),
        "WebSearch" | "ToolSearch" => string_arg(&args, &["query"]),
        "Task" | "Agent" => string_arg(&args, &["description"]),
        "SlashCommand" => string_arg(&args, &["command"]),
        // —— agy 原生工具(antigravity 中转;入参键已在流层归一成 Miyu 的) ——
        "view_file" | "write_to_file" | "replace_file_content" | "list_dir" => {
            string_arg(&args, &["path"])
        }
        "find_by_name" | "grep_search" => {
            let needle = string_arg(&args, &["pattern", "query"])?;
            Some(match string_arg(&args, &["path"]) {
                Some(path) => format!("{needle} · {path}"),
                None => needle,
            })
        }
        "read_url_content" => string_arg(&args, &["url"]).and_then(|url| safe_url_subject(&url)),
        "search_web" => string_arg(&args, &["query"]),
        "call_mcp_tool" => string_arg(&args, &["ToolName"]),
        "subagent" | "task" => string_arg(&args, &["description"]),
        "web_search"
        | "search_web_images"
        | "use_meme"
        | "search_knowledge_base"
        | "search_evicted_context"
        | "recall_memories"
        | "aur"
        | "online_man"
        | "game_compat"
        | "fcitx5_input_method_wiki_qurey" => string_arg(&args, &["query", "topic"]),
        "archwiki_query" | "query_moegirl" => string_arg(&args, &["title", "query"]),
        "read" | "read_file" => {
            let path = string_arg(&args, &["path"])?;
            Some(match read_page_label(&args) {
                Some(page) => format!("{path} ({page})"),
                None => path,
            })
        }
        // 补丁三件套:从补丁头里抠文件名当副标题,不然工具行只剩一个名字,
        // 用户分不清这回编辑的是什么(08-22 验收反馈)。
        "edit" | "kb" | "artifact" | "apply_patch" | "apply_artifact_patch" => {
            let patch = string_arg(&args, &["patchText", "patch_text"])?;
            let files: Vec<String> = patch
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("*** Add File: ")
                        .or_else(|| line.strip_prefix("*** Update File: "))
                        .or_else(|| line.strip_prefix("*** Delete File: "))
                })
                .map(|name| name.trim().to_string())
                .collect();
            match files.as_slice() {
                [] => None,
                [only] => Some(only.clone()),
                [first, ..] => Some(format!("{first} +{} {}", files.len() - 1, t("more", "项"))),
            }
        }
        "write_file" | "edit_file" | "edit_string" | "manage_script" => {
            string_arg(&args, &["path"])
        }
        "trash_path" => {
            let paths = args.get("paths").and_then(Value::as_array)?;
            match paths.len() {
                0 => None,
                // 只删一个时报路径更有用;成堆删时路径无信息量,报条数。
                1 => paths[0].as_str().map(str::to_string),
                count => Some(format!("{count} {}", t("items", "项"))),
            }
        }
        "run_command" => {
            let command = string_arg(&args, &["command"])?;
            Some(
                if args.get("background").and_then(Value::as_bool) == Some(true) {
                    format!("[后台] {command}")
                } else {
                    command
                },
            )
        }
        "read_knowledge_base_file" | "edit_knowledge_base_file" | "remove_knowledge_base_file" => {
            string_arg(&args, &["file_name"])
        }
        "glob" | "grep" | "Glob" | "Grep" => {
            let pattern = string_arg(&args, &["pattern"]);
            let path = string_arg(&args, &["path"]);
            match (pattern, path) {
                (Some(pattern), Some(path)) if !path.trim().is_empty() => {
                    Some(format!("{pattern} · {path}"))
                }
                (pattern, _) => pattern,
            }
        }
        "web_fetch" => string_arg(&args, &["url"]).and_then(|url| safe_url_subject(&url)),
        "load_skill" => string_arg(&args, &["name"]),
        "manage_skill" => string_arg(&args, &["name", "draft_id"]),
        "load_tools" => args.get("names").and_then(Value::as_array).map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(|name| {
                    let display = readable_tool_name(&format!("load_tools:{name}"));
                    display
                        .split_once('：')
                        .or_else(|| display.split_once(": "))
                        .map(|(_, target)| target.to_string())
                        .unwrap_or(display)
                })
                .collect::<Vec<_>>()
                .join(t(", ", "、"))
        }),
        "get_weather" => string_arg(&args, &["location"])
            .or_else(|| Some(t("missing location", "缺少地点").to_string())),
        "get_exchange_rate" => {
            let base = string_arg(&args, &["base"])?;
            let target = string_arg(&args, &["target"])?;
            Some(format!(
                "{} → {}",
                base.to_uppercase(),
                target.to_uppercase()
            ))
        }
        "scientific_calculator" => string_arg(&args, &["expression", "operation"]),
        "alarm" => string_arg(&args, &["label", "time", "id"]),
        "archlinux_official_package_query" | "review_aur_package" | "install_aur_package" => {
            string_arg(&args, &["package_name", "package"])
        }
        "vision_analyze" | "print_image" | "manage_meme" => {
            string_arg(&args, &["image"]).map(|image| image_basename(&image))
        }
        "generate_image" => string_arg(&args, &["prompt"]),
        "upload_text_to_knowledge_base" => string_arg(&args, &["file_name", "title"]),
        _ => None,
    }?;
    safe_inline_subject(&value)
}

/// Page label for a read_file call: `L<start>-<end>` when the range is
/// bounded, `L<start>+` for an open tail. `None` for a plain full read so
/// the common case stays a bare path.
pub(crate) fn read_page_label(args: &Value) -> Option<String> {
    let offset = args.get("offset").and_then(Value::as_u64);
    let limit = args.get("limit").and_then(Value::as_u64);
    let start = offset.unwrap_or(1).max(1);
    match (offset, limit) {
        (None, None) => None,
        (_, Some(limit)) => Some(format!(
            "L{start}-{}",
            start.saturating_add(limit.saturating_sub(1))
        )),
        (Some(_), None) => Some(format!("L{start}+")),
    }
}

pub(crate) fn string_arg(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn safe_inline_subject(value: &str) -> Option<String> {
    let value = truncate_inline_input(&sanitize_terminal_text(value), 256);
    let value = clip_progress_line(&value, 256);
    let value = redact_sensitive_inline(&value);
    let value = clip_progress_line(&value, 80);
    (!value.is_empty()).then_some(value)
}

pub(crate) fn truncate_inline_input(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub fn redact_sensitive_inline(value: &str) -> String {
    const KEYS: &[&str] = &[
        "secret_access_key",
        "secret-access-key",
        "access_key_id",
        "access-key-id",
        "api_key",
        "api-key",
        "apikey",
        "token",
        "password",
        "passwd",
        "secret",
        "authorization",
        "cookie",
        "credential",
        "private_key",
        "private-key",
    ];
    let mut output = value.to_string();
    for key in KEYS {
        let mut from = 0usize;
        loop {
            let lower = output.to_ascii_lowercase();
            let Some(relative) = lower[from..].find(key) else {
                break;
            };
            let key_start = from + relative;
            let key_end = key_start + key.len();
            let boundary_ok =
                key_start == 0 || !lower.as_bytes()[key_start - 1].is_ascii_alphanumeric();
            let mut separator = key_end;
            if matches!(lower.as_bytes().get(separator), Some(b'\'' | b'"')) {
                separator += 1;
            }
            let mut had_space = false;
            while lower.as_bytes().get(separator) == Some(&b' ') {
                had_space = true;
                separator += 1;
            }
            let flag_prefix = &lower[..key_start];
            let single_dash_flag = flag_prefix.ends_with('-')
                && (key_start == 1 || lower.as_bytes()[key_start - 2].is_ascii_whitespace());
            let flag_space = had_space && (flag_prefix.ends_with("--") || single_dash_flag);
            let space_delimited = had_space
                && (matches!(*key, "authorization" | "password" | "passwd") || flag_space);
            if !boundary_ok
                || (!space_delimited
                    && !matches!(lower.as_bytes().get(separator), Some(b'=' | b':')))
            {
                from = key_end;
                continue;
            }
            let mut value_start = separator + usize::from(!space_delimited);
            while lower.as_bytes().get(value_start) == Some(&b' ') {
                value_start += 1;
            }
            let quote = lower
                .as_bytes()
                .get(value_start)
                .copied()
                .filter(|value| matches!(value, b'\'' | b'"'));
            value_start += usize::from(quote.is_some());
            let value_end = quote
                .and_then(|quote| {
                    lower.as_bytes()[value_start..]
                        .iter()
                        .position(|value| *value == quote)
                        .map(|end| value_start + end)
                })
                .or_else(|| {
                    flag_space.then(|| {
                        lower.as_bytes()[value_start..]
                            .iter()
                            .position(|byte| byte.is_ascii_whitespace())
                            .map(|end| value_start + end)
                            .unwrap_or(output.len())
                    })
                })
                .or_else(|| {
                    lower[value_start..]
                        .find(['&', ',', ';'])
                        .map(|end| value_start + end)
                })
                .unwrap_or(output.len());
            output.replace_range(value_start..value_end, "[redacted]");
            from = value_start + "[redacted]".len();
        }
    }
    redact_bearer_token(output)
}

pub(crate) fn redact_bearer_token(mut output: String) -> String {
    let mut from = 0usize;
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(relative) = lower[from..].find("bearer") else {
            break;
        };
        let start = from + relative;
        let end = start + "bearer".len();
        let boundary_ok = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
        let mut value_start = end;
        while lower.as_bytes().get(value_start) == Some(&b' ') {
            value_start += 1;
        }
        if !boundary_ok || value_start == end || value_start == output.len() {
            from = end;
            continue;
        }
        let value_end = lower.as_bytes()[value_start..]
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || matches!(*byte, b',' | b';' | b'&'))
            .map(|relative| value_start + relative)
            .unwrap_or(output.len());
        output.replace_range(value_start..value_end, "[redacted]");
        from = value_start + "[redacted]".len();
    }
    output
}

pub(crate) fn safe_url_subject(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value).ok()?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

pub(crate) fn image_basename(value: &str) -> String {
    if let Some(url) = safe_url_subject(value) {
        return url;
    }
    std::path::Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value)
        .to_string()
}

/// 抬头右边那句窥视:命令给的是模型自报的 `title`,不是命令文本——命令就印在
/// 抬头底下。没填就留空(中转线的原生 Bash 没这个参数,天然走空)。
pub fn command_peek(arguments: &str) -> Option<String> {
    let args = serde_json::from_str::<serde_json::Value>(arguments).ok()?;
    let title = args
        .get("title")
        .and_then(serde_json::Value::as_str)?
        .trim();
    (!title.is_empty()).then(|| miyu_base::terminal::clip_to_display_width(title, 72))
}
