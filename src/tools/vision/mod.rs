mod print;
mod reference;
pub(crate) use print::*;
pub(crate) use reference::*;

use super::{ToolRegistry, ToolSpec};
use crate::clipboard::write_image_cache_file;
use crate::config::{AppConfig, PrintImagePluginConfig};
use crate::llm::{ChatMessage, OpenAiCompatibleClient};
use crate::paths::MiyuPaths;
use crate::platform_types::{PlatformContextImageRef, PlatformImageData};
// 工具层只认这个 trait：主体身份、管理员标志、宿主工具放行、按消息取图。
// 依赖 PlatformTurnContext 本身等于把整个平台运行时钉进工具层。
use crate::platform_types::PlatformToolContext;
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;

pub fn register(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    register_analyze: bool,
) {
    if !register_analyze {
        return;
    }
    registry.register(ToolSpec::new(
        "vision_analyze",
        "Analyze an image using the current multimodal model or a configured vision provider. Supports local image paths and http(s) image URLs.",
        json!({
            "type": "object",
            "properties": {
                "image": { "type": "string", "description": "Local image path or http(s) image URL." },
                "prompt": { "type": "string", "description": "Question or instruction for image analysis. Defaults to a concise description." }
            },
            "required": ["image"],
            "additionalProperties": false
        }),
        move |args| {
            let config = config.clone();
            let paths = paths.clone();
            async move { analyze_image(args, config, paths).await }
        },
    ));
}

pub fn register_scoped_local(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    allowed_images: Vec<PathBuf>,
) {
    register_scoped(
        registry,
        config,
        paths,
        allowed_images,
        Vec::new(),
        None,
        false,
    );
}

pub fn register_scoped_platform(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    allowed_images: Vec<PathBuf>,
    context_images: Vec<PlatformContextImageRef>,
    platform_context: Arc<dyn PlatformToolContext>,
) {
    let allow_general_access = platform_context.host_tools_allowed();
    register_scoped(
        registry,
        config,
        paths,
        allowed_images,
        context_images,
        Some(platform_context),
        allow_general_access,
    );
}

fn register_scoped(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    allowed_images: Vec<PathBuf>,
    context_images: Vec<PlatformContextImageRef>,
    platform_context: Option<Arc<dyn PlatformToolContext>>,
    allow_general_access: bool,
) {
    let allowed_paths = allowed_images
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect::<Vec<_>>();
    let context_images = context_images
        .into_iter()
        .map(|image| (image.id.clone(), image))
        .collect::<HashMap<_, _>>();
    // Register even with an empty scope: keeping the tool pinned keeps the
    // provider-visible tools array byte-stable across turns (cache prefix).
    // Analysis calls against an empty scope fail with the existing clear
    // "not attached to the current platform turn" style errors.
    let state = Arc::new(ScopedVisionState {
        allowed_paths,
        context_images,
        platform_context,
        allow_general_access,
        resolve_lock: tokio::sync::Mutex::new(()),
        resolved: Mutex::new(HashMap::new()),
        content_images: Mutex::new(HashMap::new()),
        analyses: Mutex::new(HashMap::new()),
        calls: AtomicUsize::new(0),
        fetches: AtomicUsize::new(0),
        total_bytes: AtomicUsize::new(0),
    });
    // 生图的参考图与看图共用同一份作用域:两者都会把图片原样送到第三方,
    // 信任面必须一致(08-17)。只在插件启用时接管,否则保持工具不存在。
    if config.plugins.image_generation.enabled {
        super::image_generation::register_scoped(
            registry,
            config.clone(),
            ReferenceResolver {
                config: config.clone(),
                paths: paths.clone(),
                state: Some(state.clone()),
            },
            state.platform_context.is_some(),
        );
    }
    if !config.plugins.vision.enabled {
        // 只为生图的参考图建作用域:看图插件关着就不注册 vision_analyze。
        return;
    }
    registry.register(ToolSpec::new(
        "vision_analyze",
        "Analyze an image. image can be an image path from this turn's prompt or context_image_N; historical context images are fetched on demand.",
        json!({
            "type": "object",
            "properties": {
                "image": { "type": "string", "description": "A path listed in this turn's image prompt, or a historical image ID such as context_image_1." },
                "prompt": { "type": "string", "description": "Question or instruction for the image analysis. Defaults to a concise description." }
            },
            "required": ["image"],
            "additionalProperties": false
        }),
        move |args| {
            let config = config.clone();
            let paths = paths.clone();
            let state = state.clone();
            async move { analyze_scoped_image(args, config, paths, state).await }
        },
    ));
    registry.amend_description(
        "vision_analyze",
        if allow_general_access {
            " Historical image IDs from this turn (context_image_N) are fetched on demand; plain local paths and URLs still work as well."
        } else {
            " Only these images may be analyzed: this turn's paths from the current or quoted message, context_image_N IDs explicitly listed in earlier group-chat history, or avatar_url links returned by the group query tools. No other paths or URLs are allowed."
        },
    );
}

