//! 宿主能力端口(09-16 core/normal 接口治理)。
//!
//! 下层想用上层才有的能力时,不再反向 `use miyu_hosts::web` / `use miyu_hosts::platforms`,
//! 而是调这里的窄 trait;实现由**拥有那份能力的层**在 daemon 启动时装入
//! (`web::server` 装语音桥,`platforms::onebot::proactive` 装 QQ 直发)。
//! 每条端口只暴露调用方真正用到的几个动作,不传 `DaemonState`、不传配置整本。
//!
//! - [`VoicePort`]:语音桥。`speak` / `end_voice_chat` 工具、平台的
//!   `send_voice_message`、QQ 入站语音转写都走它。
//! - [`QqOutreachPort`]:终端会话往 QQ 直发(`send_qq_message` 工具)。
//!
//! 非 daemon 进程(REPL 直连、`miyu run` 单次、测试)里没人装端口,取到 `None`
//! ——与从前 `voice_bridge::daemon_state()` 为 `None` 同义,各调用方沿用原来的
//! 错误文案兜底。装入是覆盖语义(后装的赢),测试可以装假实现。

use crate::config::AppConfig;
use crate::platform_types::OutboundMessage;
use anyhow::Result;
use futures_util::future::BoxFuture;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// 语音桥能力。方法语义与 `web::voice_bridge` 同名函数一一对应。
pub trait VoicePort: Send + Sync {
    /// 播报能不能用:daemon 内、TTS 开关开着、播报供应商激活。平台工具注册时问。
    fn tts_available(&self) -> bool;
    /// 语音识别当下能不能用:语音唤醒开着且前端已接上。入站 QQ 语音据它决定
    /// 「转」还是「静默留占位」,不会去等前端拉起。
    fn stt_available(&self) -> bool;
    /// `end_voice_chat` 工具:向语音前端发关窗信令;前端不在时无操作。
    fn end_voice_chat(&self);
    /// `speak` 工具:合成后从扬声器播出。前端未就绪时拉起并等它。
    fn speak(&self, text: String) -> BoxFuture<'static, Result<()>>;
    /// 合成成 wav 文件(QQ 语音消息用),不播;调用方用完负责删。
    fn synthesize_file(&self, text: String) -> BoxFuture<'static, Result<PathBuf>>;
    /// 整段 16k 单声道 PCM WAV → 文本。
    fn transcribe_wav(&self, wav: Vec<u8>) -> BoxFuture<'static, Result<String>>;
}

/// 终端会话直发 QQ 的策略快照:按当前配置现算,配置重载后下一次调用即生效。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QqOutreachPolicy {
    /// `platforms.terminal_outreach`。
    pub allowed: bool,
    /// (QQ 号, 显示名)按配置顺序;第一个是主管理员。
    pub recipients: Vec<(i64, String)>,
}

/// 直发的收件方:好友(私聊)或群。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QqTarget {
    Friend(i64),
    Group(i64),
}

impl QqTarget {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Friend(_) => "friend",
            Self::Group(_) => "group",
        }
    }

    pub fn id(self) -> i64 {
        match self {
            Self::Friend(id) | Self::Group(id) => id,
        }
    }
}

/// 好友一条:昵称 + 备注(备注可空)。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QqContact {
    pub user_id: i64,
    pub nickname: String,
    pub remark: String,
}

/// 机器人所在的群一条。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QqGroup {
    pub group_id: i64,
    pub name: String,
    pub member_count: u32,
}

/// 地址簿:机器人的好友列表 + 它所在的群(NapCat `get_friend_list` /
/// `get_group_list`)。「发到 xxx 交流群」要先把名字翻成号码,靠的就是它。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QqDirectory {
    pub friends: Vec<QqContact>,
    pub groups: Vec<QqGroup>,
}

/// 往 QQ 直发的能力(不经 AI 回合):终端会话的 `send_qq_message`、平台会话里
/// 管理员让她「发到别的群/好友」都经这里。
pub trait QqOutreachPort: Send + Sync {
    /// NapCat 的反向 WebSocket 至少一个账号在线。
    fn connected(&self) -> bool;
    fn policy(&self) -> QqOutreachPolicy;
    /// 直发到任意好友/群。`account` 为空用第一个在线账号(平台会话传当前会话
    /// 所在的账号,多号时别串号)。
    fn send_to(
        &self,
        account: Option<i64>,
        target: QqTarget,
        message: OutboundMessage,
    ) -> BoxFuture<'static, Result<()>>;
    /// 地址簿(实现方缓存几分钟,别每次调用都打两个 API)。
    fn directory(&self, account: Option<i64>) -> BoxFuture<'static, Result<QqDirectory>>;
    /// 私聊直发(老入口,等于 `send_to(None, Friend)`)。
    fn send_private(
        &self,
        user_id: i64,
        message: OutboundMessage,
    ) -> BoxFuture<'static, Result<()>> {
        self.send_to(None, QqTarget::Friend(user_id), message)
    }
}

