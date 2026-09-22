use super::*;

#[cfg(test)]
mod batch_tests {
    use super::*;

    #[test]
    fn batch_targets_requires_non_empty_array() {
        assert!(batch_targets(&json!({"image": "a.png"})).is_none());
        assert!(batch_targets(&json!({"images": []})).is_none());
        assert!(batch_targets(&json!({"images": ["  "]})).is_none());
        assert_eq!(
            batch_targets(&json!({"images": ["a.png", " b.png "]})).unwrap(),
            vec!["a.png".to_string(), "b.png".to_string()]
        );
    }

    /// 批量输出保序分节;单张失败记 ERROR 不掀整批;prompt 透传给每张。
    #[tokio::test]
    async fn vision_batch_keeps_order_and_isolates_failures() {
        let targets = vec![
            "one.png".to_string(),
            "two.png".to_string(),
            "three.png".to_string(),
        ];
        let output = run_vision_batch(
            targets,
            Some(Value::String("what is it".to_string())),
            |sub| {
                Box::pin(async move {
                    let image = sub["image"].as_str().unwrap().to_string();
                    assert_eq!(sub["prompt"].as_str(), Some("what is it"));
                    if image == "two.png" {
                        bail!("boom")
                    }
                    Ok(format!("desc of {image}"))
                })
            },
        )
        .await
        .unwrap();
        let sections: Vec<&str> = output.split("\n\n").collect();
        assert_eq!(sections.len(), 3);
        assert!(sections[0].starts_with("[Image 1] one.png\ndesc of one.png"));
        assert!(sections[1].starts_with("[Image 2] two.png\nERROR: boom"));
        assert!(sections[2].starts_with("[Image 3] three.png\ndesc of three.png"));
    }
}

#[cfg(test)]
mod inline_batch_tests {
    use super::*;

    fn item(source: &str) -> miyu_core::state::TurnInlineMedia {
        miyu_core::state::TurnInlineMedia {
            call_id: String::new(),
            seq: 0,
            kind: miyu_core::state::INLINE_MEDIA_KIND_IMAGE.to_string(),
            mime: "image/png".to_string(),
            source: source.to_string(),
            data: Some(vec![1, 2, 3]),
        }
    }

    /// 批里两张各自内联寄存、一张旁路转述、一张出错:输出必须是**一条** inline
    /// JSON——媒体两张按序、`analyses` 记另外两张——逐张的寄存要被取走(不泄漏)。
    ///
    /// 退回修复前:输出是 `[Image 1] …\n{"mode":"inline"…}` 的拼接文本,
    /// `take_from_output` 一张都取不到,逐张寄存的 ref 永远留在表里。
    #[tokio::test]
    async fn vision_batch_merges_per_target_inline_deposits_into_one() {
        let deposits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let targets = vec![
            "a.png".to_string(),
            "b.mp4".to_string(),
            "c.png".to_string(),
            "d.png".to_string(),
        ];
        let output = run_vision_batch(targets, None, |sub| {
            let deposits = deposits.clone();
            Box::pin(async move {
                let image = sub["image"].as_str().unwrap().to_string();
                match image.as_str() {
                    "a.png" | "c.png" => {
                        let output = inline::deposit(vec![item(&image)]);
                        deposits.lock().unwrap().push(output.clone());
                        Ok(output)
                    }
                    "b.mp4" => Ok("a red square then a blue one".to_string()),
                    _ => bail!("boom"),
                }
            })
        })
        .await
        .unwrap();
        assert!(inline::inline_reference(&output).is_some(), "{output}");
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["media"].as_array().unwrap().len(), 2);
        let analyses = value["analyses"].as_array().unwrap();
        assert_eq!(analyses.len(), 2, "{output}");
        assert_eq!(analyses[0]["image"], "b.mp4");
        assert_eq!(analyses[0]["analysis"], "a red square then a blue one");
        assert_eq!(analyses[1]["image"], "d.png");
        assert!(analyses[1]["error"].as_str().unwrap().contains("boom"));
        let items = inline::take_from_output(&output);
        assert_eq!(
            items
                .iter()
                .map(|item| item.source.as_str())
                .collect::<Vec<_>>(),
            vec!["a.png", "c.png"]
        );
        // 逐张的寄存已被合并取走,凭旧 ref 什么都拿不到。
        for deposit in deposits.lock().unwrap().iter() {
            assert!(inline::take_from_output(deposit).is_empty());
        }
    }

    /// 没有任何内联时,输出仍是原来的分节文本,一个字节不变。
    #[tokio::test]
    async fn vision_batch_without_inline_keeps_plain_sections() {
        let output = run_vision_batch(vec!["x.png".to_string()], None, |sub| {
            Box::pin(async move { Ok(format!("desc of {}", sub["image"].as_str().unwrap())) })
        })
        .await
        .unwrap();
        assert_eq!(output, "[Image 1] x.png\ndesc of x.png");
        assert!(inline::inline_reference(&output).is_none());
    }
}

