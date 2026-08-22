use super::{ToolRegistry, ToolSpec};
use crate::config::AppConfig;
use crate::memory::{MemoryAccess, MemoryStore};
use crate::paths::MiyuPaths;
use anyhow::{bail, Result};
use serde_json::{json, Value};

pub fn register(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    register_with_context(
        registry,
        config,
        paths,
        MemoryAccess::Privileged,
        None,
        String::new(),
    );
}

pub(crate) fn register_with_context(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    access: MemoryAccess,
    writer_principal: Option<String>,
    writer_display_name: String,
) {
    if !config.memory_config().enabled {
        return;
    }
    register_readonly_with_context(
        registry,
        config.clone(),
        paths.clone(),
        access.clone(),
        writer_principal.clone(),
        writer_display_name.clone(),
    );
    registry.register(ToolSpec::new(
        "remember_fact",
        "Save a durable memory fact or useful knowledge point for future association. Use only for reusable facts, preferences, methods, or stable discoveries.",
        json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "The concise fact or knowledge point to remember." },
                "source": { "type": "string", "description": "Optional source label." }
            },
            "required": ["content"],
            "additionalProperties": false
        }),
        {
            let config = config.clone();
            let paths = paths.clone();
            let access = access.clone();
            let writer_principal = writer_principal.clone();
            let writer_display_name = writer_display_name.clone();
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                let access = access.clone();
                let writer_principal = writer_principal.clone();
                let writer_display_name = writer_display_name.clone();
                async move {
                    remember_fact(
                        args,
                        config,
                        paths,
                        access,
                        writer_principal,
                        writer_display_name,
                    )
                    .await
                }
            }
        },
    ).writes());
}

pub fn register_readonly(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    register_readonly_with_context(
        registry,
        config,
        paths,
        MemoryAccess::Privileged,
        None,
        String::new(),
    );
}

pub(crate) fn register_readonly_with_context(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    access: MemoryAccess,
    writer_principal: Option<String>,
    writer_display_name: String,
) {
    if !config.memory_config().enabled {
        return;
    }
    // 不再按 on_overflow 门控:compact 体制下 CLI `miyu pop` 照样把弹出
    // 轮归档进外溢库,没有这把工具模型就永远找不回它们(验收三轮:pop
    // 去留之争的答案是"补检索",不是删命令)。
    if config.memory_config().evicted_context_enabled {
        registry.register(ToolSpec::new(
            "search_evicted_context",
            "Search conversation turns that were moved out of the active context window. Use this when the current context appears to be missing earlier discussion.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search keywords or question." },
                    "max_results": { "type": "integer", "description": "Optional result limit." },
                    "start_time": { "type": "string", "description": "Optional lower bound: RFC 3339, YYYY-MM-DD, or YYYY-MM-DD HH:MM[:SS]." },
                    "end_time": { "type": "string", "description": "Optional upper bound, same formats; a bare date covers that whole day." }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            {
                let config = config.clone();
                let paths = paths.clone();
                let access = access.clone();
                let writer_principal = writer_principal.clone();
                let writer_display_name = writer_display_name.clone();
                move |args| {
                    let config = config.clone();
                    let paths = paths.clone();
                    let access = access.clone();
                    let writer_principal = writer_principal.clone();
                    let writer_display_name = writer_display_name.clone();
                    async move {
                        search_evicted_context(
                            args,
                            config,
                            paths,
                            access,
                            writer_principal,
                            writer_display_name,
                        )
                        .await
                    }
                }
            },
        ));
    }
    // recall_past_events 被 recall_memories 严格包含(前者只搜日记,后者搜
    // 知识点+日记),合并成一件 `recall_memories` 加 scope 参数(08-17)。
    registry.register(ToolSpec::new(
        "recall_memories",
        "Search remembered facts and past events, including forgotten memories when requested. scope=episode narrows the search to the diary-like record of things that happened in previous conversations. This read-only tool does not change memory state.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search keywords or question. Omit only when passing id." },
                "id": { "type": "integer", "description": "Fetch one memory in full by its id, as printed in a truncated associative-memory entry." },
                "scope": { "type": "string", "enum": ["all", "episode"], "description": "all searches facts and events (default); episode searches only past events." },
                "max_results": { "type": "integer", "description": "Optional result limit." },
                "include_forgotten": { "type": "boolean", "description": "Whether to include forgotten memories." }
            },
            "additionalProperties": false
        }),
        {
            let config = config.clone();
            let paths = paths.clone();
            let access = access.clone();
            let writer_principal = writer_principal.clone();
            let writer_display_name = writer_display_name.clone();
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                let access = access.clone();
                let writer_principal = writer_principal.clone();
                let writer_display_name = writer_display_name.clone();
                async move {
                    if args.get("scope").and_then(Value::as_str) == Some("episode")
                        && args.get("id").is_none()
                    {
                        recall_past_events(
                            args,
                            config,
                            paths,
                            access,
                            writer_principal,
                            writer_display_name,
                        )
                        .await
                    } else {
                        recall_memories(
                            args,
                            config,
                            paths,
                            access,
                            writer_principal,
                            writer_display_name,
                        )
                        .await
                    }
                }
            }
        },
    ));
}

