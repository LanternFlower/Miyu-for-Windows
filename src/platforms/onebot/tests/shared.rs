//! onebot 测试共用的 fixture。
//!
//! 建临时目录、起假连接、造事件——每个测试文件都要，放一份。

use crate::platforms::onebot::*;
use crate::paths::MiyuPaths;

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
        fish_hook_file: root.join("fish"),
        bash_hook_file: root.join("bash"),
        zsh_hook_file: root.join("zsh"),
        scripts_dir: root.join("scripts"),
        system_scripts_dir: root.join("system-scripts"),
    }
}

pub(super) fn test_web_state(root: &std::path::Path, web_port: u16) -> DaemonState {
    DaemonState::for_test(test_paths(root), web_port).unwrap()
}

pub(super) fn config_with(mutate: impl FnOnce(&mut OneBotConfig)) -> OneBotConfig {
    let mut config = OneBotConfig::default();
    mutate(&mut config);
    config
}

pub(super) fn friend_request_event(user_id: i64, flag: &str) -> Value {
    json!({
        "post_type": "request",
        "request_type": "friend",
        "self_id": 10000,
        "user_id": user_id,
        "flag": flag,
    })
}

pub(super) fn group_add_request_event(group_id: i64, user_id: i64, flag: &str) -> Value {
    json!({
        "post_type": "request",
        "request_type": "group",
        "sub_type": "add",
        "self_id": 10000,
        "group_id": group_id,
        "user_id": user_id,
        "comment": "申请加入",
        "flag": flag,
    })
}

pub(super) fn test_connection(
    asset_base_url: Option<String>,
) -> (ConnectionHandle, mpsc::UnboundedReceiver<String>) {
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let (shutdown, _shutdown_rx) = watch::channel(false);
    (
        ConnectionHandle {
            out_tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
            bot_name: Arc::new(Mutex::new(None)),
            asset_base_url,
            assets: crate::platforms::assets::AssetLeaseStore::new(),
            shutdown,
        },
        out_rx,
    )
}

pub(super) fn test_adapter(handle: ConnectionHandle, target: Target) -> OneBotAdapter {
    let mut registry = ConnectionRegistry::default();
    registry.register(10000, handle.clone());
    OneBotAdapter {
        conn: handle,
        registry: Arc::new(Mutex::new(registry)),
        http: reqwest::Client::new(),
        self_id: 10000,
        target,
        max_reply_chars: 0,
        file_store_lock: Arc::new(tokio::sync::Mutex::new(())),
    }
}

pub(super) struct BlockingObserverPlugin {
    pub(super) observed: mpsc::UnboundedSender<String>,
    pub(super) release_first: Arc<tokio::sync::Notify>,
}

pub(super) struct BlockingJudgePlugin {
    pub(super) entered: mpsc::UnboundedSender<String>,
    pub(super) barrier: Arc<tokio::sync::Barrier>,
}

impl crate::platforms::plugins::PlatformPlugin for BlockingJudgePlugin {
    fn descriptor(&self) -> crate::platforms::plugins::PluginDescriptor {
        crate::platforms::plugins::PluginDescriptor {
            id: "test_parallel_judge",
            priority: 1,
            default_enabled: true,
        }
    }

    fn decide_trigger<'a>(
        &'a self,
        _context: &'a PlatformTurnContext,
        event: &'a PlatformInboundEvent,
        decision: &'a mut TriggerDecision,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.entered.send(event.message_id.clone()).unwrap();
            self.barrier.wait().await;
            decision.should_reply = false;
            Ok(())
        })
    }
}

impl crate::platforms::plugins::PlatformPlugin for BlockingObserverPlugin {
    fn descriptor(&self) -> crate::platforms::plugins::PluginDescriptor {
        crate::platforms::plugins::PluginDescriptor {
            id: "test_fifo_observer",
            priority: 1,
            default_enabled: true,
        }
    }

    fn observe_inbound<'a>(
        &'a self,
        _context: &'a PlatformTurnContext,
        event: &'a PlatformInboundEvent,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.observed.send(event.message_id.clone()).unwrap();
            if event.message_id == "1" {
                self.release_first.notified().await;
            }
            Ok(())
        })
    }
}