async fn analyze_image(args: Value, config: AppConfig, paths: MiyuPaths) -> Result<String> {
    let vision = &config.plugins.vision;
    if !vision.enabled {
        bail!("vision plugin is disabled")
    }
    let image = args
        .get("image")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if image.is_empty() {
        bail!("image is required")
    }
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Describe this image concisely and point out the important details.")
        .trim();
    let image_url = if image.starts_with("http://") || image.starts_with("https://") {
        image.to_string()
    } else {
        local_image_data_url(image)?
    };
    analyze_image_url_with_prompt(&config, &paths, &image_url, prompt).await
}

async fn analyze_scoped_image(
    args: Value,
    config: AppConfig,
    paths: MiyuPaths,
    state: Arc<ScopedVisionState>,
) -> Result<String> {
    let image = args
        .get("image")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if image.is_empty() {
        bail!("image is required")
    }
    if state.calls.fetch_add(1, Ordering::AcqRel) >= MAX_SCOPED_VISION_CALLS {
        bail!("vision_analyze call limit reached for the current platform turn")
    }
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Describe this image concisely and point out the important details.")
        .trim();
    if state.context_images.contains_key(image) {
        let resolved = resolve_context_image(&paths, &state, image).await?;
        let cache_key = (resolved.digest.clone(), prompt.to_string());
        if let Some(cached) = state.analyses.lock().unwrap().get(&cache_key).cloned() {
            return Ok(cached);
        }
        let image_url = image_data_url(&resolved.image.mime, &resolved.image.data);
        let result = analyze_image_url_with_prompt(&config, &paths, &image_url, prompt).await?;
        state
            .analyses
            .lock()
            .unwrap()
            .insert(cache_key, result.clone());
        return Ok(result);
    }
    if state.allow_general_access {
        return analyze_image(args, config, paths).await;
    }
    if image.starts_with("http://") || image.starts_with("https://") {
        // QQ avatar URLs are built by our own tools from numeric IDs
        // (fixed host, digits-only parameters), so admitting them opens
        // no injection or exfiltration surface.
        if crate::platform_types::is_trusted_avatar_url(image) {
            return analyze_image(args, config, paths).await;
        }
        bail!("only images attached to the current platform turn are allowed")
    }
    let image = expand_path(image)
        .canonicalize()
        .context("failed to resolve the requested image")?;
    if !state.allowed_paths.iter().any(|allowed| allowed == &image) {
        bail!("image is not attached to the current platform turn")
    }
    analyze_local_image_with_prompt(&config, &paths, &image, prompt).await
}

pub async fn analyze_local_image_with_prompt(
    config: &AppConfig,
    paths: &MiyuPaths,
    image: &Path,
    prompt: &str,
) -> Result<String> {
    let image_url = local_image_data_url(&image.display().to_string())?;
    analyze_image_url_with_prompt(config, paths, &image_url, prompt).await
}

pub async fn analyze_image_url_with_prompt(
    config: &AppConfig,
    paths: &MiyuPaths,
    image_url: &str,
    prompt: &str,
) -> Result<String> {
    let vision = &config.plugins.vision;
    if !vision.enabled {
        bail!("vision plugin is disabled")
    }
    let client = vision_client(config, paths)?.with_request_timeouts(
        Duration::from_secs(vision.response_header_timeout_seconds.max(1)),
        Duration::from_secs(vision.stream_idle_timeout_seconds.max(1)),
    );
    let endpoint_count = client.endpoint_count();
    let request = client.chat_stream(
        vec![
            ChatMessage::system("Answer based on the image content; do not make up details you cannot see."),
            ChatMessage::user_with_image(prompt, image_url.to_string()),
        ],
        Vec::new(),
        |_| Ok(()),
    );
    let result = with_image_timeout(vision_pool_timeout(vision, endpoint_count), request).await?;
    if result.content.trim().is_empty() {
        bail!("vision model returned empty response")
    }
    Ok(result.content)
}

