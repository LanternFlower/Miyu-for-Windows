//! agent 测试共用的 fixture：临时目录、假 HTTP/SSE 端、配置构造。

use crate::agent::*;
use futures_util::future::BoxFuture;
use miyu_base::config::{AppConfig, ProviderConfig};
use miyu_base::paths::MiyuPaths;
use miyu_base::platform_types::{
    OutboundMessage, PlatformAdapter, PlatformContextFileRef, PlatformFileDownload,
    PlatformImageData, PlatformPrincipal, PlatformToolContext, SendReceipt,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub(super) async fn read_test_http_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0);
        request.extend_from_slice(&buffer[..read]);
        if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.strip_prefix("content-length: ")
                .or_else(|| line.strip_prefix("Content-Length: "))
        })
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0);
        request.extend_from_slice(&buffer[..read]);
    }
    request[header_end..header_end + content_length].to_vec()
}

pub(super) async fn write_test_sse(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

pub(super) fn queue_test_config(base_url: String) -> AppConfig {
    let mut config = AppConfig {
        active_provider: "queue-test".to_string(),
        active_provider_models: None,
        providers: vec![ProviderConfig {
            enabled: true,
            id: "queue-test".to_string(),
            display_name: "Queue Test".to_string(),
            base_url,
            protocol: "openai-chat".to_string(),
            api_key: Some("test-key".to_string()),
            models: vec!["test-model".to_string()],
            custom_models: Vec::new(),
            model_context_window: Default::default(),
            model_temperature: HashMap::new(),
            model_tools_loading_mode: HashMap::new(),
            model_modalities: Default::default(),
            tool_result_media: None,
            model_costs: Default::default(),
            default_model: "test-model".to_string(),
            timeout_seconds: 30,
            temperature: 0.0,
            anthropic_max_tokens: 4096,
            extra_body: None,
        }],
        ..AppConfig::default()
    };
    config.skills.enabled = false;
    config.memory.enabled = false;
    // 人格提醒会触发一次蒸馏 LLM 调用,与各测试的 mock 应答序列冲突;
    // 测提醒本身的用例再显式打开。
    config.prompt.persona_reminder = false;
    // 自动表情包提醒是**掷骰子**决定的(`memes::auto_meme_reminder` 里
    // `rand::random::<f32>()` 对 `auto_send_probability`,默认 0.05),掷中就往
    // 消息尾部塞一条 system-reminder。
    //
    // 5% 看着小,但 `manual_persona_reminder_overrides_distillation` 断言的正是
    // **最后一条消息**,被顶掉就报红。todolist 把它记成「典型并发时序 flake,
    // 单独跑 8/8 全过」——归因错了,和并发毫无关系:5% 下连过 8 次的概率是
    // 0.95⁸ ≈ 66%,「单独跑没事」只是运气。
    config.plugins.memes.auto_send_enabled = false;
    config.plugins.memes.auto_send_platform_enabled = false;
    config
}

pub(super) fn test_paths(root: &std::path::Path) -> MiyuPaths {
    MiyuPaths {
        root_dir: root.to_path_buf(),
        config_dir: root.join("config"),
        config_file: root.join("config/config.jsonc"),
        skills_dir: root.join("config/skills"),
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        state_dir: root.join("state"),
        pictures_dir: root.join("pictures"),
        fish_hook_file: root.join("fish/miyu.fish"),
        bash_hook_file: root.join("shell/bash-hook.sh"),
        zsh_hook_file: root.join("shell/zsh-hook.zsh"),
        scripts_dir: root.join("config/scripts"),
        system_scripts_dir: PathBuf::new(),
    }
}

pub(super) struct NoopPlatformAdapter;

impl PlatformAdapter for NoopPlatformAdapter {
    fn send<'a>(&'a self, _message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
        Box::pin(async { bail!("send is not used in this test") })
    }

    fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok("Miyu".to_string()) })
    }
}

/// 假的平台回合上下文:agent 生产代码只认 [`PlatformTurn`] 端口,测试用它摆出「这是个 onebot 群回合」,
/// 不碰平台层(拆 crate 后 engine 的测试也不能再 dev-depend hosts:那会让 `PlatformTurn` 出现两份)。
pub(super) struct FakePlatformTurn {
    platform: String,
}

impl FakePlatformTurn {
    pub(super) fn onebot() -> Self {
        Self {
            platform: "onebot".to_string(),
        }
    }
}

impl PlatformToolContext for FakePlatformTurn {
    fn principal(&self) -> PlatformPrincipal {
        PlatformPrincipal {
            platform: self.platform.clone(),
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
        Box::pin(async { Ok(Vec::new()) })
    }

    fn fetch_platform_file_task(
        &self,
        _file_ref: PlatformContextFileRef,
    ) -> BoxFuture<'static, Result<PlatformFileDownload>> {
        Box::pin(async { bail!("no platform files in this test") })
    }
}

impl PlatformTurn for FakePlatformTurn {
    fn platform_name(&self) -> &str {
        &self.platform
    }

    fn take_queued_files(&self, _prompt_id: &str) -> Vec<PlatformContextFileRef> {
        Vec::new()
    }

    fn register_file_reader(
        self: Arc<Self>,
        _registry: &mut crate::tools::ToolRegistry,
        _files: Vec<PlatformContextFileRef>,
    ) {
    }
}
