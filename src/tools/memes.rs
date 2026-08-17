use super::{vision, ToolRegistry, ToolSpec};
use crate::config::{AppConfig, MemesPluginConfig};
use crate::i18n::agent_text as t;
use crate::paths::MiyuPaths;
use crate::prompts::MEME_DESCRIPTION_PROMPT;
use anyhow::{bail, Context, Result};
use image::AnimationDecoder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufReader, Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::SystemTime;

const BUILTIN_MEMES_DIR: &str = "/usr/share/miyu/memes";
const MIN_SHORT_MEME_ID_LEN: usize = 7;

static MEME_LIBRARY_CACHE: OnceLock<RwLock<Option<MemeLibraryCache>>> = OnceLock::new();
static MEME_LIBRARY_LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

const MIN_IMAGE_EDGE: u32 = 32;
const MAX_IMAGE_EDGE: u32 = 4096;
const MAX_IMAGE_PIXELS: u64 = 16_000_000;
const MAX_GIF_FRAMES: usize = 120;
const MAX_GIF_DURATION_MS: u64 = 15_000;
const MAX_NAME_CHARS: usize = 80;
const MAX_DESCRIPTION_CHARS: usize = 500;
const MAX_USAGE_CHARS: usize = 500;
const MAX_AVOID_CHARS: usize = 500;
const MAX_TAGS: usize = 16;
const MAX_TAG_CHARS: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MemeRef {
    pub(crate) library: String,
    pub(crate) id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum MemeCollectionOutcome {
    Accepted { meme: MemeRef },
    Rejected { reason: String },
    AlreadyExists { meme: MemeRef },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemeClassification {
    save: bool,
    confidence: u8,
    positive_gates: PositiveGates,
    risk_gates: RiskGates,
    name: LocalizedName,
    description: String,
    usage: String,
    avoid: String,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PositiveGates {
    chat_reaction: bool,
    emotion_or_meme: bool,
    reusable: bool,
    context_independent: bool,
    persona_fit: bool,
    meaning_clear: bool,
    visual_quality: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RiskGates {
    ordinary_photo: bool,
    informational_content: bool,
    privacy: bool,
    advertisement: bool,
    unsafe_or_abusive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidatedImageFormat {
    Jpeg,
    Png,
    Gif,
    Webp,
}

impl ValidatedImageFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MemeIndex {
    #[serde(default)]
    library: String,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    memes: Vec<MemeItem>,
    #[serde(default)]
    disabled_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemeItem {
    id: String,
    name: LocalizedName,
    file: String,
    mime_type: String,
    #[serde(default)]
    animated: bool,
    description: String,
    usage: String,
    avoid: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<MemeOrigin>,
}

/// 表情包的收集来源：从哪个平台会话、谁发的、什么时候发/收的。
/// 本地 add_meme 入库的表情没有该字段。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct MemeOrigin {
    #[serde(default)]
    pub(crate) platform: String,
    #[serde(default)]
    pub(crate) conversation_kind: String,
    #[serde(default)]
    pub(crate) conversation_id: String,
    #[serde(default)]
    pub(crate) sender_id: String,
    #[serde(default)]
    pub(crate) sender_name: String,
    #[serde(default)]
    pub(crate) message_id: String,
    /// 消息发送时刻（RFC3339；平台未提供时为空）
    #[serde(default)]
    pub(crate) sent_at: String,
    /// 入库时刻（RFC3339）
    #[serde(default)]
    pub(crate) collected_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalizedName {
    #[serde(default)]
    zh: String,
    #[serde(default)]
    en: String,
}

#[derive(Debug, Clone)]
struct LoadedMeme {
    item: MemeItem,
    path: PathBuf,
    source: MemeSource,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum MemeSource {
    Builtin,
    User,
}

pub(crate) fn auto_meme_reminder(
    config: &AppConfig,
    user_message: &str,
    platform: bool,
) -> Option<String> {
    let meme_config = &config.plugins.memes;
    let auto_enabled = if platform {
        meme_config.auto_send_platform_enabled
    } else {
        meme_config.auto_send_enabled
    };
    if !meme_config.enabled
        || !auto_enabled
        || user_message.trim().is_empty()
        || meme_config.auto_send_probability <= 0.0
    {
        return None;
    }
    if rand::random::<f32>() > meme_config.auto_send_probability.clamp(0.0, 1.0) {
        return None;
    }
    Some(
        "<system-reminder>\n<send_meme_plan>\n触发自动发送表情包提醒。注意！本轮回复时你必须发送表情包。\n\n- 不要提及本提醒。\n- 根据上下文判断表情包是否合适，若匹配程度不足80%则不发送。\n- 不要说“我将发送表情包”。\n- 如果决定发送，应让文字回复和表情包语气自然一致。\n</send_meme_plan>\n</system-reminder>"
            .to_string(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemeLibraryCacheKey {
    library: String,
    builtin_index: PathBuf,
    user_index: PathBuf,
    builtin_mtime: Option<SystemTime>,
    user_mtime: Option<SystemTime>,
}

#[derive(Debug, Clone)]
struct MemeLibraryCache {
    key: MemeLibraryCacheKey,
    memes: Vec<LoadedMeme>,
}

pub fn register(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    if !config.plugins.memes.enabled {
        return;
    }
    register_search_and_show(registry, config.clone(), paths.clone());
    registry.register(
        ToolSpec::new(
            "add_meme",
            t(
                "Add a local image to the current persona's writable meme library. If metadata is not supplied, the tool asks the configured vision model to generate it from the image.",
                "把本地图片加入当前人格的可写表情库。若未提供元数据，工具会调用配置的识图模型根据图片生成。",
            ),
            json!({
                "type": "object",
                "properties": {
                    "image": { "type": "string", "description": t("Local image path.", "本地图片路径。") },
                    "library": { "type": "string", "description": t("Optional meme library override.", "可选表情库覆盖。") },
                    "name_zh": { "type": "string", "description": t("Chinese display name.", "中文显示名。") },
                    "name_en": { "type": "string", "description": t("English display name.", "英文显示名。") },
                    "description": { "type": "string", "description": t("Visible content description.", "图片可见内容描述。") },
                    "usage": { "type": "string", "description": t("When to use this meme.", "什么时候使用该表情。") },
                    "avoid": { "type": "string", "description": t("When not to use this meme.", "什么场景不要使用。") },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": t("Search tags.", "检索标签。") }
                },
                "required": ["image"],
                "additionalProperties": false
            }),
            {
                let config = config.clone();
                let paths = paths.clone();
                move |args| {
                    let config = config.clone();
                    let paths = paths.clone();
                    async move { add_meme(args, &config, &paths).await }
                }
            },
        )
        .writes(),
    );
    registry.register(
        ToolSpec::new(
            "update_meme",
            t(
                "Update meme index metadata in the writable overlay for the current library.",
                "更新当前表情库可写覆盖层中的表情元数据。",
            ),
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": t("Full sha256 id or unique short id.", "完整 sha256 id 或唯一短 id。") },
                    "library": { "type": "string", "description": t("Optional meme library override.", "可选表情库覆盖。") },
                    "name_zh": { "type": "string" },
                    "name_en": { "type": "string" },
                    "description": { "type": "string" },
                    "usage": { "type": "string" },
                    "avoid": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "enabled": { "type": "boolean", "description": t("Enable or disable this meme.", "启用或禁用该表情。") }
                },
                "required": ["id"],
                "additionalProperties": false
            }),
            {
                let config = config.clone();
                let paths = paths.clone();
                move |args| {
                    let config = config.clone();
                    let paths = paths.clone();
                    async move { update_meme(args, &config, &paths).await }
                }
            },
        )
        .writes(),
    );
    registry.register(
        ToolSpec::new(
            "delete_meme",
            t(
                "Delete a user meme or disable a built-in meme in the current library.",
                "删除用户表情，或在当前表情库中禁用内置表情。",
            ),
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": t("Full sha256 id or unique short id.", "完整 sha256 id 或唯一短 id。") },
                    "library": { "type": "string", "description": t("Optional meme library override.", "可选表情库覆盖。") },
                    "hard_delete": { "type": "boolean", "description": t("Permanently remove user image instead of moving it to trash.", "永久删除用户图片，而不是移入回收站。") }
                },
                "required": ["id"],
                "additionalProperties": false
            }),
            {
                let config = config.clone();
                let paths = paths.clone();
                move |args| {
                    let config = config.clone();
                    let paths = paths.clone();
                    async move { delete_meme(args, &config, &paths).await }
                }
            },
        )
        .writes(),
    );
}

pub fn register_chat(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    if !config.plugins.memes.enabled {
        return;
    }
    register_search_and_show(registry, config, paths);
}

fn register_search_and_show(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    registry.register(ToolSpec::new(
        "search_meme",
        t(
            "Search the current persona's meme library by scene, mood, tags, or visible content. Use before showing a meme unless the user provided a specific meme id.",
            "按场景、情绪、标签或画面内容搜索当前人格表情库。除非用户给了具体表情 id，否则发表情前先调用。",
        ),
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": t("Scene, mood, visible content, or user intent.", "场景、情绪、画面内容或用户意图。") },
                "tags": { "type": "array", "items": { "type": "string" }, "description": t("Optional preferred tags.", "可选偏好标签。") },
                "library": { "type": "string", "description": t("Optional meme library override.", "可选表情库覆盖。") },
                "limit": { "type": "integer", "description": t("Maximum number of candidates. Defaults to the meme plugin setting, max 3. Increase only when you need to compare alternatives.", "候选数量上限。默认使用表情包插件配置，最大 3。仅在需要比较多个备选时调大。") }
            },
            "additionalProperties": false
        }),
        {
            let config = config.clone();
            let paths = paths.clone();
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                async move { search_meme(args, &config, &paths).await }
            }
        },
    ));
    registry.register(ToolSpec::new_with_progress(
        "show_meme",
        t(
            "Render a meme in the terminal with chafa. GIFs are shown as static previews unless animation is explicitly allowed in config.",
            "发送表情包并使用 chafa 在终端渲染。GIF 默认显示静态预览，除非配置显式允许动画。",
        ),
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": t("Full sha256 id or unique short id.", "完整 sha256 id 或唯一短 id。") },
                "library": { "type": "string", "description": t("Optional meme library override.", "可选表情库覆盖。") },
                "size": { "type": "string", "description": t("Optional chafa size, e.g. 40x15.", "可选 chafa 尺寸，例如 40x15。") },
                "width": { "type": "integer", "description": t("Optional output width in terminal cells.", "可选终端单元格输出宽度。") },
                "height": { "type": "integer", "description": t("Optional output height in terminal cells.", "可选终端单元格输出高度。") }
            },
            "required": ["id"],
            "additionalProperties": false
        }),
        {
            let config = config.clone();
            let paths = paths.clone();
            move |args, progress| {
                let config = config.clone();
                let paths = paths.clone();
                async move { show_meme(args, &config, &paths, progress).await }
            }
        },
    ));
}

