//! 不经 AI 回合的主动直发通道。
//!
//! 定时消息插件到点后把固定文本直接投递到会话——没有入站事件、没有 agent、
//! 没有回合调度，只借用现成的 `OneBotAdapter` 分帧与发送逻辑。

use crate::platforms::onebot::*;
use crate::runtime::{QqContact, QqDirectory, QqGroup, QqTarget};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 把一段纯文本直接发到指定 QQ 会话。
///
/// `account` 为空时用当前已连接的第一个账号；`conversation_kind` 只接受
/// `group` / `private`。任何一步失败都返回错误，由调用方决定日志与重试策略。
pub(crate) async fn send_direct_text(
    state: &DaemonState,
    account: Option<i64>,
    conversation_kind: &str,
    conversation_id: &str,
    text: &str,
) -> Result<()> {
    let text = text.trim();
    if text.is_empty() {
        bail!("scheduled message text is empty");
    }
    send_direct(
        state,
        account,
        conversation_kind,
        conversation_id,
        OutboundMessage::text(OutboundOrigin::Plugin, text),
    )
    .await
}

/// 同 [`send_direct_text`],但可以带任意出站消息(语音段等)。
pub(crate) async fn send_direct(
    state: &DaemonState,
    account: Option<i64>,
    conversation_kind: &str,
    conversation_id: &str,
    message: OutboundMessage,
) -> Result<()> {
    let registry = state.platforms.onebot.clone();
    let (self_id, conn) = {
        let locked = registry.lock().unwrap();
        let self_id = match account {
            Some(id) => id,
            None => *locked
                .connected_accounts()
                .first()
                .context("no QQ account is connected")?,
        };
        let conn = locked
            .handle(self_id)
            .context("the QQ account is not connected")?;
        (self_id, conn)
    };
    let target_id: i64 = conversation_id
        .parse()
        .context("invalid QQ conversation id for a scheduled message")?;
    let target = match conversation_kind {
        "group" => Target::Group {
            group_id: target_id,
        },
        "private" => Target::Private { user_id: target_id },
        other => bail!("unsupported QQ conversation kind: {other}"),
    };
    let max_reply_chars = state
        .manager
        .lock()
        .unwrap()
        .config
        .platforms
        .qq
        .max_reply_chars;
    let adapter = OneBotAdapter {
        conn,
        registry,
        http: state.platforms.http_client()?,
        self_id,
        target,
        max_reply_chars,
        file_store_lock: state.platforms.file_store_lock.clone(),
    };
    adapter.send_message(message).await?;
    Ok(())
}

/// 地址簿缓存多久。好友/群列表变动很慢,几分钟内重复问 NapCat 没意义;
/// 过期就重拉,拉失败时有旧的先用旧的。
const DIRECTORY_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// 拉一份地址簿:`get_friend_list` + `get_group_list`。字段按 OneBot 11 /
/// NapCat 的返回体取,缺的按空处理,别因为某个字段没给就整份作废。
async fn fetch_directory(state: &DaemonState, account: Option<i64>) -> Result<QqDirectory> {
    let registry = state.platforms.onebot.clone();
    let conn = {
        let locked = registry.lock().unwrap();
        let self_id = match account {
            Some(id) => id,
            None => *locked
                .connected_accounts()
                .first()
                .context("no QQ account is connected")?,
        };
        locked
            .handle(self_id)
            .context("the QQ account is not connected")?
    };
    let friends = conn
        .call_api("get_friend_list", json!({ "no_cache": false }))
        .await?;
    let groups = conn
        .call_api("get_group_list", json!({ "no_cache": false }))
        .await?;
    Ok(parse_directory(&friends, &groups))
}

fn parse_directory(friends: &Value, groups: &Value) -> QqDirectory {
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let friends = friends
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|friend| {
            let user_id = friend.get("user_id").and_then(Value::as_i64)?;
            Some(QqContact {
                user_id,
                nickname: text(friend, "nickname"),
                remark: text(friend, "remark"),
            })
        })
        .collect();
    let groups = groups
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|group| {
            let group_id = group.get("group_id").and_then(Value::as_i64)?;
            Some(QqGroup {
                group_id,
                name: text(group, "group_name"),
                member_count: group
                    .get("member_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
            })
        })
        .collect();
    QqDirectory { friends, groups }
}

