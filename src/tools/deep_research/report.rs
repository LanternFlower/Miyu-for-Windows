//! 引用登记与最终报告。
//!
//! 引用编号必须和正文里的标记一一对应（`extract_markers` 对账）：模型可能引用
//! 一个不存在的编号，也可能登记了却没引用。两种都要报出来
//! （`reference_diagnostics`），而不是静默修好——静默修意味着报告里的引用可能
//! 指向别的来源。

use crate::tools::deep_research::*;

#[derive(Default)]
pub(in crate::tools::deep_research) struct ReferenceCounters {
    pub(crate) record: usize,
    pub(crate) knowledge: usize,
    pub(crate) web: usize,
}

#[derive(Clone)]
pub(in crate::tools::deep_research) struct Reference {
    pub(crate) marker: String,
    pub(crate) kind: String,
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) path: String,
    pub(crate) snippet: String,
}

pub(in crate::tools::deep_research) fn register_reference_tools(registry: &mut ToolRegistry, state: Arc<Mutex<ResearchState>>) {
    let title_state = Arc::clone(&state);
    registry.register(ToolSpec::new(
        "register_deep_research_topic_title",
        "Register a concise title for this deep research task.",
        json!({"type":"object","properties":{"topic_title":{"type":"string"},"reason":{"type":"string"}},"required":["topic_title"],"additionalProperties":false}),
        move |args| {
            let title_state = Arc::clone(&title_state);
            async move {
                let title = args.get("topic_title").and_then(Value::as_str).unwrap_or_default();
                let title = sanitize_title(title, 40);
                let mut state = title_state.lock().expect("deep research state lock");
                state.topic_title = title.clone();
                Ok(json!({"ok": true, "topic_title": title}).to_string())
            }
        },
    ));
    let ref_state = Arc::clone(&state);
    registry.register(ToolSpec::new(
        "register_deep_research_reference",
        "Register a source and receive a stable citation marker such as [W1].",
        json!({"type":"object","properties":{"reference_type":{"type":"string","enum":["R","K","W","record","knowledge","web"]},"title":{"type":"string"},"url":{"type":"string"},"path":{"type":"string"},"snippet":{"type":"string"}},"required":["reference_type","title"],"additionalProperties":false}),
        move |args| {
            let ref_state = Arc::clone(&ref_state);
            async move {
                let kind = normalized_reference_kind(args.get("reference_type").and_then(Value::as_str).unwrap_or("W"));
                let title = args.get("title").and_then(Value::as_str).unwrap_or("Untitled").trim().to_string();
                let url = args.get("url").and_then(Value::as_str).unwrap_or_default().trim().to_string();
                let path = args.get("path").and_then(Value::as_str).unwrap_or_default().trim().to_string();
                let snippet = args.get("snippet").and_then(Value::as_str).unwrap_or_default().trim().to_string();
                let mut state = ref_state.lock().expect("deep research state lock");
                let number = match kind.as_str() {
                    "R" => { state.counters.record += 1; state.counters.record }
                    "K" => { state.counters.knowledge += 1; state.counters.knowledge }
                    _ => { state.counters.web += 1; state.counters.web }
                };
                let marker = format!("{kind}{number}");
                state.references.push(Reference { marker: marker.clone(), kind, title, url, path, snippet });
                Ok(json!({"ok": true, "ref": marker, "citation": format!("[{marker}]")}).to_string())
            }
        },
    ));
    registry.register(ToolSpec::new(
        "remove_deep_research_reference",
        "Remove a registered source by marker.",
        json!({"type":"object","properties":{"ref":{"type":"string"},"reason":{"type":"string"}},"required":["ref"],"additionalProperties":false}),
        move |args| {
            let state = Arc::clone(&state);
            async move {
                let marker = args.get("ref").and_then(Value::as_str).unwrap_or_default().trim().trim_matches(&['[', ']'][..]).to_string();
                let mut state = state.lock().expect("deep research state lock");
                let old_len = state.references.len();
                state.references.retain(|item| item.marker != marker);
                Ok(json!({"ok": old_len != state.references.len(), "ref": marker}).to_string())
            }
        },
    ));
}

pub(in crate::tools::deep_research) fn reference_registry_json(state: &Arc<Mutex<ResearchState>>) -> Result<String> {
    let state = state.lock().expect("deep research state lock");
    let refs = state.references.iter().map(|item| json!({"ref": item.marker, "type": item.kind, "title": item.title, "url": item.url, "path": item.path, "snippet": item.snippet})).collect::<Vec<_>>();
    Ok(serde_json::to_string_pretty(&refs)?)
}

