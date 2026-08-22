//! 视觉能力的判定与图片投递。

use super::shared::*;
use crate::agent::*;
use crate::config::{ActiveProviderModelConfig, AppConfig, ProviderConfig};
use crate::platforms::{ConversationKind, PlatformConversation};
use std::path::PathBuf;
use tokio::net::TcpListener;

#[test]
fn vision_support_requires_every_effective_text_pool_model() {
    let mut config = AppConfig::default();
    let provider = config
        .providers
        .iter_mut()
        .find(|provider| !provider.is_claude_code())
        .unwrap();
    provider.default_model = "vision-model".to_string();
    provider.models = vec!["vision-model".to_string(), "text-model".to_string()];
    provider.model_modalities.insert(
        "vision-model".to_string(),
        vec!["text".to_string(), "image".to_string()],
    );
    provider
        .model_modalities
        .insert("text-model".to_string(), vec!["text".to_string()]);
    let provider_id = provider.id.clone();

    config.active_provider_models = Some(vec![ActiveProviderModelConfig {
        provider_id: provider_id.clone(),
        model: "vision-model".to_string(),
    }]);
    assert!(active_text_pool_supports_vision(&config));

    config
        .active_provider_models
        .as_mut()
        .unwrap()
        .push(ActiveProviderModelConfig {
            provider_id,
            model: "text-model".to_string(),
        });
    assert!(!active_text_pool_supports_vision(&config));
}

#[test]
fn vision_preference_controls_direct_image_delivery_to_the_text_pool() {
    let mut config = AppConfig::default();
    let provider = config
        .providers
        .iter_mut()
        .find(|provider| !provider.is_claude_code())
        .unwrap();
    provider.model_modalities.insert(
        provider.default_model.clone(),
        vec!["text".to_string(), "image".to_string()],
    );

    assert!(should_use_active_text_pool_for_images(&config));
    config.plugins.vision.prefer_current_multimodal_model = false;
    assert!(!should_use_active_text_pool_for_images(&config));
}

#[tokio::test]
async fn platform_images_register_a_turn_scoped_vision_tool() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
    agent.set_image_platform("qq", "QQ");
    let images = vec![Some(PastedImage::Binary(ClipboardImage::new(
        "image/png".to_string(),
        vec![1, 2, 3],
    )))];

    let prepared = agent.prepare_user_input("看图", &images).await.unwrap();
    let hint = format!("{:?}", prepared.hints);
    assert!(hint.contains("vision_analyze"));
    let tools = agent.tools.lock().unwrap().clone();
    assert!(tools.contains("vision_analyze"));
    let foreign = temp.path().join("foreign-image.png");
    std::fs::write(&foreign, b"not an attached image").unwrap();
    let request = serde_json::json!({ "image": foreign.display().to_string() }).to_string();
    let error = tools.call("vision_analyze", &request).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("image is not attached to the current platform turn"));
}

#[tokio::test]
async fn context_image_ids_register_vision_without_a_current_image() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let mut agent = Agent::new(
        config.clone(),
        &paths,
        state,
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
    agent.set_image_platform("qq", "QQ");
    let context = Arc::new(PlatformTurnContext::new(
        PlatformConversation {
            platform: "onebot".to_string(),
            account_id: "10000".to_string(),
            kind: ConversationKind::Group,
            conversation_id: "20000".to_string(),
        },
        "30000".to_string(),
        "tester".to_string(),
        false,
        config,
        paths.clone(),
        StateStore::new(&paths).unwrap(),
        Arc::new(NoopPlatformAdapter),
        Arc::new(crate::platforms::plugins::PlatformPluginRegistry::default()),
    ));
    agent.set_platform_context_images(
        context,
        vec![PlatformContextImageRef {
            id: "context_image_1".to_string(),
            message_id: "90".to_string(),
            image_index: 1,
        }],
    );

    let prepared = agent.prepare_user_input("接着说", &[]).await.unwrap();
    assert!(format!("{:?}", prepared.hints).contains("context_image_1"));
    let tools = agent.tools.lock().unwrap();
    assert!(tools.contains("vision_analyze"));
    let definition = tools
        .definitions()
        .into_iter()
        .find(|definition| definition.function.name == "vision_analyze")
        .unwrap();
    assert!(definition.function.description.contains("context_image_N"));
}