/// 收件人只能是 `qq.admin_users` 里的号码:显示名取 `qq.admin_aliases` 的别名,
/// 没别名显示号码;顺序照配置,第一个是主管理员。
pub fn qq_outreach_policy(config: &AppConfig) -> QqOutreachPolicy {
    let qq = &config.platforms.qq;
    let recipients = qq
        .admin_users
        .iter()
        .map(|id| {
            let label = qq
                .admin_aliases
                .get(&id.to_string())
                .map(|alias| alias.trim())
                .filter(|alias| !alias.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| id.to_string());
            (*id, label)
        })
        .collect();
    QqOutreachPolicy {
        allowed: config.platforms.terminal_outreach,
        recipients,
    }
}

static VOICE: RwLock<Option<Arc<dyn VoicePort>>> = RwLock::new(None);
static QQ_OUTREACH: RwLock<Option<Arc<dyn QqOutreachPort>>> = RwLock::new(None);

pub fn install_voice_port(port: Arc<dyn VoicePort>) {
    *VOICE.write().unwrap() = Some(port);
}

/// 语音桥端口;非 daemon 进程里为 `None`。
pub fn voice_port() -> Option<Arc<dyn VoicePort>> {
    VOICE.read().unwrap().clone()
}

pub fn install_qq_outreach_port(port: Arc<dyn QqOutreachPort>) {
    *QQ_OUTREACH.write().unwrap() = Some(port);
}

/// QQ 直发端口;非 daemon 进程里为 `None`。
pub fn qq_outreach_port() -> Option<Arc<dyn QqOutreachPort>> {
    QQ_OUTREACH.read().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outreach_policy_follows_admin_list_and_aliases() {
        let mut config = AppConfig::default();
        config.platforms.terminal_outreach = false;
        assert_eq!(qq_outreach_policy(&config), QqOutreachPolicy::default());
        config.platforms.terminal_outreach = true;
        config.platforms.qq.admin_users = vec![10001, 10002, 10003];
        config
            .platforms
            .qq
            .admin_aliases
            .insert("10002".to_string(), "  老板 ".to_string());
        // 空白别名视同没别名。
        config
            .platforms
            .qq
            .admin_aliases
            .insert("10003".to_string(), "   ".to_string());
        let policy = qq_outreach_policy(&config);
        assert!(policy.allowed);
        assert_eq!(
            policy.recipients,
            vec![
                (10001, "10001".to_string()),
                (10002, "老板".to_string()),
                (10003, "10003".to_string()),
            ]
        );
    }

    /// 装入前取不到;装入后工具层拿到的就是那份实现。假实现全部报「不可用」,
    /// 免得影响同进程里其它按「非 daemon」前提写的用例。
    struct SilentVoice;

    impl VoicePort for SilentVoice {
        fn tts_available(&self) -> bool {
            false
        }
        fn stt_available(&self) -> bool {
            false
        }
        fn end_voice_chat(&self) {}
        fn speak(&self, _text: String) -> BoxFuture<'static, Result<()>> {
            Box::pin(async { anyhow::bail!("silent") })
        }
        fn synthesize_file(&self, _text: String) -> BoxFuture<'static, Result<PathBuf>> {
            Box::pin(async { anyhow::bail!("silent") })
        }
        fn transcribe_wav(&self, wav: Vec<u8>) -> BoxFuture<'static, Result<String>> {
            Box::pin(async move { Ok(format!("{} bytes", wav.len())) })
        }
    }

    #[tokio::test]
    async fn installed_voice_port_is_what_callers_see() {
        install_voice_port(Arc::new(SilentVoice));
        let port = voice_port().expect("installed");
        assert!(!port.tts_available());
        assert_eq!(port.transcribe_wav(vec![0; 4]).await.unwrap(), "4 bytes");
        assert!(port.speak("hi".to_string()).await.is_err());
    }
}