#[cfg(test)]
mod video_route_tests {
    use super::*;

    /// 扩展名分流是视频路由的唯一开关:带查询串的 URL、大小写、图片后缀
    /// 都不能误判。
    #[test]
    fn video_mime_detection_covers_url_and_case() {
        assert_eq!(video_mime("/tmp/a.mp4"), Some("video/mp4"));
        assert_eq!(video_mime("/tmp/A.MOV"), Some("video/mov"));
        assert_eq!(
            video_mime("https://x.com/v.webm?sig=abc"),
            Some("video/webm")
        );
        assert_eq!(video_mime("/tmp/a.png"), None);
        assert_eq!(video_mime("https://x.com/v"), None);
        // GLM 官方列的三种格式必须全认(08-27:mkv 原先漏了,会被当图片走)。
        for (path, mime) in [
            ("/tmp/a.mp4", "video/mp4"),
            ("/tmp/a.mkv", "video/x-matroska"),
            ("/tmp/a.mov", "video/mov"),
        ] {
            assert_eq!(video_mime(path), Some(mime), "GLM 支持的格式: {path}");
        }
    }

    /// 体积上限对齐 GLM 官方规格(200MB);卡在旧的 24MB 会把 GLM 能吃的量挡住。
    #[test]
    fn video_size_cap_matches_the_glm_limit() {
        assert_eq!(MAX_VIDEO_BYTES, 200 * 1024 * 1024);
    }

