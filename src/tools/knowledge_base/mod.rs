mod search;
mod store;
pub(crate) use search::embed_text;
mod files;
mod index;
#[cfg(test)]
use index::keyword_search_blocking;

use search::*;
use store::*;

use super::{ToolRegistry, ToolSpec};
use crate::config::{AppConfig, KnowledgeBasePluginConfig, ProviderConfig};
use crate::paths::MiyuPaths;
use anyhow::{bail, Context, Result};
use chrono::Local;
use reqwest::Client;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;

pub fn register(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    register_readonly(registry, config.clone(), paths.clone());
    if config.plugins.knowledge_base.upload_tool_enabled {
        let upload_config = config.clone();
        let upload_paths = paths.clone();
        registry.register(ToolSpec::new(
                "upload_text_to_knowledge_base",
            "Create a new knowledge-base file or replace an entire existing file. For updating part of an existing file, first search/read it and prefer edit_knowledge_base_file. Never use this for skills, memory, persona, identity, or configuration.",
            json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Text content to save." },
                    "title": { "type": "string", "description": "Optional title used for markdown heading and default file name." },
                    "file_name": { "type": "string", "description": "Optional knowledge base relative path." }
                },
                "required": ["content"],
                "additionalProperties": false
            }),
            move |args| {
                let config = upload_config.clone();
                let paths = upload_paths.clone();
                async move { tool_upload(args, config, paths).await }
            },
        ).writes());
        let edit_config = config.clone();
        let edit_paths = paths.clone();
        registry.register(ToolSpec::new(
            "edit_knowledge_base_file",
            "Edit an existing knowledge-base file by replacing an inclusive 1-based line range. Use after search_knowledge_base/read_knowledge_base_file identifies the exact file and line numbers. This updates metadata and refreshes semantic indexing when embeddings are enabled.",
            json!({
                "type": "object",
                "properties": {
                    "file_name": { "type": "string", "description": "Knowledge base relative path to edit." },
                    "start_line": { "type": "integer", "description": "1-based first line to replace." },
                    "end_line": { "type": "integer", "description": "1-based last line to replace, inclusive." },
                    "replacement": { "type": "string", "description": "Replacement text. May contain multiple lines. Empty text deletes the line range." }
                },
                "required": ["file_name", "start_line", "end_line", "replacement"],
                "additionalProperties": false
            }),
            move |args| {
                let config = edit_config.clone();
                let paths = edit_paths.clone();
                async move { tool_edit(args, config, paths).await }
            },
        ).writes());
        let remove_config = config.clone();
        let remove_paths = paths.clone();
        registry.register(ToolSpec::new(
            "remove_knowledge_base_file",
            "Remove a knowledge-base file by relative path. Use only after the user asks to delete a knowledge-base entry or confirms the exact file. This also removes its metadata and semantic chunks.",
            json!({
                "type": "object",
                "properties": {
                    "file_name": { "type": "string", "description": "Knowledge base relative path to remove." }
                },
                "required": ["file_name"],
                "additionalProperties": false
            }),
            move |args| {
                let config = remove_config.clone();
                let paths = remove_paths.clone();
                async move { tool_remove(args, config, paths).await }
            },
        ).writes());
    }
}