async fn search_meme(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let library = selected_library(&args, config);
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tags = string_array(args.get("tags"));
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(config.plugins.memes.search_max_results as u64)
        .clamp(1, 3) as usize;
    let loaded = load_library(paths, &library)?;
    let ids = meme_ids(&loaded);
    let mut scored = loaded
        .into_iter()
        .filter_map(|meme| {
            let score = score_meme(&meme.item, query, &tags);
            (score > 0.0).then_some((score, meme))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let results = scored
        .into_iter()
        .take(limit)
        .map(|(score, meme)| {
            json!({
                "id": unique_short_id_from_ids(&ids, &meme.item.id),
                "name": meme.item.name,
                "score": (score * 100.0).round() / 100.0,
                "description": meme.item.description,
                "usage": meme.item.usage,
                "avoid": meme.item.avoid,
                "tags": meme.item.tags,
                "animated": meme.item.animated,
                "source": source_label(meme.source),
                "origin": meme.item.origin,
            })
        })
        .collect::<Vec<_>>();
    if limit == 1 {
        return Ok(json!({
            "success": true,
            "library": library,
            "result": results.into_iter().next(),
        })
        .to_string());
    }
    Ok(json!({ "success": true, "library": library, "results": results }).to_string())
}

async fn show_meme(
    args: Value,
    config: &AppConfig,
    paths: &MiyuPaths,
    progress: crate::tools::ToolProgress,
) -> Result<String> {
    let library = selected_library(&args, config);
    let id = required_str(&args, "id")?;
    let memes = load_library(paths, &library)?;
    let ids = meme_ids(&memes);
    let meme = find_meme_in(memes, id)?.with_context(|| format!("meme not found: {id}"))?;
    let size = meme_print_size(&args, &config.plugins.memes);
    progress.report_image(meme.path.clone(), meme.item.description.clone());
    if progress.prepare_for_external_output().await {
        if meme.item.animated {
            let preview = static_gif_preview(&meme.path).await?;
            vision::print_image_file(preview.path(), size).await?;
        } else {
            vision::print_image_file(&meme.path, size).await?;
        }
    }
    Ok(json!({
        "success": true,
        "id": unique_short_id_from_ids(&ids, &meme.item.id),
        "description": meme.item.description,
        "origin": meme.item.origin,
    })
    .to_string())
}

async fn add_meme(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let library = selected_library(&args, config);
    let library_lock = library_lock(&library);
    let _guard = library_lock.lock().await;
    let source = expand_path(required_str(&args, "image")?);
    let metadata = std::fs::metadata(&source)
        .with_context(|| format!("failed to stat image {}", source.display()))?;
    if !metadata.is_file() {
        bail!("image path is not a file: {}", source.display())
    }
    let max_bytes = config
        .plugins
        .memes
        .max_image_mb
        .saturating_mul(1024 * 1024);
    if metadata.len() > max_bytes {
        bail!(
            "image too large: {} bytes; limit is {} MiB",
            metadata.len(),
            config.plugins.memes.max_image_mb
        )
    }
    let bytes = std::fs::read(&source)
        .with_context(|| format!("failed to read image {}", source.display()))?;
    let digest = Sha256::digest(&bytes);
    let hash = format!("{digest:x}");
    let id = format!("sha256:{hash}");
    if let Some(existing) = find_meme(paths, &library, &id)? {
        return Ok(json!({
            "success": true,
            "already_exists": true,
            "library": library,
            "id": id,
            "name": existing.item.name,
            "path": existing.path,
        })
        .to_string());
    }
    let format = validate_image_bytes(&bytes)?;
    let ext = format.extension();
    let mime_type = format.mime().to_string();
    let animated = format == ValidatedImageFormat::Gif;
    let user_dir = user_library_dir(paths, &library);
    let images_dir = user_dir.join("images");
    std::fs::create_dir_all(&images_dir)?;
    let target_file = format!("{}.{}", &hash[..16], ext);
    let target = images_dir.join(&target_file);
    std::fs::copy(&source, &target).with_context(|| {
        format!(
            "failed to copy image {} to {}",
            source.display(),
            target.display()
        )
    })?;
    let mut item = if has_supplied_metadata(&args) {
        match item_from_args(
            &args,
            id.clone(),
            format!("images/{target_file}"),
            mime_type,
            animated,
        ) {
            Ok(item) => item,
            Err(error) => {
                let _ = std::fs::remove_file(&target);
                return Err(error);
            }
        }
    } else {
        match classify_meme_image(config, paths, &target).await {
            Ok(classification) => match item_from_classification(
                id.clone(),
                format!("images/{target_file}"),
                mime_type,
                animated,
                classification,
                None,
            ) {
                Ok(item) => item,
                Err(err) => {
                    let _ = std::fs::remove_file(&target);
                    return Ok(json!({
                        "success": false,
                        "rejected": true,
                        "message": "vision classification rejected the image",
                        "error": err.to_string(),
                    })
                    .to_string());
                }
            },
            Err(err) => {
                let _ = std::fs::remove_file(&target);
                return Ok(json!({
                    "success": false,
                    "needs_user_info": true,
                    "message": "vision metadata generation failed; ask the user what the image shows and when to use it, then call add_meme again with metadata fields",
                    "error": err.to_string(),
                })
                .to_string());
            }
        }
    };
    item.file = format!("images/{target_file}");
    let mut index = load_index(&user_dir.join("index.json"))?.unwrap_or_else(|| MemeIndex {
        library: library.clone(),
        version: 2,
        memes: Vec::new(),
        disabled_ids: Vec::new(),
    });
    index.library = library.clone();
    index.version = 2;
    index.disabled_ids.retain(|value| !ids_match(value, &id));
    index.memes.retain(|meme| !ids_match(&meme.id, &id));
    index.memes.push(item.clone());
    if let Err(error) = save_index(&user_dir.join("index.json"), &index) {
        let _ = std::fs::remove_file(&target);
        return Err(error);
    }
    Ok(json!({
        "success": true,
        "library": library,
        "id": item.id,
        "name": item.name,
        "path": target,
        "metadata": item,
    })
    .to_string())
}

async fn update_meme(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let library = selected_library(&args, config);
    let library_lock = library_lock(&library);
    let _guard = library_lock.lock().await;
    let id = required_str(&args, "id")?;
    let existing =
        find_meme_any(paths, &library, id)?.with_context(|| format!("meme not found: {id}"))?;
    let id = existing.item.id.clone();
    let user_dir = user_library_dir(paths, &library);
    if matches!(existing.source, MemeSource::Builtin) {
        // 内置条目的 file 是相对内置目录的路径:落进用户 overlay 前必须
        // 把图片实体复制过来,否则之后按 user_dir 解析落空,show 必失败。
        let target = user_dir.join(&existing.item.file);
        if !target.exists() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&existing.path, &target).with_context(|| {
                format!(
                    "failed to copy builtin meme image {} to {}",
                    existing.path.display(),
                    target.display()
                )
            })?;
        }
    }
    let mut index = load_index(&user_dir.join("index.json"))?.unwrap_or_else(|| MemeIndex {
        library: library.clone(),
        version: 2,
        memes: Vec::new(),
        disabled_ids: Vec::new(),
    });
    index.library = library.clone();
    index.version = 2;
    let mut item = existing.item;
    apply_updates(&mut item, &args);
    if !index.memes.iter().any(|meme| ids_match(&meme.id, &id)) {
        index.memes.push(item.clone());
    } else {
        for meme in &mut index.memes {
            if ids_match(&meme.id, &id) {
                *meme = item.clone();
                break;
            }
        }
    }
    if let Some(enabled) = args.get("enabled").and_then(Value::as_bool) {
        if enabled {
            index.disabled_ids.retain(|value| !ids_match(value, &id));
        } else if !index.disabled_ids.iter().any(|value| ids_match(value, &id)) {
            index.disabled_ids.push(id.clone());
        }
    }
    save_index(&user_dir.join("index.json"), &index)?;
    Ok(json!({ "success": true, "library": library, "id": id, "metadata": item }).to_string())
}