pub(in crate::tools::deep_research) fn normalize_final_answer(draft: &str, state: &Arc<Mutex<ResearchState>>) -> Result<String> {
    let diagnostics = reference_diagnostics(draft, state);
    let mut answer = strip_reference_section(draft).trim().to_string();
    if !diagnostics.is_empty() {
        answer.push_str("\n\n## 引用校验提示\n");
        for item in diagnostics {
            answer.push_str(&format!("- {item}\n"));
        }
    }
    answer.push_str("\n\n## 参考资料\n");
    let state = state.lock().expect("deep research state lock");
    if state.references.is_empty() {
        answer.push_str("- 本次研究没有注册外部参考资料。\n");
    } else {
        for item in &state.references {
            let source = if !item.url.is_empty() {
                format!("[{}]({})", item.title, item.url)
            } else if !item.path.is_empty() {
                format!("{} ({})", item.title, item.path)
            } else {
                item.title.clone()
            };
            answer.push_str(&format!("- [{}] {}\n", item.marker, source));
        }
    }
    Ok(answer)
}

pub(in crate::tools::deep_research) fn reference_diagnostics(draft: &str, state: &Arc<Mutex<ResearchState>>) -> Vec<String> {
    let state = state.lock().expect("deep research state lock");
    let known = state
        .references
        .iter()
        .map(|item| item.marker.as_str())
        .collect::<Vec<_>>();
    let mut diagnostics = Vec::new();
    for marker in extract_markers(draft) {
        if !known.iter().any(|item| *item == marker) {
            diagnostics.push(format!("正文引用了未注册来源 [{marker}]。"));
        }
    }
    if draft.contains("http://") || draft.contains("https://") {
        diagnostics.push("正文中存在裸 URL；建议注册为 W 类型参考资料后使用编号引用。".to_string());
    }
    diagnostics
}

pub(in crate::tools::deep_research) fn extract_markers(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in value.split('[').skip(1) {
        let Some(end) = part.find(']') else { continue };
        let marker = &part[..end];
        if marker.len() >= 2
            && matches!(marker.as_bytes()[0], b'R' | b'K' | b'W')
            && marker[1..].chars().all(|ch| ch.is_ascii_digit())
        {
            out.push(marker.to_string());
        }
    }
    out
}

pub(in crate::tools::deep_research) fn strip_reference_section(value: &str) -> String {
    for heading in ["\n## 参考资料", "\n# 参考资料"] {
        if let Some(index) = value.find(heading) {
            return value[..index].to_string();
        }
    }
    value.to_string()
}

pub(in crate::tools::deep_research) fn write_report(
    plugin: &DeepResearchPluginConfig,
    paths: &MiyuPaths,
    topic: &str,
    final_answer: &str,
    state: &Arc<Mutex<ResearchState>>,
    stop_reason: &str,
    iterations: usize,
    state_for_stats: &Arc<Mutex<ResearchState>>,
) -> Result<PathBuf> {
    let output_dir = expand_output_dir(&plugin.output_dir, paths);
    std::fs::create_dir_all(&output_dir)?;
    let title = topic_title(state, topic);
    let filename = unique_report_filename(&output_dir, &title);
    let path = output_dir.join(filename);
    let stats = public_stats(state_for_stats);
    let report = format!(
        "---\ntopic: {}\ntopic_title: {}\ncreated_at: {}\nstop_reason: {}\niterations_used: {}\ntool_calls: {}\ntool_ok: {}\ntool_errors: {}\ntoken_estimate: {}\ntoken_estimate_method: {}\ntoken_estimate_is_actual: {}\n---\n\n{}\n",
        topic,
        title,
        Local::now().to_rfc3339(),
        stop_reason,
        iterations,
        stats["tool_calls"].as_u64().unwrap_or(0),
        stats["tool_ok"].as_u64().unwrap_or(0),
        stats["tool_errors"].as_u64().unwrap_or(0),
        stats["token_estimate"].as_u64().unwrap_or(0),
        stats["token_estimate_method"].as_str().unwrap_or("rough_char_estimate"),
        stats["token_estimate_is_actual"].as_bool().unwrap_or(false),
        final_answer.trim_end()
    );
    std::fs::write(&path, report)?;
    Ok(path)
}

pub(in crate::tools::deep_research) fn public_sources(state: &Arc<Mutex<ResearchState>>) -> Vec<Value> {
    let state = state.lock().expect("deep research state lock");
    state.references.iter().map(|item| json!({"ref": item.marker, "type": item.kind, "title": item.title, "url": item.url, "path": item.path})).collect()
}