pub fn register_readonly(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    registry.register(ToolSpec::new(
        "search_knowledge_base",
        // 内容检索与文件名检索合并(08-17):同一个知识库的两种检索口径,
        // 拆成两个工具只是让 tools 数组多背一份外壳。by 缺省 content。
        "Search the local knowledge base. by=content (default) searches file contents and returns paths plus original snippets; by=name finds files by file name, directory, extension, or path fragment and returns relative paths. Use read_knowledge_base_file if snippets are insufficient. Mention paths only when useful or when the user asks.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search keywords, user question, or (with by=name) a file name / directory / extension / path fragment." },
                "by": { "type": "string", "enum": ["content", "name"], "description": "content searches text, name searches paths. Defaults to content." },
                "max_results": { "type": "integer", "description": "Optional result limit." }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        {
            let config = config.clone();
            let paths = paths.clone();
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                async move {
                    match args.get("by").and_then(Value::as_str).unwrap_or("content") {
                        "content" => tool_search_readonly(args, config, paths).await,
                        "name" => tool_find_readonly(args, config, paths).await,
                        other => bail!("unknown by: {other}; expected content or name"),
                    }
                }
            }
        },
    ));
    registry.register(ToolSpec::new(
        "read_knowledge_base_file",
        "Read a knowledge base file by relative path with line pagination. Prefer paths returned by search_knowledge_base or search_knowledge_base_by_name. Summarize the relevant content without exposing raw tool JSON.",
        json!({
            "type": "object",
            "properties": {
                "file_name": { "type": "string", "description": "Knowledge base relative path." },
                "start_line": { "type": "integer", "description": "1-based start line." },
                "max_lines": { "type": "integer", "description": "Optional line limit." }
            },
            "required": ["file_name"],
            "additionalProperties": false
        }),
        {
            let config = config.clone();
            let paths = paths.clone();
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                async move { tool_read_readonly(args, config, paths).await }
            }
        },
    ));
}

pub struct KnowledgeBase {
    config: AppConfig,
    root: PathBuf,
    files_dir: PathBuf,
    meta_db: PathBuf,
    semantic_db: PathBuf,
}

impl KnowledgeBase {
    pub fn new(config: AppConfig, paths: MiyuPaths) -> Result<Self> {
        let root = kb_root(&config.plugins.knowledge_base, &paths);
        let files_dir = root.join("files");
        let meta_db = root.join("kb_meta.db");
        let semantic_db = root.join("semantic_index.db");
        Ok(Self {
            config,
            root,
            files_dir,
            meta_db,
            semantic_db,
        })
    }

    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(&self.files_dir)?;
        let conn = self.meta_conn()?;
        init_meta_db(&conn)?;
        let semantic = self.semantic_conn()?;
        init_semantic_db(&semantic)?;
        Ok(())
    }

    fn readonly_available(&self) -> bool {
        self.root.is_dir() && self.files_dir.is_dir() && self.meta_db.is_file()
    }

    pub async fn search(&self, query: &str, max_results: Option<usize>) -> Result<Value> {
        self.init()?;
        self.search_existing(query, max_results, true).await
    }

    pub async fn search_readonly(&self, query: &str, max_results: Option<usize>) -> Result<Value> {
        if !self.readonly_available() {
            return Ok(
                json!({"ok": true, "query": query, "total_matches": 0, "semantic_used": false, "results": []}),
            );
        }
        self.search_existing(query, max_results, self.semantic_db.is_file())
            .await
    }

    async fn search_existing(
        &self,
        query: &str,
        max_results: Option<usize>,
        allow_semantic: bool,
    ) -> Result<Value> {
        let limit = max_results
            .unwrap_or(self.config.plugins.knowledge_base.max_search_results)
            .clamp(1, 50);
        let mut results = self.keyword_search(query, limit).await?;
        let strongest = results.first().map(|item| item.score).unwrap_or(0.0);
        let mut semantic_used = false;
        if allow_semantic
            && self.config.plugins.knowledge_base.embedding_enabled
            && strongest
                < self
                    .config
                    .plugins
                    .knowledge_base
                    .keyword_strong_score_threshold
        {
            if let Ok(semantic) = self.semantic_search(query).await {
                semantic_used = !semantic.is_empty();
                merge_results(&mut results, semantic, limit);
            }
        }
        Ok(json!({
            "ok": true,
            "query": query,
            "total_matches": results.len(),
            "semantic_used": semantic_used,
            "results": results.iter().map(SearchResult::to_json).collect::<Vec<_>>(),
        }))
    }

    pub fn stats(&self) -> Result<Value> {
        self.init()?;
        let files = self.list()?;
        let semantic = self.semantic_conn()?;
        let chunks: i64 =
            semantic.query_row("SELECT COUNT(*) FROM semantic_chunks", [], |row| row.get(0))?;
        Ok(json!({
            "ok": true,
            "root": self.root.display().to_string(),
            "files_dir": self.files_dir.display().to_string(),
            "files": files.len(),
            "total_size_kb": (files.iter().map(|file| file.size_bytes).sum::<i64>() as f64 / 1024.0 * 10.0).round() / 10.0,
            "semantic_chunks": chunks,
            "embedding_enabled": self.config.plugins.knowledge_base.embedding_enabled,
            "embedding_provider_id": self.config.plugins.knowledge_base.embedding_provider_id,
            "embedding_model": self.config.plugins.knowledge_base.embedding_model,
        }))
    }
}

