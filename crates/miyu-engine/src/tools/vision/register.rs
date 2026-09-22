use super::*;

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
    context_files: Vec<PlatformContextFileRef>,
    platform_context: Arc<dyn PlatformToolContext>,
) {
    let allow_general_access = platform_context.host_tools_allowed();
    register_scoped(
        registry,
        config,
        paths,
        allowed_images,
        context_images,
        context_files,
        Some(platform_context),
        allow_general_access,
    );
}

#[allow(clippy::too_many_arguments)]
fn register_scoped(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    allowed_images: Vec<PathBuf>,
    context_images: Vec<PlatformContextImageRef>,
    context_files: Vec<PlatformContextFileRef>,
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
    let context_files = context_files
        .into_iter()
        .map(|file| (file.id.clone(), file))
        .collect::<HashMap<_, _>>();
    // Register even with an empty scope: keeping the tool pinned keeps the
    // provider-visible tools array byte-stable across turns (cache prefix).
    // Analysis calls against an empty scope fail with the existing clear
    // "not attached to the current platform turn" style errors.
    let state = Arc::new(ScopedVisionState {
        allowed_paths,
        context_images,
        context_files,
        platform_context,
        allow_general_access,
        resolve_lock: tokio::sync::Mutex::new(()),
        resolved: Mutex::new(HashMap::new()),
        resolved_files: Mutex::new(HashMap::new()),
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
    let native_viewer = active_pool_views_media_natively(&config);
    registry.register(ToolSpec::new(
        "vision_analyze",
        "Analyze an image or a video. image can be an image path from this turn's prompt, context_image_N, or a file_<message_id>_<n> id from chat history (videos and image files shared in the chat); context media is fetched on demand.",
        json!({
            "type": "object",
            "properties": {
                "image": { "type": "string", "description": "A path listed in this turn's image prompt, a historical image ID such as context_image_1, or a file id such as file_<message_id>_1 for a video or image file from the chat." },
                "images": { "type": "array", "items": { "type": "string" }, "description": "Several images to analyze in one call. Overrides image. Videos are analyzed one at a time — pass a single video through `image`." },
                "prompt": { "type": "string", "description": "Question or instruction for the image analysis. Defaults to a concise description." }
            },
            "required": [],
            "additionalProperties": false
        }),
        move |args| {
            let config = config.clone();
            let paths = paths.clone();
            let state = state.clone();
            async move { analyze_scoped_image(args, config, paths, state).await }
        },
    ));
    if native_viewer {
        registry.amend_description(
            "vision_analyze",
            " On this relay the tool does not analyze anything: it fetches the referenced media into a local file and returns the absolute path for you to open with view_file.",
        );
    }
    registry.amend_description(
        "vision_analyze",
        if allow_general_access {
            " Historical image IDs from this turn (context_image_N) and file ids (file_<message_id>_<n>, for videos or image files) are fetched on demand; plain local paths and URLs still work as well."
        } else {
            " Only these may be analyzed: this turn's paths from the current or quoted message, context_image_N IDs explicitly listed in earlier group-chat history, file_<message_id>_<n> ids for videos or image files listed in the chat, or avatar_url links returned by the group query tools. No other paths or URLs are allowed."
        },
    );
}