/// 罩在整条故障转移外面的总预算。
///
/// `image_timeout_seconds` 原本是个固定的总超时，可它跟端点数无关。08-18 实测：
/// 60s 预算 / 单端点 15s 响应头超时 = 最多容得下 4 个卡住的端点，排在后面的哪怕
/// 能用也永远轮不到；而且报错会从「5 个端点各自为什么失败」退化成一句
/// 「pool timed out」，把定位所需的信息全丢掉。
///
/// 所以总预算不能小于「所有端点各自超时之和」：按单端点超时 × 端点数兜底，
/// 配置里的值只当下限。单端点那两个超时仍然是真正的保护，一个卡住的端点最多
/// 拖 `response_header_timeout_seconds`。
pub(crate) fn vision_pool_timeout(
    vision: &crate::config::VisionPluginConfig,
    endpoints: usize,
) -> u64 {
    let worst_case = vision
        .response_header_timeout_seconds
        .max(1)
        .saturating_mul(endpoints.max(1) as u64)
        .saturating_add(vision.stream_idle_timeout_seconds);
    vision.image_timeout_seconds.max(worst_case)
}

pub(crate) async fn with_image_timeout<T, F>(timeout_seconds: u64, future: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let timeout = Duration::from_secs(timeout_seconds.max(1));
    tokio::time::timeout(timeout, future).await.map_err(|_| {
        anyhow::anyhow!(
            "vision model pool timed out after {} seconds",
            timeout.as_secs()
        )
    })?
}

/// 当前文本模型池自己就能看图时,`vision_analyze` 直接用它。
///
/// `prefer_current_multimodal_model` 此前只管一件事:粘贴进来的图片要不要
/// 内联发给聊天模型。`vision_analyze` 完全没看这个开关——哪怕当前文本模型
/// 自带眼睛,工具照旧把图发给另配的多模态池,既多一次跨模型往返,答案也来
/// 自一个没有对话上下文的模型(08-17 用户报的问题)。
///
/// 要求整池都支持图片输入:池是负载均衡的,只要有一个端点不认图片,这一路
/// 就可能随机落到它头上。
fn active_text_pool_for_vision(
    config: &AppConfig,
) -> Option<Vec<crate::config::ProviderModelChoice>> {
    if !config.plugins.vision.prefer_current_multimodal_model {
        return None;
    }
    let pool = config.active_provider_model_choices();
    let usable = !pool.is_empty()
        && pool.iter().all(|choice| {
            config.model_supports_any_input(&choice.provider_id, &choice.model, &["image"])
        });
    usable.then_some(pool)
}

fn vision_client(config: &AppConfig, paths: &MiyuPaths) -> Result<OpenAiCompatibleClient> {
    // An explicit global vision provider preserves its existing precedence.
    // Platform turns with a conversation override clear that single-provider
    // field in their private config clone, exposing the full routed pool here.
    if config.plugins.vision.vision_provider_id.trim().is_empty() {
        if let Some(text_pool) = active_text_pool_for_vision(config) {
            return OpenAiCompatibleClient::from_choices(config, paths, &text_pool)
                .map(|client| client.with_request_scope("vision"));
        }
        let choices = config
            .active_multimodal_provider_model_choices()
            .into_iter()
            .filter(|choice| {
                config.model_supports_any_input(&choice.provider_id, &choice.model, &["image"])
            })
            .collect::<Vec<_>>();
        if !choices.is_empty() {
            return OpenAiCompatibleClient::from_choices(config, paths, &choices)
                .map(|client| client.with_request_scope("vision"));
        }
    }
    let (provider_id, model) = config.vision_provider_choice()?;
    let mut provider = config.provider(Some(&provider_id))?.clone();
    provider.default_model = model;
    if !provider
        .models
        .iter()
        .any(|item| item == &provider.default_model)
    {
        provider.models.push(provider.default_model.clone());
    }
    OpenAiCompatibleClient::new(&provider, config, paths)
}

