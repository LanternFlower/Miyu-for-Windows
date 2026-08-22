//! 平台层测试共用的 fixture。

use crate::platforms::*;
use crate::paths::MiyuPaths;
use futures_util::future::BoxFuture;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

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

pub(super) fn test_group_members() -> Vec<PlatformGroupMember> {
    ["20000", "30000", "40000", "50000"]
        .into_iter()
        .map(|user_id| PlatformGroupMember {
            group_id: "20000".to_string(),
            user_id: user_id.to_string(),
            nickname: format!("member-{user_id}"),
            card: String::new(),
            role: "member".to_string(),
            title: String::new(),
            joined_at: 0,
            last_active_at: 0,
        })
        .collect()
}

pub(super) fn test_turn_context(
    fail_first: bool,
) -> (tempfile::TempDir, PlatformTurnContext, Arc<CountingAdapter>) {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let adapter = Arc::new(CountingAdapter {
        calls: AtomicUsize::new(0),
        fail_first,
        messages: Mutex::new(Vec::new()),
        group_members: test_group_members(),
    });
    // Unique conversation per context: the delivered-image ledger is
    // process-global and keyed by conversation, so two test contexts
    // sharing an id would observe each other's deliveries.
    static NEXT_CONVERSATION: AtomicUsize = AtomicUsize::new(0);
    let conversation_id = format!(
        "20000-{}",
        NEXT_CONVERSATION.fetch_add(1, AtomicOrdering::Relaxed)
    );
    let context = PlatformTurnContext::new(
        PlatformConversation {
            platform: "onebot".to_string(),
            account_id: "10000".to_string(),
            kind: ConversationKind::Private,
            conversation_id,
        },
        "20000".to_string(),
        "tester".to_string(),
        false,
        AppConfig::default(),
        paths.clone(),
        StateStore::new(&paths).unwrap(),
        adapter.clone(),
        Arc::new(plugins::PlatformPluginRegistry::new(vec![Arc::new(
            SuppressingToolPlugin,
        )])),
    );
    (temp, context, adapter)
}

pub(super) fn built_in_test_context(
    kind: ConversationKind,
) -> (tempfile::TempDir, Arc<PlatformTurnContext>) {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let adapter = Arc::new(CountingAdapter {
        calls: AtomicUsize::new(0),
        fail_first: false,
        messages: Mutex::new(Vec::new()),
        group_members: test_group_members(),
    });
    let context = PlatformTurnContext::new(
        PlatformConversation {
            platform: "onebot".to_string(),
            account_id: "10000".to_string(),
            kind,
            conversation_id: "20000".to_string(),
        },
        "20000".to_string(),
        "tester".to_string(),
        false,
        AppConfig::default(),
        paths.clone(),
        StateStore::new(&paths).unwrap(),
        adapter,
        Arc::new(plugins::PlatformPluginRegistry::built_in().unwrap()),
    );
    (temp, Arc::new(context))
}

pub(super) struct SuppressingToolPlugin;

impl plugins::PlatformPlugin for SuppressingToolPlugin {
    fn descriptor(&self) -> plugins::PluginDescriptor {
        plugins::PluginDescriptor {
            id: "test_suppress",
            priority: 1,
            default_enabled: true,
        }
    }

    fn before_send<'a>(
        &'a self,
        _context: &'a PlatformTurnContext,
        message: OutboundMessage,
    ) -> BoxFuture<'a, Result<plugins::PreparedSend>> {
        Box::pin(async move {
            Ok(plugins::PreparedSend {
                primary: message.clone(),
                after_success: Vec::new(),
                fallback: Some(message),
                suppress_final_reply: true,
                suppress_prior_reply: false,
            })
        })
    }
}

pub(super) struct CountingAdapter {
    pub(super) calls: AtomicUsize,
    pub(super) fail_first: bool,
    pub(super) messages: Mutex<Vec<OutboundMessage>>,
    pub(super) group_members: Vec<PlatformGroupMember>,
}

pub(super) struct PartialFailureAdapter {
    pub(super) calls: AtomicUsize,
    pub(super) digest: blake3::Hash,
    pub(super) response_target_delivered: bool,
}

impl PlatformAdapter for CountingAdapter {
    fn send<'a>(&'a self, message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
        Box::pin(async move {
            let image_digests = match &message.body {
                OutboundBody::Segments(segments) => segments
                    .iter()
                    .filter_map(|segment| match segment {
                        OutboundSegment::ImageBytes { data, .. } => Some(blake3::hash(data)),
                        _ => None,
                    })
                    .collect(),
                OutboundBody::Forward(_) => Vec::new(),
            };
            self.messages.lock().unwrap().push(message);
            let call = self.calls.fetch_add(1, AtomicOrdering::Relaxed);
            if self.fail_first && call == 0 {
                anyhow::bail!("injected primary failure");
            }
            Ok(SendReceipt {
                delivered_parts: 1,
                image_digests,
                ..SendReceipt::default()
            })
        })
    }

    fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok("Miyu".to_string()) })
    }

    fn group_members<'a>(&'a self) -> BoxFuture<'a, Result<Vec<PlatformGroupMember>>> {
        let members = self.group_members.clone();
        Box::pin(async move { Ok(members) })
    }
}

impl PlatformAdapter for PartialFailureAdapter {
    fn send<'a>(&'a self, _message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
        Box::pin(async move {
            self.calls.fetch_add(1, AtomicOrdering::Relaxed);
            Err(anyhow::Error::new(PartialSendError::new(
                anyhow::anyhow!("injected failure after partial delivery"),
                SendReceipt {
                    delivered_parts: 1,
                    image_digests: vec![self.digest],
                    response_target_delivered: self.response_target_delivered,
                    ..SendReceipt::default()
                },
            )))
        })
    }

    fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok("Miyu".to_string()) })
    }
}