#[tokio::test]
async fn binary_image_reaches_vision_pool_then_text_model() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let vision_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let text_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config =
        queue_test_config(format!("http://{}/v1", text_listener.local_addr().unwrap()));
    config.tools.enabled = false;
    config.plugins.vision.enabled = true;
    config.providers.push(ProviderConfig {
        enabled: true,
        id: "vision-test".to_string(),
        display_name: "Vision Test".to_string(),
        base_url: format!("http://{}/v1", vision_listener.local_addr().unwrap()),
        protocol: "openai-chat".to_string(),
        api_key: Some("test-key".to_string()),
        models: vec!["vision-model".to_string()],
        model_context_window: Default::default(),
        model_temperature: HashMap::new(),
        model_modalities: [(
            "vision-model".to_string(),
            vec!["text".to_string(), "image".to_string()],
        )]
        .into(),
        model_costs: Default::default(),
        default_model: "vision-model".to_string(),
        timeout_seconds: 30,
        temperature: 0.0,
        anthropic_max_tokens: 4096,
        extra_body: None,
    });
    config.active_multimodal_provider_models = Some(vec![ActiveProviderModelConfig {
        provider_id: "vision-test".to_string(),
        model: "vision-model".to_string(),
    }]);

    let (vision_request_tx, vision_request_rx) = oneshot::channel();
    let vision_server = tokio::spawn(async move {
        let (mut stream, _) = vision_listener.accept().await.unwrap();
        let request = read_test_http_request(&mut stream).await;
        let _ = vision_request_tx.send(request);
        write_test_sse(
            &mut stream,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"a red square\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });
    let (text_request_tx, text_request_rx) = oneshot::channel();
    let text_server = tokio::spawn(async move {
        let (mut stream, _) = text_listener.accept().await.unwrap();
        let request = read_test_http_request(&mut stream).await;
        let _ = text_request_tx.send(request);
        write_test_sse(
            &mut stream,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"I can see it.\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let text_provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&text_provider, &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
    let image = PastedImage::Binary(ClipboardImage::new(
        "image/png".to_string(),
        b"qq-image-bytes".to_vec(),
    ));

    let result = agent
        .chat_stream_with_images("What is shown?", &[Some(image)], |_| Ok(()))
        .await
        .unwrap();

    assert_eq!(result.content, "I can see it.");
    let vision_request: Value = serde_json::from_slice(&vision_request_rx.await.unwrap()).unwrap();
    let vision_parts = vision_request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .unwrap()["content"]
        .as_array()
        .unwrap();
    assert!(vision_parts.iter().any(|part| {
        part["type"] == "image_url"
            && part["image_url"]["url"]
                .as_str()
                .is_some_and(|url| url.starts_with("data:image/png;base64,"))
    }));

    let text_request: Value = serde_json::from_slice(&text_request_rx.await.unwrap()).unwrap();
    let serialized = serde_json::to_string(&text_request).unwrap();
    assert!(serialized.contains("What is shown?"));
    assert!(serialized.contains("a red square"));
    vision_server.await.unwrap();
    text_server.await.unwrap();
}

#[test]
fn binary_image_cache_is_isolated_by_platform() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let images = vec![Some(PastedImage::Binary(ClipboardImage::new(
        "image/jpeg".to_string(),
        b"same-image-content".to_vec(),
    )))];

    let platform = resolve_pasted_image_paths(&images, &paths, Some("qq"));
    let platform_path = PathBuf::from(platform[0].as_deref().unwrap());
    assert!(platform_path.starts_with(paths.cache_dir.join("platform_images/qq")));
    assert!(platform_path.is_file());

    let clipboard = resolve_pasted_image_paths(&images, &paths, None);
    let clipboard_path = PathBuf::from(clipboard[0].as_deref().unwrap());
    assert!(clipboard_path.starts_with(paths.cache_dir.join("clipboard_images")));
    assert!(clipboard_path.is_file());
    assert_ne!(platform_path, clipboard_path);
}