fn local_image_data_url(value: &str) -> Result<String> {
    let path = expand_path(value);
    let metadata = std::fs::metadata(&path)
        .with_context(|| format!("failed to stat image {}", path.display()))?;
    if !metadata.is_file() {
        bail!("image path is not a file: {}", path.display())
    }
    if metadata.len() as usize > MAX_IMAGE_BYTES {
        bail!("image too large: {} bytes", metadata.len())
    }
    let bytes =
        std::fs::read(&path).with_context(|| format!("failed to read image {}", path.display()))?;
    let mime = mime_from_path(&path)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

fn expand_path(value: &str) -> PathBuf {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = crate::platform_dirs::PlatformDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
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

fn mime_from_path(path: &Path) -> Result<&'static str> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => Ok("image/jpeg"),
        "png" => Ok("image/png"),
        "webp" => Ok("image/webp"),
        "gif" => Ok("image/gif"),
        value => {
            bail!("unsupported image extension: {value}; supported: jpg, jpeg, png, webp, gif")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ActiveProviderModelConfig;
    use crate::platforms::{
        ConversationKind, OutboundMessage, PlatformAdapter, PlatformConversation, SendReceipt,
    };
    use crate::state::StateStore;
    use futures_util::future::BoxFuture;

    struct ContextImageAdapter {
        calls: Arc<AtomicUsize>,
        images: Vec<PlatformImageData>,
    }

    impl PlatformAdapter for ContextImageAdapter {
        fn send<'a>(&'a self, _message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
            Box::pin(async { bail!("send is not used in this test") })
        }

        fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
            Box::pin(async { Ok("Miyu".to_string()) })
        }

        fn message_images<'a>(
            &'a self,
            _message_id: &'a str,
        ) -> BoxFuture<'a, Result<Vec<PlatformImageData>>> {
            let calls = self.calls.clone();
            let images = self.images.clone();
            Box::pin(async move {
                tokio::task::yield_now().await;
                calls.fetch_add(1, Ordering::AcqRel);
                Ok(images)
            })
        }
    }

    fn test_paths(root: &Path) -> MiyuPaths {
        MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("fish"),
            bash_hook_file: root.join("bash"),
            zsh_hook_file: root.join("zsh"),
            scripts_dir: root.join("scripts"),
            system_scripts_dir: root.join("system-scripts"),
        }
    }

    /// 平台回合的作用域不能只由看图插件把门:生图的参考图共用同一份作用域,
    /// vision 关、生图开时若不建作用域,generate_image 会留着不受限的解析器。
    #[test]
    fn scoped_registration_binds_image_generation_even_without_vision() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::paths::MiyuPaths {
            root_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            config_file: temp.path().join("config/config.jsonc"),
            skills_dir: temp.path().join("config/skills"),
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
            state_dir: temp.path().join("state"),
            pictures_dir: temp.path().join("pictures"),
            fish_hook_file: temp.path().join("fish"),
            bash_hook_file: temp.path().join("bash"),
            zsh_hook_file: temp.path().join("zsh"),
            scripts_dir: temp.path().join("config/scripts"),
            system_scripts_dir: temp.path().join("system-scripts"),
        };
        let mut config = AppConfig::default();
        config.plugins.vision.enabled = false;
        config.plugins.image_generation.enabled = true;

        let mut registry = ToolRegistry::new();
        register_scoped_local(&mut registry, config, paths, Vec::new());
        // 看图插件关着 ⇒ 不注册 vision_analyze,但生图必须换成带作用域的版本。
        assert!(!registry.contains("vision_analyze"));
        assert!(registry.contains("generate_image"));
    }

    /// 当前文本模型自己能看图时就用它,不再绕道另配的多模态池。
    #[test]
    fn vision_uses_the_active_text_pool_when_it_can_see() {
        let mut config = AppConfig::default();
        let provider = config
        .providers
        .iter_mut()
        .find(|provider| !provider.is_claude_code())
        .unwrap();
        let provider_id = provider.id.clone();
        provider.model_modalities.insert(
            provider.default_model.clone(),
            vec!["text".to_string(), "image".to_string()],
        );
        provider
            .model_modalities
            .insert("blind-model".to_string(), vec!["text".to_string()]);
        provider.models.push("blind-model".to_string());
        assert!(active_text_pool_for_vision(&config).is_some());

        // 开关关掉就走原路。
        config.plugins.vision.prefer_current_multimodal_model = false;
        assert!(active_text_pool_for_vision(&config).is_none());
        config.plugins.vision.prefer_current_multimodal_model = true;

        // 池里只要混进一个不认图片的端点就不能用:负载均衡会随机落到它。
        config.active_provider_models = Some(vec![
            ActiveProviderModelConfig {
                provider_id: provider_id.clone(),
                model: config
                    .providers
                    .iter()
                    .find(|provider| !provider.is_claude_code())
                    .unwrap()
                    .default_model
                    .clone(),
            },
            ActiveProviderModelConfig {
                provider_id,
                model: "blind-model".to_string(),
            },
        ]);
        assert!(active_text_pool_for_vision(&config).is_none());
    }

    /// 08-18 实测的那次：5 个端点，其中 3 个各卡满 15s，总共 45.9s；固定的
    /// 60s 预算刚好没被撑破。再多一个卡住的端点就会被从中间砍断——排在后面的
    /// 端点哪怕能用也永远轮不到。
    #[test]
    fn the_pool_budget_covers_every_endpoint_timing_out() {
        let mut vision = crate::config::VisionPluginConfig::default();
        vision.response_header_timeout_seconds = 15;
        vision.stream_idle_timeout_seconds = 20;
        vision.image_timeout_seconds = 60;

        // 端点少时，配置里的值仍然说了算
        assert_eq!(vision_pool_timeout(&vision, 1), 60);
        assert_eq!(vision_pool_timeout(&vision, 2), 60);

        // 端点一多，预算跟着涨：5 × 15 + 20 = 95 > 60
        assert_eq!(vision_pool_timeout(&vision, 5), 95);
        // 关键回归：9 个端点全卡住也要够，不能停在 60
        assert_eq!(vision_pool_timeout(&vision, 9), 155);
        assert!(
            vision_pool_timeout(&vision, 9) >= vision.response_header_timeout_seconds * 9,
            "预算必须罩得住每个端点各自超时一次"
        );
    }

    /// 端点数为 0（不该发生）也不能算出 0 秒预算。
    #[test]
    fn the_pool_budget_is_never_zero() {
        let mut vision = crate::config::VisionPluginConfig::default();
        vision.response_header_timeout_seconds = 0;
        vision.stream_idle_timeout_seconds = 0;
        vision.image_timeout_seconds = 0;
        assert!(vision_pool_timeout(&vision, 0) >= 1);
    }

    #[tokio::test]
    async fn image_timeout_cancels_a_stalled_model_pool() {
        let error = with_image_timeout(1, std::future::pending::<Result<()>>())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "vision model pool timed out after 1 seconds"
        );
    }

    #[tokio::test]
    async fn context_images_reuse_resolved_ids_and_duplicate_content_cache() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = Arc::new(ContextImageAdapter {
            calls: calls.clone(),
            images: vec![PlatformImageData {
                mime: "image/png".to_string(),
                data: Arc::from(vec![1_u8, 2, 3]),
            }],
        });
        let context = Arc::new(crate::platforms::PlatformTurnContext::new(
            PlatformConversation {
                platform: "onebot".to_string(),
                account_id: "10000".to_string(),
                kind: ConversationKind::Group,
                conversation_id: "20000".to_string(),
            },
            "30000".to_string(),
            "tester".to_string(),
            false,
            AppConfig::default(),
            paths.clone(),
            StateStore::new(&paths).unwrap(),
            adapter,
            Arc::new(crate::platforms::plugins::PlatformPluginRegistry::default()),
        ));
        let source = PlatformContextImageRef {
            id: "context_image_1".to_string(),
            message_id: "90".to_string(),
            image_index: 1,
        };
        let duplicate_source = PlatformContextImageRef {
            id: "context_image_2".to_string(),
            message_id: "91".to_string(),
            image_index: 1,
        };
        let state = ScopedVisionState {
            allowed_paths: Vec::new(),
            context_images: [
                (source.id.clone(), source),
                (duplicate_source.id.clone(), duplicate_source),
            ]
            .into(),
            platform_context: Some(context),
            allow_general_access: false,
            resolve_lock: tokio::sync::Mutex::new(()),
            resolved: Mutex::new(HashMap::new()),
            content_images: Mutex::new(HashMap::new()),
            analyses: Mutex::new(HashMap::new()),
            calls: AtomicUsize::new(0),
            fetches: AtomicUsize::new(0),
            total_bytes: AtomicUsize::new(0),
        };

        let (first, second) = tokio::join!(
            resolve_context_image(&paths, &state, "context_image_1"),
            resolve_context_image(&paths, &state, "context_image_1")
        );
        let first = first.unwrap();
        let second = second.unwrap();
        let duplicate = resolve_context_image(&paths, &state, "context_image_2")
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::Acquire), 2);
        assert_eq!(first.digest, second.digest);
        assert_eq!(first.cache_path, second.cache_path);
        assert_eq!(first.cache_path, duplicate.cache_path);
        assert_eq!(state.total_bytes.load(Ordering::Acquire), 3);
        assert!(first.cache_path.is_file());
        let error = resolve_context_image(&paths, &state, "context_image_999")
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("context image ID is not available"));
        assert_eq!(calls.load(Ordering::Acquire), 2);
    }
}