    /// wire 形态锁定:GLM 官方文档与 OpenRouter/Qwen 系一致,都是
    /// {"type":"video_url","video_url":{"url":…}}(08-27 对过官方 API 文档)。
    #[test]
    fn video_part_serializes_to_openrouter_shape() {
        let message =
            miyu_core::llm::ChatMessage::user_with_video("看看这段", "data:video/mp4;base64,AAAA");
        let json = serde_json::to_value(&message).unwrap();
        let parts = json["content"].as_array().unwrap();
        assert_eq!(parts[1]["type"], "video_url");
        assert_eq!(parts[1]["video_url"]["url"], "data:video/mp4;base64,AAAA");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::BoxFuture;
    use miyu_base::config::ActiveProviderModelConfig;
    use miyu_base::platform_types::PlatformPrincipal;

    /// 只实现工具层认的窄 trait(`PlatformToolContext`),不借真的
    /// `PlatformTurnContext`——那会把平台运行时整个钉进工具层的测试。
    /// `PlatformTurnContext::message_images_task` 本身只是转发给适配器,
    /// 「按消息取图 + 计数」直接放进假对象,覆盖面不变。
    struct ContextImageAdapter {
        calls: Arc<AtomicUsize>,
        images: Vec<PlatformImageData>,
    }

    impl PlatformToolContext for ContextImageAdapter {
        fn principal(&self) -> PlatformPrincipal {
            PlatformPrincipal {
                platform: "onebot".to_string(),
                account_id: "10000".to_string(),
                user_id: "30000".to_string(),
            }
        }

        fn is_admin(&self) -> bool {
            false
        }

        fn sender_display_name(&self) -> String {
            "tester".to_string()
        }

        fn host_tools_allowed(&self) -> bool {
            false
        }

        fn message_images_task(
            &self,
            _message_id: String,
        ) -> BoxFuture<'static, Result<Vec<PlatformImageData>>> {
            let calls = self.calls.clone();
            let images = self.images.clone();
            Box::pin(async move {
                tokio::task::yield_now().await;
                calls.fetch_add(1, Ordering::AcqRel);
                Ok(images)
            })
        }

        fn fetch_platform_file_task(
            &self,
            _file_ref: PlatformContextFileRef,
        ) -> BoxFuture<'static, Result<PlatformFileDownload>> {
            Box::pin(async { bail!("files are not used in this test") })
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
        let paths = miyu_base::paths::MiyuPaths {
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
            .find(|provider| !provider.is_builtin_cli_provider())
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
                    .find(|provider| !provider.is_builtin_cli_provider())
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
        let mut vision = miyu_base::config::VisionPluginConfig::default();
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
        let mut vision = miyu_base::config::VisionPluginConfig::default();
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
        let context: Arc<dyn PlatformToolContext> = Arc::new(ContextImageAdapter {
            calls: calls.clone(),
            images: vec![PlatformImageData {
                mime: "image/png".to_string(),
                data: Arc::from(vec![1_u8, 2, 3]),
            }],
        });
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
            context_files: HashMap::new(),
            platform_context: Some(context),
            allow_general_access: false,
            resolve_lock: tokio::sync::Mutex::new(()),
            resolved: Mutex::new(HashMap::new()),
            resolved_files: Mutex::new(HashMap::new()),
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
    /// QQ 线一条消息带两张图、模型一次 `images` 全交:两张各自寄存的内联媒体要
    /// 合成一次寄存,而且整批只计一次 vision_analyze 调用。
    ///
    /// 退回修复前:输出是拼接文本,`take_from_output` 取不到图;`calls` 记 2。
    #[tokio::test]
    async fn scoped_batch_attaches_every_image_and_counts_one_call() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let mut config = AppConfig::default();
        let provider = config
            .providers
            .iter_mut()
            .find(|provider| !provider.is_builtin_cli_provider())
            .unwrap();
        provider.model_modalities.insert(
            provider.default_model.clone(),
            vec!["text".to_string(), "image".to_string()],
        );
        let mut targets = Vec::new();
        for name in ["a.png", "b.png"] {
            let path = temp.path().join(name);
            image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
                .save(&path)
                .unwrap();
            targets.push(path.canonicalize().unwrap());
        }
        let state = Arc::new(ScopedVisionState {
            allowed_paths: targets.clone(),
            context_images: HashMap::new(),
            context_files: HashMap::new(),
            platform_context: None,
            allow_general_access: false,
            resolve_lock: tokio::sync::Mutex::new(()),
            resolved: Mutex::new(HashMap::new()),
            resolved_files: Mutex::new(HashMap::new()),
            content_images: Mutex::new(HashMap::new()),
            analyses: Mutex::new(HashMap::new()),
            calls: AtomicUsize::new(0),
            fetches: AtomicUsize::new(0),
            total_bytes: AtomicUsize::new(0),
        });
        let wanted = targets
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        let output =
            analyze_scoped_image(json!({ "images": wanted }), config, paths, state.clone())
                .await
                .unwrap();
        assert!(inline::inline_reference(&output).is_some(), "{output}");
        assert!(!output.contains("base64"));
        let items = inline::take_from_output(&output);
        assert_eq!(
            items
                .iter()
                .map(|item| item.source.as_str())
                .collect::<Vec<_>>(),
            wanted
        );
        assert_eq!(state.calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn inline_short_circuit_only_when_the_text_pool_can_see() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("dot.png");
        image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
            .save(&image_path)
            .unwrap();
        let target = image_path.display().to_string();
        let mut config = AppConfig::default();
        // 池不认图片:退回旁路。
        assert!(try_inline_targets(&config, &[target.clone()])
            .unwrap()
            .is_none());
        let provider = config
            .providers
            .iter_mut()
            .find(|provider| !provider.is_builtin_cli_provider())
            .unwrap();
        provider.model_modalities.insert(
            provider.default_model.clone(),
            vec!["text".to_string(), "image".to_string()],
        );
        // 池认图片:寄存并返回 inline 标记,媒体本体不在结果文本里。
        let output = try_inline_targets(&config, &[target.clone()])
            .unwrap()
            .expect("inline output");
        assert!(inline::inline_reference(&output).is_some());
        assert!(!output.contains("base64"));
        let items = inline::take_from_output(&output);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, miyu_core::state::INLINE_MEDIA_KIND_IMAGE);
        assert_eq!(items[0].mime, "image/png");
        assert_eq!(
            items[0].data.as_deref(),
            Some(std::fs::read(&image_path).unwrap().as_slice())
        );
        // 视频而池不认视频:整批退回旁路,不拆一半给主模型。
        assert!(
            try_inline_targets(&config, &[target, "https://x/y.mp4".to_string()])
                .unwrap()
                .is_none()
        );
        // 关掉偏好开关:一律旁路。
        config.plugins.vision.prefer_current_multimodal_model = false;
        assert!(
            try_inline_targets(&config, &[image_path.display().to_string()])
                .unwrap()
                .is_none()
        );
    }
}