/// Records store RFC 3339 timestamps; the model may write a bare date or a
/// local wall-clock time. `end_of_day` makes a bare end date cover that day.
fn optional_time_bound(args: &Value, key: &str, end_of_day: bool) -> Result<Option<String>> {
    let Some(raw) = args.get(key).and_then(Value::as_str) else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    // Bounds are compared against a TEXT column, so both sides have to be in
    // the same zone or the comparison is lexicographic nonsense: a local
    // midnight in +09:00 sorts after a UTC instant that actually came later.
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(Some(to_utc_rfc3339(parsed.with_timezone(&chrono::Utc))));
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(value) = chrono::NaiveDateTime::parse_from_str(raw, format) {
            return Ok(Some(local_rfc3339(value)));
        }
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let time = if end_of_day {
            date.and_hms_opt(23, 59, 59)
        } else {
            date.and_hms_opt(0, 0, 0)
        }
        .ok_or_else(|| anyhow::anyhow!("date is outside the supported range"))?;
        return Ok(Some(local_rfc3339(time)));
    }
    anyhow::bail!("invalid {key} {raw:?}; use RFC 3339, YYYY-MM-DD, or YYYY-MM-DD HH:MM[:SS]")
}

fn local_rfc3339(value: chrono::NaiveDateTime) -> String {
    use chrono::TimeZone;
    chrono::Local
        .from_local_datetime(&value)
        .earliest()
        .map(|value| to_utc_rfc3339(value.with_timezone(&chrono::Utc)))
        .unwrap_or_else(|| to_utc_rfc3339(chrono::Utc.from_utc_datetime(&value)))
}

fn to_utc_rfc3339(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

async fn search_evicted_context(
    args: Value,
    config: AppConfig,
    paths: MiyuPaths,
    access: MemoryAccess,
    writer_principal: Option<String>,
    writer_display_name: String,
) -> Result<String> {
    let query = required_str(&args, "query")?;
    let limit = optional_limit(&args);
    let start = optional_time_bound(&args, "start_time", false)?;
    let end = optional_time_bound(&args, "end_time", true)?;
    let store = MemoryStore::new(&config, &paths).with_request_context(
        access,
        writer_principal,
        writer_display_name,
    );
    Ok(store
        .search_evicted_context_hybrid(query, limit, start.as_deref(), end.as_deref())
        .await?
        .to_string())
}

async fn recall_past_events(
    args: Value,
    config: AppConfig,
    paths: MiyuPaths,
    access: MemoryAccess,
    writer_principal: Option<String>,
    writer_display_name: String,
) -> Result<String> {
    let query = required_str(&args, "query")?;
    let limit = optional_limit(&args);
    let store = MemoryStore::new(&config, &paths).with_request_context(
        access,
        writer_principal,
        writer_display_name,
    );
    Ok(store.recall_past_events_readonly(query, limit)?.to_string())
}

async fn remember_fact(
    args: Value,
    config: AppConfig,
    paths: MiyuPaths,
    access: MemoryAccess,
    writer_principal: Option<String>,
    writer_display_name: String,
) -> Result<String> {
    let content = required_str(&args, "content")?;
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("conversation");
    let store = MemoryStore::new(&config, &paths).with_request_context(
        access,
        writer_principal,
        writer_display_name,
    );
    let id = store.remember_fact(content, source)?;
    Ok(json!({
        "ok": true,
        "id": id,
        "source": source.trim(),
        "content": content.trim(),
        "message": "Memory saved. The saved content is included here so the current conversation can refer to it accurately."
    })
    .to_string())
}

async fn recall_memories(
    args: Value,
    config: AppConfig,
    paths: MiyuPaths,
    access: MemoryAccess,
    writer_principal: Option<String>,
    writer_display_name: String,
) -> Result<String> {
    // id 直取:联想块里被截断的日记条目带着 `recall_memories id=<id>`,
    // 这是那条提示的落点。给了 id 就不需要 query。
    if let Some(id) = args.get("id").and_then(Value::as_i64) {
        let store = MemoryStore::new(&config, &paths).with_request_context(
            access,
            writer_principal,
            writer_display_name,
        );
        return Ok(store.recall_by_id_readonly(id)?.to_string());
    }
    let query = required_str(&args, "query")?;
    let limit = optional_limit(&args);
    let include_forgotten = args
        .get("include_forgotten")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let store = MemoryStore::new(&config, &paths).with_request_context(
        access,
        writer_principal,
        writer_display_name,
    );
    Ok(store
        .recall_memories_readonly(query, limit, include_forgotten)?
        .to_string())
}

fn required_str<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    let value = args
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        bail!("{}: {name}", "required argument missing");
    }
    Ok(value)
}

