//! `send_qq_message`:本地会话(REPL / WebUI / shellhook)里让模型把消息发到
//! 用户的 QQ。只在 `platforms.terminal_outreach` 打开时注册,平台会话不注册
//! (那边有 `send_message_to_user`)。收件人只能是 `qq.admin_users` 里的号码:
//! `to` 的可选项按 `qq.admin_aliases` 的别名列出(没别名显示号码),不传发给
//! 第一个(主管理员)。`voice: true` 走文本转语音发语音消息。

use super::{ToolRegistry, ToolSpec};
use anyhow::{bail, Context, Result};
use miyu_base::config::AppConfig;
use miyu_base::host_ports::qq_outreach_policy;
use miyu_base::platform_types::{OutboundMessage, OutboundOrigin, OutboundSegment};
use serde_json::{json, Value};

pub const TOOL_NAME: &str = "send_qq_message";

/// NapCat 的反向 WebSocket 是否已连上(至少一个账号在线)。工具只在连上时
/// 注册:掉线时模型看不到它,不会对着断线的通道尝试。直发能力经
/// `host_ports::QqOutreachPort` 拿,非 daemon 进程里没装端口即视为未连上。
pub fn qq_connected() -> bool {
    miyu_base::host_ports::qq_outreach_port().is_some_and(|port| port.connected())
}

pub fn register(registry: &mut ToolRegistry, config: &AppConfig) {
    let list = qq_outreach_policy(config).recipients;
    let labels: Vec<String> = list.iter().map(|(_, label)| label.clone()).collect();
    let primary = labels.first().cloned().unwrap_or_default();
    let mut to_schema = json!({
        "type": "string",
        "description": format!("Recipient. Omit to reach the primary administrator ({primary})."),
    });
    if !labels.is_empty() {
        to_schema["enum"] = Value::Array(labels.into_iter().map(Value::String).collect());
    }
    registry.register(
        ToolSpec::new(
            TOOL_NAME,
            "Send a message to the user's QQ, as text or as a spoken voice message.",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Message text (spoken text when voice is true)." },
                    "voice": { "type": "boolean", "description": "Send as a voice message instead of text." },
                    "to": to_schema
                },
                "required": ["text"],
                "additionalProperties": false
            }),
            move |arguments| async move { send(arguments).await },
        )
        .writes(),
    );
}

async fn send(arguments: Value) -> Result<String> {
    let text = arguments
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if text.is_empty() {
        bail!("text is required");
    }
    let voice = arguments
        .get("voice")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let port = miyu_base::host_ports::qq_outreach_port()
        .context("send_qq_message only works inside the daemon")?;
    // 策略按当前配置现算(不是注册那一刻的):配置重载后立刻生效。
    let policy = port.policy();
    let list = policy.recipients;
    if !policy.allowed {
        bail!("sending to messaging platforms from the terminal is disabled in settings");
    }
    let Some((primary, _)) = list.first() else {
        bail!("no administrator QQ id is configured (接入通讯平台 → 允许使用终端的管理员 QQ 号)");
    };
    let target = match arguments.get("to").and_then(Value::as_str).map(str::trim) {
        Some(to) if !to.is_empty() => list
            .iter()
            .find(|(id, label)| label == to || id.to_string() == to)
            .map(|(id, _)| *id)
            .with_context(|| {
                let allowed: Vec<&str> = list.iter().map(|(_, label)| label.as_str()).collect();
                format!("unknown recipient {to}; allowed: {allowed:?}")
            })?,
        _ => *primary,
    };
    let kind = if voice {
        let path = miyu_base::host_ports::voice_port()
            .context("send_qq_message only works inside the daemon")?
            .synthesize_file(text.clone())
            .await?;
        let outcome = port
            .send_private(
                target,
                OutboundMessage::segments(
                    OutboundOrigin::Tool,
                    vec![OutboundSegment::AudioPath {
                        path: path.clone(),
                        transcript: text.clone(),
                    }],
                ),
            )
            .await;
        let _ = std::fs::remove_file(&path);
        outcome?;
        "voice"
    } else {
        // 来源沿用旧 `send_direct_text` 的 `Plugin`(定时消息同一条路),不改语义。
        port.send_private(target, OutboundMessage::text(OutboundOrigin::Plugin, &text))
            .await?;
        "text"
    };
    Ok(json!({ "ok": true, "kind": kind, "to": target }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::BoxFuture;
    use miyu_base::host_ports::{QqOutreachPolicy, QqOutreachPort};
    use miyu_base::platform_types::OutboundBody;
    use std::sync::{Arc, Mutex};

    struct RecordingPort {
        policy: QqOutreachPolicy,
        sent: Arc<Mutex<Vec<(i64, OutboundMessage)>>>,
    }

    impl QqOutreachPort for RecordingPort {
        /// 报「没连上」:`TurnResources` 的缓存键读这个位,别让并行用例看见抖动。
        fn connected(&self) -> bool {
            false
        }

        fn policy(&self) -> QqOutreachPolicy {
            self.policy.clone()
        }

        fn send_private(
            &self,
            user_id: i64,
            message: OutboundMessage,
        ) -> BoxFuture<'static, Result<()>> {
            let sent = self.sent.clone();
            Box::pin(async move {
                sent.lock().unwrap().push((user_id, message));
                Ok(())
            })
        }
    }

    fn install(allowed: bool, sent: &Arc<Mutex<Vec<(i64, OutboundMessage)>>>) {
        miyu_base::host_ports::install_qq_outreach_port(Arc::new(RecordingPort {
            policy: QqOutreachPolicy {
                allowed,
                recipients: vec![(10001, "10001".to_string()), (10002, "老板".to_string())],
            },
            sent: sent.clone(),
        }));
    }

    /// 工具只认 `host_ports::QqOutreachPort`:没装端口(非 daemon 进程)报原来的错;
    /// 装上后按别名/号码解析收件人,不传 `to` 发主管理员,文本沿用 `Plugin` 来源。
    #[tokio::test]
    async fn send_routes_through_the_outreach_port() {
        let error = send(json!({ "text": "hi" })).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "send_qq_message only works inside the daemon"
        );

        let sent = Arc::new(Mutex::new(Vec::new()));
        install(false, &sent);
        let error = send(json!({ "text": "hi" })).await.unwrap_err();
        assert!(
            error.to_string().contains("disabled in settings"),
            "{error}"
        );

        install(true, &sent);
        let output = send(json!({ "text": " 开饭了 ", "to": "老板" }))
            .await
            .unwrap();
        assert!(output.contains("\"to\":10002"), "{output}");
        let error = send(json!({ "text": "x", "to": "路人" }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unknown recipient"), "{error}");
        send(json!({ "text": "默认" })).await.unwrap();
        assert!(send(json!({ "text": "  " })).await.is_err());

        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].0, 10002);
        assert!(matches!(sent[0].1.origin, OutboundOrigin::Plugin));
        assert!(matches!(
            &sent[0].1.body,
            OutboundBody::Segments(segments)
                if matches!(segments.as_slice(), [OutboundSegment::Text(text)] if text == "开饭了")
        ));
        assert_eq!(sent[1].0, 10001);
    }
}