async fn delete_meme(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let library = selected_library(&args, config);
    let library_lock = library_lock(&library);
    let _guard = library_lock.lock().await;
    let requested_id = required_str(&args, "id")?;
    let user_dir = user_library_dir(paths, &library);
    let index_path = user_dir.join("index.json");
    let mut index = load_index(&index_path)?.unwrap_or_else(|| MemeIndex {
        library: library.clone(),
        version: 2,
        memes: Vec::new(),
        disabled_ids: Vec::new(),
    });
    index.library = library.clone();
    index.version = 2;
    if let Some(pos) = index
        .memes
        .iter()
        .position(|meme| ids_match(&meme.id, requested_id))
    {
        let item = index.memes.remove(pos);
        let id = item.id.clone();
        let path = user_dir.join(&item.file);
        if path.is_file() {
            if args
                .get("hard_delete")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                std::fs::remove_file(&path)?;
            } else {
                trash::delete(&path)?;
            }
        }
        index.disabled_ids.retain(|value| !ids_match(value, &id));
        save_index(&index_path, &index)?;
        return Ok(
            json!({ "success": true, "library": library, "id": id, "action": "deleted_user_meme" })
                .to_string(),
        );
    }
    if let Some(meme) = find_meme(paths, &library, requested_id)? {
        let id = meme.item.id;
        if !index.disabled_ids.iter().any(|value| ids_match(value, &id)) {
            index.disabled_ids.push(id.clone());
        }
        save_index(&index_path, &index)?;
        return Ok(json!({ "success": true, "library": library, "id": id, "action": "disabled_builtin_meme" }).to_string());
    }
    bail!("meme not found: {requested_id}")
}

async fn classify_meme_image(
    config: &AppConfig,
    paths: &MiyuPaths,
    image: &Path,
) -> Result<MemeClassification> {
    let persona = config.active_persona_prompt(paths).unwrap_or_default();
    let persona = persona.chars().take(4_000).collect::<String>();
    let prompt = if persona.trim().is_empty() {
        MEME_DESCRIPTION_PROMPT.to_string()
    } else {
        format!(
            "{MEME_DESCRIPTION_PROMPT}\n\n## 当前人格约束\n仅当图片明确符合以下人格时，persona_fit 才能为 true：\n{persona}"
        )
    };
    let text = vision::analyze_local_image_with_prompt(config, paths, image, &prompt).await?;
    let classification: MemeClassification = serde_json::from_str(text.trim())
        .context("vision response was not the strict meme schema")?;
    validate_classification(&classification)?;
    Ok(classification)
}