/// [`crate::runtime::QqOutreachPort`] 的 OneBot 实现:终端会话的 `send_qq_message`
/// 工具经它直发,工具层不再认识 `DaemonState` 与 OneBot 适配器(09-16)。
/// 09-18 起平台会话(管理员)也经它发到别的好友/群,并带地址簿。
struct OutreachPort {
    state: DaemonState,
    /// 按账号缓存的地址簿(None 键 = 「第一个在线账号」)。
    directories: Arc<Mutex<HashMap<Option<i64>, (std::time::Instant, QqDirectory)>>>,
}

impl crate::runtime::QqOutreachPort for OutreachPort {
    fn connected(&self) -> bool {
        !self
            .state
            .platforms
            .onebot
            .lock()
            .unwrap()
            .connected_accounts()
            .is_empty()
    }

    fn policy(&self) -> crate::runtime::QqOutreachPolicy {
        crate::runtime::qq_outreach_policy(&self.state.manager.lock().unwrap().config)
    }

    fn send_to(
        &self,
        account: Option<i64>,
        target: QqTarget,
        message: OutboundMessage,
    ) -> futures_util::future::BoxFuture<'static, Result<()>> {
        let state = self.state.clone();
        let kind = match target {
            QqTarget::Friend(_) => "private",
            QqTarget::Group(_) => "group",
        };
        Box::pin(async move {
            send_direct(&state, account, kind, &target.id().to_string(), message).await
        })
    }

    fn directory(
        &self,
        account: Option<i64>,
    ) -> futures_util::future::BoxFuture<'static, Result<QqDirectory>> {
        let state = self.state.clone();
        let cache = self.directories.clone();
        Box::pin(async move {
            let now = std::time::Instant::now();
            let stale = {
                let cache = cache.lock().unwrap();
                match cache.get(&account) {
                    Some((fetched, directory)) if now.duration_since(*fetched) < DIRECTORY_TTL => {
                        return Ok(directory.clone());
                    }
                    Some((_, directory)) => Some(directory.clone()),
                    None => None,
                }
            };
            match fetch_directory(&state, account).await {
                Ok(directory) => {
                    cache
                        .lock()
                        .unwrap()
                        .insert(account, (now, directory.clone()));
                    Ok(directory)
                }
                // 拉失败(NapCat 忙/超时)有旧的先用旧的,别让一次抖动把
                // 「发到某群」整条卡死。
                Err(error) => match stale {
                    Some(directory) => {
                        tracing::warn!(target: "miyu::qq", error = %error, "QQ directory refresh failed; using the cached copy");
                        Ok(directory)
                    }
                    None => Err(error),
                },
            }
        })
    }
}

/// daemon 启动时装入端口(`web::server`)。
pub(crate) fn install_outreach_port(state: &DaemonState) {
    crate::runtime::install_qq_outreach_port(std::sync::Arc::new(OutreachPort {
        state: state.clone(),
        directories: Default::default(),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 地址簿按 OneBot 返回体解析:缺备注/缺人数照样收下,没 id 的丢掉。
    #[test]
    fn directory_parses_friend_and_group_lists() {
        let friends = json!([
            { "user_id": 10001, "nickname": "小明", "remark": "同事小明" },
            { "user_id": 10002, "nickname": "阿花" },
            { "nickname": "没有号的" }
        ]);
        let groups = json!([
            { "group_id": 20001, "group_name": "Miyu 交流群", "member_count": 42 },
            { "group_id": 20002, "group_name": "家庭群" }
        ]);
        let directory = parse_directory(&friends, &groups);
        assert_eq!(directory.friends.len(), 2);
        assert_eq!(directory.friends[0].remark, "同事小明");
        assert_eq!(directory.friends[1].remark, "");
        assert_eq!(directory.groups.len(), 2);
        assert_eq!(directory.groups[0].member_count, 42);
        assert_eq!(directory.groups[1].name, "家庭群");
        assert_eq!(
            parse_directory(&json!(null), &json!({})),
            QqDirectory::default()
        );
    }
}