async fn tool_search_readonly(args: Value, config: AppConfig, paths: MiyuPaths) -> Result<String> {
    ensure_enabled(&config)?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if query.is_empty() {
        bail!("query is required")
    }
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    Ok(KnowledgeBase::new(config, paths)?
        .search_readonly(query, max_results)
        .await?
        .to_string())
}

async fn tool_find_readonly(args: Value, config: AppConfig, paths: MiyuPaths) -> Result<String> {
    ensure_enabled(&config)?;
    // 合并后统一用 query;file_name_query 保留为兼容别名。
    let query = args
        .get("query")
        .or_else(|| args.get("file_name_query"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if query.is_empty() {
        bail!("query is required")
    }
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    Ok(KnowledgeBase::new(config, paths)?
        .find_by_name_readonly(query, max_results)?
        .to_string())
}

async fn tool_read_readonly(args: Value, config: AppConfig, paths: MiyuPaths) -> Result<String> {
    ensure_enabled(&config)?;
    let name = args
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if name.is_empty() {
        bail!("file_name is required")
    }
    let start_line = args.get("start_line").and_then(Value::as_u64).unwrap_or(1) as usize;
    let max_lines = args
        .get("max_lines")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    KnowledgeBase::new(config, paths)?.read_file_readonly(name, start_line, max_lines)
}

async fn tool_upload(args: Value, config: AppConfig, paths: MiyuPaths) -> Result<String> {
    ensure_enabled(&config)?;
    if !config.plugins.knowledge_base.upload_tool_enabled {
        bail!("knowledge base upload tool is disabled")
    }
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if content.is_empty() {
        bail!("content is required")
    }
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("knowledge note")
        .trim();
    let file_name = args
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    reject_non_kb_upload(content, title, file_name)?;
    let rel = if file_name.is_empty() {
        format!(
            "chat_uploads/{}/{}.md",
            Local::now().format("%Y-%m-%d"),
            slug(title)
        )
    } else {
        normalize_relative_path(file_name)?
    };
    let body = format!(
        "# {}\n\n> 来源：用户要求保存到本地知识库\n> 上传时间：{}\n\n{}\n",
        if title.is_empty() {
            Path::new(&rel)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("knowledge note")
        } else {
            title
        },
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        content
    );
    let kb = KnowledgeBase::new(config, paths)?;
    kb.init()?;
    let temp = tempfile::NamedTempFile::new()?;
    std::fs::write(temp.path(), body.as_bytes())?;
    let saved = kb.import_file(temp.path(), &rel)?;
    kb.spawn_embedding_reindex()?;
    Ok(json!({
        "ok": true,
        "path": saved,
    })
    .to_string())
}

async fn tool_edit(args: Value, config: AppConfig, paths: MiyuPaths) -> Result<String> {
    ensure_enabled(&config)?;
    let name = args
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if name.is_empty() {
        bail!("file_name is required")
    }
    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .context("start_line is required")? as usize;
    let end_line = args
        .get("end_line")
        .and_then(Value::as_u64)
        .context("end_line is required")? as usize;
    let replacement = args
        .get("replacement")
        .and_then(Value::as_str)
        .context("replacement is required")?;
    let result =
        KnowledgeBase::new(config, paths)?.edit_lines(name, start_line, end_line, replacement)?;
    Ok(json!({
        "ok": true,
        "path": result.path,
        "old_line_count": result.old_line_count,
        "new_line_count": result.new_line_count,
        "semantic_refreshed": result.semantic_refreshed,
        "warning": if name.starts_with("default-kb/") { Some("default-kb files may be overwritten by miyu update-default-kb") } else { None::<&str> },
    })
    .to_string())
}

async fn tool_remove(args: Value, config: AppConfig, paths: MiyuPaths) -> Result<String> {
    ensure_enabled(&config)?;
    let name = args
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if name.is_empty() {
        bail!("file_name is required")
    }
    let rel = normalize_relative_path(name)?;
    KnowledgeBase::new(config, paths)?.remove(&rel)?;
    Ok(json!({
        "ok": true,
        "path": rel,
        "warning": if name.starts_with("default-kb/") { Some("default-kb files may be restored by miyu update-default-kb") } else { None::<&str> },
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::MiyuPaths;

    pub(super) fn test_paths(root: &Path) -> MiyuPaths {
        MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("fish/conf.d/miyu.fish"),
            bash_hook_file: root.join("config/shell/bash-hook.sh"),
            zsh_hook_file: root.join("config/shell/zsh-hook.zsh"),
            scripts_dir: root.join("config/scripts"),
            system_scripts_dir: PathBuf::new(),
        }
    }

    #[test]
    fn edit_lines_replaces_inclusive_range() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let kb = KnowledgeBase::new(config, paths).unwrap();
        let source = temp.path().join("note.md");
        std::fs::write(&source, "one\ntwo\nthree\n").unwrap();
        kb.import_file(&source, "notes/note.md").unwrap();

        let result = kb.edit_lines("notes/note.md", 2, 2, "TWO\nTWO-B").unwrap();

        assert_eq!(result.old_line_count, 3);
        assert_eq!(result.new_line_count, 4);
        assert!(!result.semantic_refreshed);
        let edited =
            std::fs::read_to_string(kb.existing_file_path("notes/note.md").unwrap()).unwrap();
        assert_eq!(edited, "one\nTWO\nTWO-B\nthree\n");
        let chunks: i64 = kb
            .semantic_conn()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM semantic_chunks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(chunks, 0);
    }

    #[test]
    fn edit_lines_empty_replacement_deletes_range() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let kb = KnowledgeBase::new(config, paths).unwrap();
        let source = temp.path().join("note.md");
        std::fs::write(&source, "one\ntwo\nthree").unwrap();
        kb.import_file(&source, "note.md").unwrap();

        let result = kb.edit_lines("note.md", 2, 3, "").unwrap();

        assert_eq!(result.old_line_count, 3);
        assert_eq!(result.new_line_count, 1);
        let edited = std::fs::read_to_string(kb.existing_file_path("note.md").unwrap()).unwrap();
        assert_eq!(edited, "one");
    }

    #[test]
    fn edit_lines_rejects_out_of_range() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let kb = KnowledgeBase::new(config, paths).unwrap();
        let source = temp.path().join("note.md");
        std::fs::write(&source, "one\n").unwrap();
        kb.import_file(&source, "note.md").unwrap();

        let error = kb.edit_lines("note.md", 2, 2, "two").unwrap_err();

        assert!(error.to_string().contains("out of range"));
    }
}

#[cfg(test)]
mod scaling_probe {
    use super::*;
    use std::time::Instant;

    /// 量尺，不是断言：`cargo test --lib knowledge_base::scaling_probe -- --ignored --nocapture`
    ///
    /// keyword_search 对库里**每个**文件做「整读 + 整份 lowercase 拷贝」，
    /// 而且是在 `async fn search_existing` 里同步跑——这段时间 tokio worker
    /// 是卡住的。这里量的就是那段卡住有多长。
    #[test]
    #[ignore]
    fn keyword_search_scaling() {
        println!("\n  文件数  每文件KB   库总量MB   搜索耗时(ms)");
        for (files, kb_each) in [(20usize, 32usize), (50, 32), (100, 32), (200, 32)] {
            let temp = tempfile::tempdir().unwrap();
            let paths = super::tests::test_paths(temp.path());
            let kb = KnowledgeBase::new(AppConfig::default(), paths).unwrap();
            // 内容里不含查询词,走的是「全扫一遍都没命中」这条最坏路径
            let body = "lorem ipsum dolor sit amet ".repeat(kb_each * 1024 / 27);
            for index in 0..files {
                let source = temp.path().join(format!("doc{index}.md"));
                std::fs::write(&source, &body).unwrap();
                kb.import_file(&source, &format!("docs/doc{index}.md"))
                    .unwrap();
            }
            let start = Instant::now();
            let found = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(kb.keyword_search("需要检索的关键词", 5))
                .unwrap();
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            std::hint::black_box(found);
            let total_mb = (files * kb_each) as f64 / 1024.0;
            println!("  {files:>6}  {kb_each:>8}  {total_mb:>9.1}  {ms:>13.1}");
        }
    }

    /// 真正要证明的不是搜索本身变快了（活儿一样多），而是**搜索期间别的
    /// 异步任务还转不转**。单 worker 运行时上放一个 5ms 心跳，量它被堵住的
    /// 最长间隔：同步跑 = 堵满整个搜索时长，spawn_blocking = 基本不堵。
    #[test]
    #[ignore]
    fn keyword_search_does_not_freeze_the_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let paths = super::tests::test_paths(temp.path());
        let kb = KnowledgeBase::new(AppConfig::default(), paths).unwrap();
        let body = "lorem ipsum dolor sit amet ".repeat(200 * 1024 / 27);
        for index in 0..30 {
            let source = temp.path().join(format!("doc{index}.md"));
            std::fs::write(&source, &body).unwrap();
            kb.import_file(&source, &format!("docs/doc{index}.md"))
                .unwrap();
        }

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();

        for (label, blocking) in [
            ("同步跑（改前的做法）", true),
            ("spawn_blocking（现在）", false),
        ] {
            let gap = runtime.block_on(async {
                let worst = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                let probe = worst.clone();
                let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let halt = stop.clone();
                let ticker = tokio::spawn(async move {
                    let mut last = Instant::now();
                    while !halt.load(std::sync::atomic::Ordering::Relaxed) {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        let gap = last.elapsed().as_millis() as u64;
                        probe.fetch_max(gap, std::sync::atomic::Ordering::Relaxed);
                        last = Instant::now();
                    }
                });
                tokio::task::yield_now().await;
                // 搜索必须也在 spawn 出来的任务里跑,才会和心跳抢同一个
                // worker——`block_on` 的 future 跑在调用线程上,放这儿量不出
                // 任何东西(第一版探针就是这么白跑的)。
                let records = kb.list().unwrap();
                let search = tokio::spawn(async move {
                    if blocking {
                        // 改前的形状:在 async 上下文里直接同步跑
                        let found =
                            keyword_search_blocking(records, "需要检索的关键词", 5, 200, 200);
                        std::hint::black_box(found.unwrap());
                    } else {
                        let found = tokio::task::spawn_blocking(move || {
                            keyword_search_blocking(records, "需要检索的关键词", 5, 200, 200)
                        })
                        .await
                        .unwrap();
                        std::hint::black_box(found.unwrap());
                    }
                });
                let _ = search.await;
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                let _ = ticker.await;
                worst.load(std::sync::atomic::Ordering::Relaxed)
            });
            println!("  {label:<26} 心跳最长被堵 {gap:>5} ms");
        }
    }
}