pub(crate) async fn collect_meme_from_local_image(
    image: &Path,
    config: &AppConfig,
    paths: &MiyuPaths,
    origin: Option<MemeOrigin>,
) -> Result<MemeCollectionOutcome> {
    let library = current_persona_library(config);
    let image = image.to_path_buf();
    let max_bytes = config
        .plugins
        .memes
        .max_image_mb
        .saturating_mul(1024 * 1024);
    let prepared = match tokio::task::spawn_blocking(move || prepare_image(&image, max_bytes))
        .await
        .context("image validation task failed")?
    {
        Ok(prepared) => prepared,
        Err(error) => {
            return Ok(MemeCollectionOutcome::Rejected {
                reason: error.to_string(),
            })
        }
    };
    let meme_ref = MemeRef {
        library: library.clone(),
        id: prepared.id.clone(),
    };
    if find_meme(paths, &library, &prepared.id)?.is_some() {
        return Ok(MemeCollectionOutcome::AlreadyExists { meme: meme_ref });
    }

    let vision_input = tempfile::Builder::new()
        .suffix(&format!(".{}", prepared.format.extension()))
        .tempfile()?;
    std::fs::copy(&prepared.source, vision_input.path())?;
    let classification = match classify_meme_image(config, paths, vision_input.path()).await {
        Ok(classification) => classification,
        Err(error) => {
            return Ok(MemeCollectionOutcome::Rejected {
                reason: error.to_string(),
            })
        }
    };
    if !classification.save {
        return Ok(MemeCollectionOutcome::Rejected {
            reason: "vision classification rejected the image".to_string(),
        });
    }

    let lock = library_lock(&library);
    let _guard = lock.lock().await;
    if find_meme(paths, &library, &prepared.id)?.is_some() {
        return Ok(MemeCollectionOutcome::AlreadyExists { meme: meme_ref });
    }
    let user_dir = user_library_dir(paths, &library);
    let images_dir = user_dir.join("images");
    std::fs::create_dir_all(&images_dir)?;
    let target_file = format!("{}.{}", &prepared.hash[..16], prepared.format.extension());
    let target = images_dir.join(&target_file);
    std::fs::copy(&prepared.source, &target).with_context(|| {
        format!(
            "failed to copy image {} to {}",
            prepared.source.display(),
            target.display()
        )
    })?;
    let origin = origin.map(|mut origin| {
        origin.collected_at = chrono::Utc::now().to_rfc3339();
        origin
    });
    let item = match item_from_classification(
        prepared.id.clone(),
        format!("images/{target_file}"),
        prepared.format.mime().to_string(),
        prepared.format == ValidatedImageFormat::Gif,
        classification,
        origin,
    ) {
        Ok(item) => item,
        Err(error) => {
            let _ = std::fs::remove_file(&target);
            return Ok(MemeCollectionOutcome::Rejected {
                reason: error.to_string(),
            });
        }
    };
    let mut index = load_index(&user_dir.join("index.json"))?.unwrap_or_else(|| MemeIndex {
        library: library.clone(),
        version: 2,
        memes: Vec::new(),
        disabled_ids: Vec::new(),
    });
    index.library = library.clone();
    index.version = 2;
    index
        .disabled_ids
        .retain(|value| !ids_match(value, &prepared.id));
    // 之前被禁用的同 id 条目还留在 memes 里(find_meme 滤 disabled 看
    // 不见它):替换而不是追加,避免同 id 双条目。
    if let Some(existing) = index
        .memes
        .iter_mut()
        .find(|meme| ids_match(&meme.id, &prepared.id))
    {
        *existing = item;
    } else {
        index.memes.push(item);
    }
    if let Err(error) = save_index(&user_dir.join("index.json"), &index) {
        let _ = std::fs::remove_file(&target);
        return Err(error);
    }
    Ok(MemeCollectionOutcome::Accepted { meme: meme_ref })
}

struct PreparedImage {
    source: PathBuf,
    hash: String,
    id: String,
    format: ValidatedImageFormat,
}

fn prepare_image(source: &Path, max_bytes: u64) -> Result<PreparedImage> {
    let metadata = std::fs::metadata(source)
        .with_context(|| format!("failed to stat image {}", source.display()))?;
    if !metadata.is_file() {
        bail!("image path is not a file: {}", source.display())
    }
    if metadata.len() > max_bytes {
        bail!("image exceeds the configured meme size limit")
    }
    let bytes = std::fs::read(source)
        .with_context(|| format!("failed to read image {}", source.display()))?;
    let format = validate_image_bytes(&bytes)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    Ok(PreparedImage {
        source: source.to_path_buf(),
        id: format!("sha256:{hash}"),
        hash,
        format,
    })
}

fn load_library(paths: &MiyuPaths, library: &str) -> Result<Vec<LoadedMeme>> {
    let builtin_dir = builtin_library_dir(library);
    let user_dir = user_library_dir(paths, library);
    let builtin_index = builtin_dir.join("index.json");
    let user_index = user_dir.join("index.json");
    let key = MemeLibraryCacheKey {
        library: sanitize_library(library),
        builtin_mtime: index_mtime(&builtin_index),
        user_mtime: index_mtime(&user_index),
        builtin_index: builtin_index.clone(),
        user_index: user_index.clone(),
    };
    let cache = MEME_LIBRARY_CACHE.get_or_init(|| RwLock::new(None));
    if let Some(cached) = cache.read().unwrap().as_ref() {
        if cached.key == key {
            return Ok(cached.memes.clone());
        }
    }
    let builtin = load_index(&builtin_index)?.unwrap_or_default();
    let user = load_index(&user_index)?.unwrap_or_default();
    let disabled = user.disabled_ids;
    let mut user_ids = Vec::new();
    let mut result = Vec::new();
    for item in user.memes {
        if disabled.iter().any(|id| ids_match(id, &item.id)) {
            continue;
        }
        user_ids.push(item.id.clone());
        result.push(LoadedMeme {
            path: user_dir.join(&item.file),
            item,
            source: MemeSource::User,
        });
    }
    for item in builtin.memes {
        if disabled.iter().any(|id| ids_match(id, &item.id))
            || user_ids.iter().any(|id| ids_match(id, &item.id))
        {
            continue;
        }
        result.push(LoadedMeme {
            path: builtin_dir.join(&item.file),
            item,
            source: MemeSource::Builtin,
        });
    }
    *cache.write().unwrap() = Some(MemeLibraryCache {
        key,
        memes: result.clone(),
    });
    Ok(result)
}

fn index_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

fn find_meme(paths: &MiyuPaths, library: &str, id: &str) -> Result<Option<LoadedMeme>> {
    find_meme_in(load_library(paths, library)?, id)
}

/// 与 [`find_meme`] 相同的匹配规则,但不滤 disabled:重新启用
/// (`enabled=true`)必须能找到已禁用条目,否则禁用成了单向门。
/// 管理操作低频,不走库缓存。
fn find_meme_any(paths: &MiyuPaths, library: &str, id: &str) -> Result<Option<LoadedMeme>> {
    let builtin_dir = builtin_library_dir(library);
    let user_dir = user_library_dir(paths, library);
    let builtin = load_index(&builtin_dir.join("index.json"))?.unwrap_or_default();
    let user = load_index(&user_dir.join("index.json"))?.unwrap_or_default();
    let mut user_ids = Vec::new();
    let mut result = Vec::new();
    for item in user.memes {
        user_ids.push(item.id.clone());
        result.push(LoadedMeme {
            path: user_dir.join(&item.file),
            item,
            source: MemeSource::User,
        });
    }
    for item in builtin.memes {
        if user_ids.iter().any(|id| ids_match(id, &item.id)) {
            continue;
        }
        result.push(LoadedMeme {
            path: builtin_dir.join(&item.file),
            item,
            source: MemeSource::Builtin,
        });
    }
    find_meme_in(result, id)
}

fn find_meme_in(memes: Vec<LoadedMeme>, id: &str) -> Result<Option<LoadedMeme>> {
    let requested = id_hash_part(id);
    if requested.is_empty() {
        return Ok(None);
    }
    if !is_full_hash(requested) && requested.len() < MIN_SHORT_MEME_ID_LEN {
        bail!(
            "meme id prefix is too short: {requested}; use at least {MIN_SHORT_MEME_ID_LEN} hex characters"
        );
    }
    let mut matches = memes
        .into_iter()
        .filter(|meme| id_hash_part(&meme.item.id).starts_with(requested))
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => bail!("meme id prefix is ambiguous: {requested}; use a longer id"),
    }
}