fn optional_limit(args: &Value) -> usize {
    args.get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 50) as usize
}

#[cfg(test)]
mod tests {

    #[test]
    fn time_bounds_are_normalized_to_utc_before_they_hit_the_text_column() {
        // The comparison is lexicographic against stored RFC 3339 text, so a
        // bound carrying a local offset compares as nonsense: midnight in
        // +09:00 sorts after a UTC instant that actually came later.
        let args = serde_json::json!({
            "start_time": "2026-08-06T10:00:00+09:00",
            "end_time": "2026-08-06"
        });
        let start = optional_time_bound(&args, "start_time", false).unwrap().unwrap();
        assert!(start.ends_with("+00:00"), "{start}");
        assert!(start.starts_with("2026-08-06T01:00:00"), "{start}");

        let end = optional_time_bound(&args, "end_time", true).unwrap().unwrap();
        assert!(end.ends_with("+00:00"), "{end}");

        // Absent and blank both mean "no bound".
        assert!(optional_time_bound(&args, "missing", false).unwrap().is_none());
        let blank = serde_json::json!({ "start_time": "   " });
        assert!(optional_time_bound(&blank, "start_time", false).unwrap().is_none());
        // Garbage is refused rather than silently ignored.
        let bad = serde_json::json!({ "start_time": "上周三" });
        assert!(optional_time_bound(&bad, "start_time", false).is_err());
    }
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn test_paths() -> MiyuPaths {
        let root = PathBuf::from("/tmp/miyu-memory-tool-test");
        MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("fish-hook.fish"),
            bash_hook_file: root.join("bash-hook.sh"),
            zsh_hook_file: root.join("zsh-hook.zsh"),
            scripts_dir: root.join("config/scripts"),
            system_scripts_dir: PathBuf::new(),
        }
    }

    /// 断言的是"注册了",不是"常驻在 tools 数组里"——search_evicted_context
    /// 08-17 起按需加载(冷门工具降级),仍然必须注册。
    fn tool_names(registry: &ToolRegistry) -> BTreeSet<String> {
        registry.tool_names().into_iter().collect()
    }

    #[test]
    fn search_evicted_context_registers_whenever_archiving_is_enabled() {
        // compact 体制下 CLI pop 也会归档,检索工具必须在(验收三轮)。
        let paths = test_paths();
        let compact_config = AppConfig::default();
        assert_eq!(compact_config.context.on_overflow, "compact");
        let mut compact_registry = ToolRegistry::new();
        register_readonly(&mut compact_registry, compact_config, paths.clone());
        assert!(tool_names(&compact_registry).contains("search_evicted_context"));

        let mut disabled_config = AppConfig::default();
        disabled_config.memory.evicted_context_enabled = false;
        let mut disabled_registry = ToolRegistry::new();
        register_readonly(&mut disabled_registry, disabled_config, paths);
        assert!(!tool_names(&disabled_registry).contains("search_evicted_context"));
    }
}