fn ids_match(stored: &str, requested: &str) -> bool {
    let stored = id_hash_part(stored);
    let requested = id_hash_part(requested);
    !requested.is_empty() && stored.starts_with(requested)
}

fn id_hash_part(value: &str) -> &str {
    let value = value.trim();
    value.strip_prefix("sha256:").unwrap_or(value)
}

fn is_full_hash(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn meme_ids(memes: &[LoadedMeme]) -> Vec<String> {
    memes.iter().map(|meme| meme.item.id.clone()).collect()
}

fn unique_short_id_from_ids(ids: &[String], id: &str) -> String {
    let hash = id_hash_part(id);
    if hash.len() <= MIN_SHORT_MEME_ID_LEN {
        return hash.to_string();
    }
    for len in MIN_SHORT_MEME_ID_LEN..=hash.len() {
        let prefix = &hash[..len];
        let matches = ids
            .iter()
            .filter(|candidate| id_hash_part(candidate).starts_with(prefix))
            .count();
        if matches <= 1 {
            return prefix.to_string();
        }
    }
    hash.to_string()
}

fn load_index(path: &Path) -> Result<Option<MemeIndex>> {
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&std::fs::read_to_string(path)?)?))
}

fn save_index(path: &Path, index: &MemeIndex) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut temp, index)?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        temp.persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("atomically replacing meme index {}", path.display()))?;
        return Ok(());
    }
    bail!("meme index path has no parent: {}", path.display())
}

fn selected_library(args: &Value, config: &AppConfig) -> String {
    args.get("library")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_library)
        .unwrap_or_else(|| current_persona_library(config))
}

pub(crate) fn current_persona_library(config: &AppConfig) -> String {
    sanitize_library(
        &config
            .plugins
            .memes
            .library_for_persona(&config.prompt.active_persona),
    )
}

pub(crate) fn meme_ref_exists(paths: &MiyuPaths, meme: &MemeRef) -> Result<bool> {
    Ok(find_meme(paths, &meme.library, &meme.id)?.is_some())
}

pub(crate) async fn delete_meme_reference(
    meme: &MemeRef,
    config: &AppConfig,
    paths: &MiyuPaths,
) -> Result<()> {
    let result = delete_meme(
        json!({
            "library": meme.library,
            "id": meme.id,
            "hard_delete": false,
        }),
        config,
        paths,
    )
    .await?;
    let result: Value = serde_json::from_str(&result)?;
    if result.get("success").and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        bail!("meme deletion did not succeed")
    }
}

fn library_lock(library: &str) -> Arc<tokio::sync::Mutex<()>> {
    let key = sanitize_library(library);
    let mut locks = MEME_LIBRARY_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn sanitize_library(value: &str) -> String {
    let value = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if value.is_empty() {
        "default".to_string()
    } else {
        value
    }
}

fn builtin_library_dir(library: &str) -> PathBuf {
    if let Some(path) = std::env::var_os("MIYU_MEMES_DIR") {
        return PathBuf::from(path).join(library);
    }
    let dev = PathBuf::from("src/memes").join(library);
    if dev.is_dir() {
        return dev;
    }
    PathBuf::from(BUILTIN_MEMES_DIR).join(library)
}

fn user_library_dir(paths: &MiyuPaths, library: &str) -> PathBuf {
    paths.data_dir.join("memes").join(sanitize_library(library))
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        bail!("{key} is required")
    }
    Ok(value)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn score_meme(item: &MemeItem, query: &str, tags: &[String]) -> f32 {
    let query = normalize(&format!("{query} {}", tags.join(" ")));
    let terms = search_terms(&query);
    if terms.is_empty() {
        return 0.1;
    }
    let name = normalize(&format!("{} {}", item.name.zh, item.name.en));
    let description = normalize(&item.description);
    let usage = normalize(&item.usage);
    let avoid = normalize(&item.avoid);
    let tag_text = normalize(&item.tags.join(" "));
    let mut score: f32 = 0.0;
    for term in terms {
        if tag_text.contains(&term) {
            score += 3.0;
        }
        if name.contains(&term) {
            score += 2.5;
        }
        if usage.contains(&term) {
            score += 2.0;
        }
        if description.contains(&term) {
            score += 1.2;
        }
        if !avoid.is_empty() && avoid.contains(&term) {
            score -= 2.5;
        }
    }
    let haystack = format!("{name} {description} {usage} {tag_text}");
    if !query.is_empty() && haystack.contains(&query) {
        score += 2.0;
    }
    score.max(0.0)
}

fn search_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for token in query.split_whitespace() {
        if token.chars().count() > 1 {
            terms.push(token.to_string());
        }
        if token.chars().any(|ch| !ch.is_ascii()) {
            let chars = token.chars().collect::<Vec<_>>();
            for pair in chars.windows(2) {
                terms.push(pair.iter().collect());
            }
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

fn normalize(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_punctuation()
                || matches!(
                    ch,
                    '，' | '。' | '！' | '？' | '、' | '；' | '：' | '（' | '）' | '“' | '”'
                )
            {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>()
}

fn validate_classification(classification: &MemeClassification) -> Result<()> {
    if !classification.save {
        return Ok(());
    }
    if classification.confidence != 100 {
        bail!("accepted meme classification confidence must be exactly 100")
    }
    if !classification.positive_gates.chat_reaction
        || !classification.positive_gates.emotion_or_meme
        || !classification.positive_gates.reusable
        || !classification.positive_gates.context_independent
        || !classification.positive_gates.persona_fit
        || !classification.positive_gates.meaning_clear
        || !classification.positive_gates.visual_quality
    {
        bail!("accepted meme classification did not pass every positive gate")
    }
    if classification.risk_gates.ordinary_photo
        || classification.risk_gates.informational_content
        || classification.risk_gates.privacy
        || classification.risk_gates.advertisement
        || classification.risk_gates.unsafe_or_abusive
    {
        bail!("accepted meme classification triggered a risk gate")
    }
    validate_text_field("name.zh", &classification.name.zh, 1, MAX_NAME_CHARS)?;
    validate_text_field("name.en", &classification.name.en, 0, MAX_NAME_CHARS)?;
    validate_text_field(
        "description",
        &classification.description,
        1,
        MAX_DESCRIPTION_CHARS,
    )?;
    validate_text_field("usage", &classification.usage, 1, MAX_USAGE_CHARS)?;
    validate_text_field("avoid", &classification.avoid, 0, MAX_AVOID_CHARS)?;
    validate_tags(&classification.tags, true)?;
    Ok(())
}

fn validate_tags(tags: &[String], required: bool) -> Result<()> {
    if (required && tags.is_empty()) || tags.len() > MAX_TAGS {
        bail!(
            "tags must contain between {} and {MAX_TAGS} items",
            usize::from(required)
        )
    }
    let mut normalized = std::collections::HashSet::new();
    for tag in tags {
        validate_text_field("tag", tag, 1, MAX_TAG_CHARS)?;
        if tag.chars().any(char::is_whitespace) {
            bail!("tags must be short single tokens")
        }
        if !normalized.insert(tag.to_lowercase()) {
            bail!("tags must be unique")
        }
    }
    Ok(())
}

fn validate_text_field(name: &str, value: &str, min: usize, max: usize) -> Result<()> {
    let trimmed = value.trim();
    let count = trimmed.chars().count();
    if trimmed != value || count < min || count > max || value.chars().any(char::is_control) {
        bail!("{name} must be trimmed, control-free, and contain {min}..={max} characters")
    }
    Ok(())
}

fn validate_image_bytes(bytes: &[u8]) -> Result<ValidatedImageFormat> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("detecting image format")?;
    let image_format = reader.format().context("unsupported image format")?;
    let format = match image_format {
        image::ImageFormat::Jpeg => ValidatedImageFormat::Jpeg,
        image::ImageFormat::Png => ValidatedImageFormat::Png,
        image::ImageFormat::Gif => ValidatedImageFormat::Gif,
        image::ImageFormat::WebP => ValidatedImageFormat::Webp,
        _ => bail!("unsupported image format; supported: jpeg, png, gif, webp"),
    };
    let (width, height) = reader
        .into_dimensions()
        .context("decoding image dimensions")?;
    validate_dimensions(width, height)?;
    if format == ValidatedImageFormat::Gif {
        validate_gif(bytes)?;
    } else {
        image::load_from_memory_with_format(bytes, image_format).context("decoding image")?;
    }
    Ok(format)
}

fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    if !(MIN_IMAGE_EDGE..=MAX_IMAGE_EDGE).contains(&width)
        || !(MIN_IMAGE_EDGE..=MAX_IMAGE_EDGE).contains(&height)
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        bail!(
            "image dimensions must be {MIN_IMAGE_EDGE}..={MAX_IMAGE_EDGE} per edge and at most {MAX_IMAGE_PIXELS} pixels"
        )
    }
    Ok(())
}

fn validate_gif(bytes: &[u8]) -> Result<()> {
    let decoder = image::codecs::gif::GifDecoder::new(BufReader::new(Cursor::new(bytes)))
        .context("decoding GIF")?;
    let frames = decoder.into_frames();
    let mut frame_count = 0_usize;
    let mut duration_ms = 0_u64;
    for frame in frames {
        let frame = frame.context("decoding GIF frame")?;
        frame_count += 1;
        if frame_count > MAX_GIF_FRAMES {
            bail!("GIF must contain 1..={MAX_GIF_FRAMES} frames")
        }
        validate_dimensions(frame.buffer().width(), frame.buffer().height())?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        if denominator == 0 {
            bail!("GIF frame has an invalid delay")
        }
        duration_ms = duration_ms.saturating_add(
            u64::from(numerator).saturating_add(u64::from(denominator) - 1)
                / u64::from(denominator),
        );
        if duration_ms > MAX_GIF_DURATION_MS {
            bail!("GIF duration exceeds 15 seconds")
        }
    }
    if frame_count == 0 {
        bail!("GIF must contain 1..={MAX_GIF_FRAMES} frames")
    }
    Ok(())
}

async fn static_gif_preview(path: &Path) -> Result<tempfile::NamedTempFile> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path)
            .with_context(|| format!("opening GIF {}", path.display()))?;
        let decoder = image::codecs::gif::GifDecoder::new(BufReader::new(file))
            .context("decoding GIF preview")?;
        let frame = decoder
            .into_frames()
            .next()
            .transpose()
            .context("decoding first GIF frame")?
            .context("GIF has no frames")?;
        let temp = tempfile::Builder::new().suffix(".png").tempfile()?;
        frame
            .buffer()
            .save_with_format(temp.path(), image::ImageFormat::Png)
            .context("writing static GIF preview")?;
        Ok(temp)
    })
    .await
    .context("GIF preview task failed")?
}

fn meme_print_size(args: &Value, config: &MemesPluginConfig) -> Option<String> {
    let width = args
        .get("width")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(160);
    let height = args
        .get("height")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(80);
    match (width, height) {
        (0, 0) => args
            .get("size")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| configured_meme_size(config)),
        (width, 0) => Some(format!("{width}x")),
        (0, height) => Some(format!("x{height}")),
        (width, height) => Some(format!("{width}x{height}")),
    }
}

pub(crate) fn configured_meme_size(config: &MemesPluginConfig) -> Option<String> {
    let (cols, rows) = crossterm::terminal::size().ok()?;
    let width = ((cols as u32 * config.width_percent as u32) / 100)
        .max(1)
        .min(160);
    let height = ((rows as u32 * config.height_percent as u32) / 100)
        .max(1)
        .min(80);
    Some(format!("{width}x{height}"))
}

fn expand_path(value: &str) -> PathBuf {
    if let Some(rest) = value.trim().strip_prefix("~/") {
        if let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
            return home.join(rest);
        }
    }
    let path = Path::new(value.trim());
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        super::workspace::effective_workdir().join(path)
    }
}

fn has_supplied_metadata(args: &Value) -> bool {
    [
        "name_zh",
        "name_en",
        "description",
        "usage",
        "avoid",
        "tags",
    ]
    .iter()
    .any(|key| args.get(*key).is_some())
}

fn item_from_args(
    args: &Value,
    id: String,
    file: String,
    mime_type: String,
    animated: bool,
) -> Result<MemeItem> {
    let name = LocalizedName {
        zh: args
            .get("name_zh")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        en: args
            .get("name_en")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
    };
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let usage = args
        .get("usage")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.zh.is_empty() || description.is_empty() || usage.is_empty() {
        bail!("name_zh, description, and usage are required when supplying metadata manually")
    }
    let tags = string_array(args.get("tags"));
    validate_text_field("name.zh", &name.zh, 1, MAX_NAME_CHARS)?;
    validate_text_field("name.en", &name.en, 0, MAX_NAME_CHARS)?;
    validate_text_field("description", &description, 1, MAX_DESCRIPTION_CHARS)?;
    validate_text_field("usage", &usage, 1, MAX_USAGE_CHARS)?;
    let avoid = args
        .get("avoid")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    validate_text_field("avoid", &avoid, 0, MAX_AVOID_CHARS)?;
    validate_tags(&tags, false)?;
    Ok(MemeItem {
        id,
        name,
        file,
        mime_type,
        animated,
        description,
        usage,
        avoid,
        tags,
        origin: None,
    })
}

fn item_from_classification(
    id: String,
    file: String,
    mime_type: String,
    animated: bool,
    classification: MemeClassification,
    origin: Option<MemeOrigin>,
) -> Result<MemeItem> {
    validate_classification(&classification)?;
    if !classification.save {
        bail!("vision classification rejected the image")
    }
    let item = MemeItem {
        id,
        name: classification.name,
        file,
        mime_type,
        animated,
        description: classification.description,
        usage: classification.usage,
        avoid: classification.avoid,
        tags: classification.tags,
        origin,
    };
    Ok(item)
}

fn apply_updates(item: &mut MemeItem, args: &Value) {
    if let Some(value) = args
        .get("name_zh")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        item.name.zh = value.to_string();
    }
    if let Some(value) = args
        .get("name_en")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        item.name.en = value.to_string();
    }
    if let Some(value) = args
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        item.description = value.to_string();
    }
    if let Some(value) = args
        .get("usage")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        item.usage = value.to_string();
    }
    if let Some(value) = args.get("avoid").and_then(Value::as_str).map(str::trim) {
        item.avoid = value.to_string();
    }
    if args.get("tags").is_some() {
        item.tags = string_array(args.get("tags"));
    }
}

fn source_label(source: MemeSource) -> &'static str {
    match source {
        MemeSource::Builtin => "builtin",
        MemeSource::User => "user",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, Frame, ImageEncoder, Rgba, RgbaImage};

    #[test]
    fn sanitize_library_keeps_simple_names() {
        assert_eq!(sanitize_library("Miyu"), "miyu");
        assert_eq!(sanitize_library("默认 表情"), "default");
    }

    /// 自动提示发送表情的平台/本地开关相互独立(两者默认都开)。
    #[test]
    fn platform_and_local_auto_send_gates_are_independent() {
        let mut config = AppConfig::default();
        config.plugins.memes.auto_send_probability = 1.0;
        assert!(auto_meme_reminder(&config, "你好", true).is_some());
        assert!(auto_meme_reminder(&config, "你好", false).is_some());
        config.plugins.memes.auto_send_enabled = false;
        assert!(auto_meme_reminder(&config, "你好", false).is_none());
        assert!(auto_meme_reminder(&config, "你好", true).is_some());
        config.plugins.memes.auto_send_enabled = true;
        config.plugins.memes.auto_send_platform_enabled = false;
        assert!(auto_meme_reminder(&config, "你好", false).is_some());
        assert!(auto_meme_reminder(&config, "你好", true).is_none());
    }

    #[test]
    fn scores_tag_matches_higher_than_no_match() {
        let item = MemeItem {
            id: "sha256:test".to_string(),
            name: LocalizedName {
                zh: "Linux 企鹅".to_string(),
                en: "Linux Penguin".to_string(),
            },
            file: "images/test.png".to_string(),
            mime_type: "image/png".to_string(),
            animated: false,
            description: "戴墨镜的企鹅抱着终端".to_string(),
            usage: "适合 Linux 话题".to_string(),
            avoid: String::new(),
            tags: vec!["Linux".to_string(), "企鹅".to_string()],
            origin: None,
        };
        assert!(score_meme(&item, "Linux", &[]) > score_meme(&item, "炸鸡", &[]));
    }

    #[test]
    fn current_library_follows_persona_mapping() {
        let mut config = AppConfig::default();
        assert_eq!(current_persona_library(&config), "miyu");
        config.prompt.active_persona = "Custom Persona.md".to_string();
        config.plugins.memes.persona_libraries.insert(
            config.active_persona_scope(),
            "Shared Reactions".to_string(),
        );
        assert_eq!(current_persona_library(&config), "shared-reactions");
    }

    #[test]
    fn strict_classification_requires_all_acceptance_gates() {
        let accepted = accepted_classification();
        validate_classification(&accepted).unwrap();

        let mut low_confidence = accepted.clone();
        low_confidence.confidence = 99;
        assert!(validate_classification(&low_confidence).is_err());

        let mut missing_positive = accepted.clone();
        missing_positive.positive_gates.reusable = false;
        assert!(validate_classification(&missing_positive).is_err());

        let mut ordinary_photo = accepted;
        ordinary_photo.risk_gates.ordinary_photo = true;
        assert!(validate_classification(&ordinary_photo).is_err());
    }

    #[test]
    fn rejected_classification_never_becomes_an_item() {
        let mut rejected = accepted_classification();
        rejected.save = false;
        validate_classification(&rejected).unwrap();
        assert!(item_from_classification(
            "sha256:test".to_string(),
            "images/test.png".to_string(),
            "image/png".to_string(),
            false,
            rejected,
            None,
        )
        .is_err());
    }

    #[test]
    fn meme_item_origin_roundtrips_and_stays_backward_compatible() {
        let legacy = r#"{"id":"sha256:x","name":{"zh":"名","en":""},"file":"images/x.png","mime_type":"image/png","description":"d","usage":"u","avoid":""}"#;
        let item: MemeItem = serde_json::from_str(legacy).unwrap();
        assert!(item.origin.is_none());
        assert!(!serde_json::to_string(&item).unwrap().contains("origin"));

        let with_origin = MemeItem {
            origin: Some(MemeOrigin {
                platform: "onebot".to_string(),
                sender_id: "10001".to_string(),
                sender_name: "群友".to_string(),
                sent_at: "2026-08-10T12:00:00+00:00".to_string(),
                ..Default::default()
            }),
            ..item
        };
        let text = serde_json::to_string(&with_origin).unwrap();
        let back: MemeItem = serde_json::from_str(&text).unwrap();
        let origin = back.origin.unwrap();
        assert_eq!(origin.sender_id, "10001");
        assert_eq!(origin.sender_name, "群友");
        assert_eq!(origin.sent_at, "2026-08-10T12:00:00+00:00");
    }

    /// 真实链路实测：cargo test --bin miyu -- --ignored collect_meme_records_origin
    /// 需要 MIYU_E2E_CONFIG_DIR 指向含识图模型配置的真实 config 目录，
    /// MIYU_E2E_IMAGE 指向一张能通过表情判定的图片；数据写入临时目录。
    #[tokio::test]
    #[ignore = "hits the real vision model; needs MIYU_E2E_CONFIG_DIR + MIYU_E2E_IMAGE"]
    async fn collect_meme_records_origin_end_to_end() {
        let config_dir = PathBuf::from(std::env::var("MIYU_E2E_CONFIG_DIR").unwrap());
        let image = PathBuf::from(std::env::var("MIYU_E2E_IMAGE").unwrap());
        let temp = tempfile::tempdir().unwrap();
        let paths = MiyuPaths {
            root_dir: config_dir.clone(),
            config_dir: config_dir.clone(),
            config_file: config_dir.join("config.jsonc"),
            skills_dir: config_dir.join("skills"),
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
            state_dir: temp.path().join("state"),
            pictures_dir: temp.path().join("pictures"),
            fish_hook_file: temp.path().join("fish/miyu.fish"),
            bash_hook_file: temp.path().join("shell/bash-hook.sh"),
            zsh_hook_file: temp.path().join("shell/zsh-hook.zsh"),
            scripts_dir: config_dir.join("scripts"),
            system_scripts_dir: PathBuf::new(),
        };
        let config = AppConfig::load_or_default(&paths).unwrap();
        let origin = MemeOrigin {
            platform: "onebot".to_string(),
            conversation_kind: "group".to_string(),
            conversation_id: "123456".to_string(),
            sender_id: "10001".to_string(),
            sender_name: "测试群友".to_string(),
            message_id: "msg-e2e-1".to_string(),
            sent_at: "2026-08-10T12:00:00+00:00".to_string(),
            collected_at: String::new(),
        };
        let outcome = collect_meme_from_local_image(&image, &config, &paths, Some(origin))
            .await
            .unwrap();
        let meme = match outcome {
            MemeCollectionOutcome::Accepted { meme } => meme,
            other => panic!("expected acceptance, got {other:?}"),
        };
        let index_path = user_library_dir(&paths, &meme.library).join("index.json");
        let index: MemeIndex =
            serde_json::from_str(&std::fs::read_to_string(index_path).unwrap()).unwrap();
        let saved = index
            .memes
            .iter()
            .find(|item| item.id == meme.id)
            .expect("saved meme in index");
        let origin = saved.origin.as_ref().expect("origin recorded");
        assert_eq!(origin.sender_id, "10001");
        assert_eq!(origin.sender_name, "测试群友");
        assert_eq!(origin.sent_at, "2026-08-10T12:00:00+00:00");
        assert!(!origin.collected_at.is_empty(), "collected_at stamped");
        println!("E2E origin: {}", serde_json::to_string_pretty(origin).unwrap());
    }

    #[test]
    fn strict_schema_rejects_unknown_and_missing_fields() {
        let mut value = serde_json::to_value(classification_json()).unwrap();
        value["extra"] = json!(true);
        assert!(serde_json::from_value::<MemeClassification>(value).is_err());

        let mut missing = classification_json();
        missing.as_object_mut().unwrap().remove("confidence");
        assert!(serde_json::from_value::<MemeClassification>(missing).is_err());

        let mut nested = classification_json();
        nested["name"]["unexpected"] = json!("value");
        assert!(serde_json::from_value::<MemeClassification>(nested).is_err());
    }

    #[test]
    fn classification_enforces_metadata_and_tag_limits() {
        let mut classification = accepted_classification();
        classification.description = "x".repeat(MAX_DESCRIPTION_CHARS + 1);
        assert!(validate_classification(&classification).is_err());

        let mut duplicate_tags = accepted_classification();
        duplicate_tags.tags = vec!["Happy".to_string(), "happy".to_string()];
        assert!(validate_classification(&duplicate_tags).is_err());

        let mut spaced_tag = accepted_classification();
        spaced_tag.tags = vec!["not short".to_string()];
        assert!(validate_classification(&spaced_tag).is_err());
    }

    #[test]
    fn image_validation_uses_content_not_extension() {
        let bytes = png_bytes(64, 48);
        assert_eq!(
            validate_image_bytes(&bytes).unwrap(),
            ValidatedImageFormat::Png
        );
        assert!(validate_image_bytes(b"not an image").is_err());
    }

    #[test]
    fn image_validation_enforces_dimension_bounds() {
        assert!(validate_image_bytes(&png_bytes(31, 64)).is_err());
        assert!(validate_image_bytes(&png_bytes(64, 32)).is_ok());
        assert!(validate_dimensions(4096, 3907).is_err());
    }

    #[test]
    fn gif_validation_enforces_frame_and_duration_limits() {
        assert!(validate_image_bytes(&gif_bytes(2, 100)).is_ok());
        assert!(validate_image_bytes(&gif_bytes(2, 8_000)).is_err());
        assert!(validate_image_bytes(&gif_bytes(MAX_GIF_FRAMES + 1, 1)).is_err());
    }

    #[tokio::test]
    async fn gif_terminal_preview_is_a_static_png() {
        let mut source = tempfile::Builder::new().suffix(".gif").tempfile().unwrap();
        source.write_all(&gif_bytes(2, 100)).unwrap();
        let preview = static_gif_preview(source.path()).await.unwrap();
        let reader = image::ImageReader::open(preview.path())
            .unwrap()
            .with_guessed_format()
            .unwrap();
        assert_eq!(reader.format(), Some(image::ImageFormat::Png));
    }

    #[test]
    fn index_save_replaces_atomically_and_remains_parseable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("library/index.json");
        let mut index = MemeIndex {
            library: "test".to_string(),
            version: 2,
            memes: Vec::new(),
            disabled_ids: Vec::new(),
        };
        save_index(&path, &index).unwrap();
        index.disabled_ids.push("sha256:abc".to_string());
        save_index(&path, &index).unwrap();
        assert_eq!(
            load_index(&path).unwrap().unwrap().disabled_ids,
            index.disabled_ids
        );
    }

    #[test]
    fn matches_full_prefixed_and_short_ids() {
        let id = "sha256:abcdef1234567890";
        assert!(ids_match(id, "sha256:abcdef1234567890"));
        assert!(ids_match(id, "abcdef1234567890"));
        assert!(ids_match(id, "abcdef12"));
        assert!(!ids_match(id, "123456"));
    }

    #[test]
    fn unique_short_id_starts_at_git_style_length() {
        let ids = vec!["sha256:abcdef1234567890".to_string()];

        assert_eq!(
            unique_short_id_from_ids(&ids, "sha256:abcdef1234567890"),
            "abcdef1"
        );
    }

    #[test]
    fn unique_short_id_extends_until_unambiguous() {
        let ids = vec![
            "sha256:abcdef1234567890".to_string(),
            "sha256:abcdef1999999999".to_string(),
        ];

        assert_eq!(
            unique_short_id_from_ids(&ids, "sha256:abcdef1234567890"),
            "abcdef12"
        );
    }

    #[test]
    fn find_meme_rejects_too_short_prefix() {
        let err = find_meme_in(vec![test_loaded_meme("sha256:abcdef1234567890")], "abcdef")
            .unwrap_err()
            .to_string();

        assert!(err.contains("too short"));
    }

    #[test]
    fn find_meme_rejects_ambiguous_prefix() {
        let err = find_meme_in(
            vec![
                test_loaded_meme("sha256:abcdef1234567890"),
                test_loaded_meme("sha256:abcdef1999999999"),
            ],
            "abcdef1",
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("ambiguous"));
    }

    #[test]
    fn find_meme_accepts_unique_short_prefix() {
        let meme = find_meme_in(vec![test_loaded_meme("sha256:abcdef1234567890")], "abcdef1")
            .unwrap()
            .unwrap();

        assert_eq!(meme.item.id, "sha256:abcdef1234567890");
    }

    fn test_loaded_meme(id: &str) -> LoadedMeme {
        LoadedMeme {
            item: MemeItem {
                id: id.to_string(),
                name: LocalizedName {
                    zh: "测试".to_string(),
                    en: "test".to_string(),
                },
                file: "images/test.png".to_string(),
                mime_type: "image/png".to_string(),
                animated: false,
                description: "测试表情".to_string(),
                usage: "测试".to_string(),
                avoid: String::new(),
                tags: Vec::new(),
                origin: None,
            },
            path: PathBuf::from("images/test.png"),
            source: MemeSource::User,
        }
    }

    fn accepted_classification() -> MemeClassification {
        MemeClassification {
            save: true,
            confidence: 100,
            positive_gates: PositiveGates {
                chat_reaction: true,
                emotion_or_meme: true,
                reusable: true,
                context_independent: true,
                persona_fit: true,
                meaning_clear: true,
                visual_quality: true,
            },
            risk_gates: RiskGates {
                ordinary_photo: false,
                informational_content: false,
                privacy: false,
                advertisement: false,
                unsafe_or_abusive: false,
            },
            name: LocalizedName {
                zh: "开心猫".to_string(),
                en: "Happy Cat".to_string(),
            },
            description: "一只卡通猫开心地挥手。".to_string(),
            usage: "适合轻松打招呼。".to_string(),
            avoid: "严肃场景不要使用。".to_string(),
            tags: vec!["开心".to_string(), "猫".to_string()],
        }
    }

    fn classification_json() -> Value {
        json!({
            "save": true,
            "confidence": 100,
            "positive_gates": {
                "chat_reaction": true,
                "emotion_or_meme": true,
                "reusable": true,
                "context_independent": true,
                "persona_fit": true,
                "meaning_clear": true,
                "visual_quality": true
            },
            "risk_gates": {
                "ordinary_photo": false,
                "informational_content": false,
                "privacy": false,
                "advertisement": false,
                "unsafe_or_abusive": false
            },
            "name": { "zh": "开心猫", "en": "Happy Cat" },
            "description": "一只卡通猫开心地挥手。",
            "usage": "适合轻松打招呼。",
            "avoid": "严肃场景不要使用。",
            "tags": ["开心", "猫"]
        })
    }

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, Rgba([20, 40, 60, 255]));
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                image.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        bytes
    }

    fn gif_bytes(frames: usize, delay_ms: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            let frames = (0..frames).map(|_| {
                Frame::from_parts(
                    RgbaImage::from_pixel(32, 32, Rgba([20, 40, 60, 255])),
                    0,
                    0,
                    Delay::from_numer_denom_ms(delay_ms, 1),
                )
            });
            encoder.encode_frames(frames).unwrap();
        }
        bytes
    }
}
