mod settings;
pub(crate) use settings::*;

use super::renderer::{MarkdownImageRenderer, RenderConfig};
use super::{PlatformPlugin, PlatformTurnInput, PluginDescriptor, PreparedSend};
use crate::platforms::{
    ConversationKind, ForwardNode, OutboundBody, OutboundMessage, OutboundOrigin, OutboundSegment,
    PlatformTurnContext, SendReceipt,
};
use crate::state::PlatformPluginScopeKey;
use anyhow::Result;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const PLUGIN_ID: &str = "reply_processor";

pub(super) struct ReplyProcessorPlugin {
    renderer: OnceLock<std::result::Result<MarkdownImageRenderer, String>>,
}

impl ReplyProcessorPlugin {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            renderer: OnceLock::new(),
        })
    }

    fn renderer(&self) -> Result<MarkdownImageRenderer> {
        self.renderer
            .get_or_init(|| MarkdownImageRenderer::new().map_err(|error| error.to_string()))
            .as_ref()
            .cloned()
            .map_err(|error| anyhow::Error::msg(error.clone()))
    }

    fn scope(context: &PlatformTurnContext) -> PlatformPluginScopeKey {
        PlatformPluginScopeKey {
            plugin_id: PLUGIN_ID.to_string(),
            platform: context.conversation.platform.clone(),
            account_id: context.conversation.account_id.clone(),
            conversation_kind: context.conversation.kind.as_str().to_string(),
            conversation_id: context.conversation.conversation_id.clone(),
        }
    }

    fn overrides(context: &PlatformTurnContext) -> Result<Option<SessionOverrides>> {
        context
            .state_store
            .plugin_get_json(&Self::scope(context), OVERRIDES_KEY)
    }

    fn effective_settings(context: &PlatformTurnContext) -> Result<EffectiveSettings> {
        let config = ReplyProcessorConfig::from_context(context);
        let overrides = Self::overrides(context)?.unwrap_or_default();
        Ok(EffectiveSettings {
            enabled: overrides.enabled.unwrap_or(config.default_enabled),
            threshold: overrides.threshold.unwrap_or(config.threshold),
            mode: overrides.mode.unwrap_or(config.mode),
            custom: !overrides.is_empty(),
            config,
        })
    }

    fn save_overrides(context: &PlatformTurnContext, overrides: &SessionOverrides) -> Result<()> {
        let scope = Self::scope(context);
        if overrides.is_empty() {
            context
                .state_store
                .plugin_delete_key(&scope, OVERRIDES_KEY)?;
        } else {
            context
                .state_store
                .plugin_put_json(&scope, OVERRIDES_KEY, overrides)?;
        }
        Ok(())
    }

    fn command_response(text: impl Into<String>) -> OutboundMessage {
        OutboundMessage::text(OutboundOrigin::Command, text)
    }

    fn handle_admin_command(
        context: &PlatformTurnContext,
        command: &str,
    ) -> Result<OutboundMessage> {
        if !context.is_admin {
            return Ok(Self::command_response(
                "只有在 admin_users 中配置的 QQ 用户可以修改回复处理设置。",
            ));
        }

        let mut parts = command.split_whitespace();
        let action = parts.next().unwrap_or("状态");
        match action {
            "" | "状态" | "status" => Ok(Self::command_response(Self::format_status(
                context,
                &Self::effective_settings(context)?,
            ))),
            "阈值" | "threshold" => {
                let Some(value) = parts.next() else {
                    return Ok(Self::command_response(
                        "用法：/回复处理 阈值 <1-100000|开|关>",
                    ));
                };
                if parts.next().is_some() {
                    return Ok(Self::command_response(
                        "用法：/回复处理 阈值 <1-100000|开|关>",
                    ));
                }
                let mut overrides = Self::overrides(context)?.unwrap_or_default();
                match value.trim().to_ascii_lowercase().as_str() {
                    "开" | "开启" | "启用" | "on" | "enable" | "enabled" | "true" => {
                        overrides.enabled = Some(true);
                        Self::save_overrides(context, &overrides)?;
                        Ok(Self::command_response("当前会话的回复处理已开启。"))
                    }
                    "关" | "关闭" | "禁用" | "off" | "disable" | "disabled" | "false" => {
                        overrides.enabled = Some(false);
                        Self::save_overrides(context, &overrides)?;
                        Ok(Self::command_response("当前会话的回复处理已关闭。"))
                    }
                    value => match value.parse::<usize>() {
                        Ok(threshold) if (1..=MAX_THRESHOLD).contains(&threshold) => {
                            overrides.enabled = Some(true);
                            overrides.threshold = Some(threshold);
                            Self::save_overrides(context, &overrides)?;
                            Ok(Self::command_response(format!(
                                "当前会话的回复处理阈值已设为 {threshold} 字，并已开启。"
                            )))
                        }
                        _ => Ok(Self::command_response(
                            "阈值必须是 1 到 100000 之间的整数。",
                        )),
                    },
                }
            }
            "模式" | "mode" => {
                let Some(value) = parts.next() else {
                    return Ok(Self::command_response(
                        "用法：/回复处理 模式 <转图片|合并转发>",
                    ));
                };
                if parts.next().is_some() {
                    return Ok(Self::command_response(
                        "用法：/回复处理 模式 <转图片|合并转发>",
                    ));
                }
                let Some(mode) = ReplyMode::parse(value) else {
                    return Ok(Self::command_response("模式只能是“转图片”或“合并转发”。"));
                };
                let mut overrides = Self::overrides(context)?.unwrap_or_default();
                overrides.enabled = Some(true);
                overrides.mode = Some(mode);
                Self::save_overrides(context, &overrides)?;
                Ok(Self::command_response(format!(
                    "当前会话的回复处理模式已设为{}，并已开启。",
                    mode.label()
                )))
            }
            "恢复默认" | "重置" | "reset" => {
                if parts.next().is_some() {
                    return Ok(Self::command_response("用法：/回复处理 恢复默认"));
                }
                context
                    .state_store
                    .plugin_delete_key(&Self::scope(context), OVERRIDES_KEY)?;
                let settings = Self::effective_settings(context)?;
                Ok(Self::command_response(format!(
                    "已恢复当前会话的默认回复处理设置。\n{}",
                    Self::format_status(context, &settings)
                )))
            }
            _ => Ok(Self::command_response(
                "用法：/回复处理 状态｜阈值 <数值|开|关>｜模式 <转图片|合并转发>｜恢复默认",
            )),
        }
    }

    fn format_status(context: &PlatformTurnContext, settings: &EffectiveSettings) -> String {
        format!(
            "回复处理状态\n会话：{}\n状态：{}\n阈值：{} 字\n模式：{}\n去尾句号：{}\n来源：{}",
            context.conversation.scope_key(),
            if settings.enabled { "开启" } else { "关闭" },
            settings.threshold,
            settings.mode.label(),
            if settings.config.strip_period {
                "开启"
            } else {
                "关闭"
            },
            if settings.custom {
                "当前会话自定义"
            } else {
                "默认配置"
            }
        )
    }

    async fn prepare_image_send(
        &self,
        message: OutboundMessage,
        settings: &EffectiveSettings,
        text: String,
    ) -> Result<PreparedSend> {
        let render_config = settings.config.render_config();
        let renderer = self.renderer()?;
        let rendered = renderer.render(&text, &render_config).await?;
        if rendered.is_empty() {
            return Ok(PreparedSend::unchanged(message));
        }

        let image_count = rendered.len();
        let replacement = rendered
            .into_iter()
            .enumerate()
            .map(|(index, image)| OutboundSegment::ImageBytes {
                mime: image.mime,
                data: Arc::from(image.png),
                alt: format!("长回复图片 {}/{}", index + 1, image_count),
            })
            .collect::<Vec<_>>();
        let mut transformed = replace_text_segments(message.clone(), replacement);
        transformed.response_target = message.response_target.clone();
        transformed.metadata.insert(
            IMAGE_METADATA_KEY.to_string(),
            json!({
                "char_count": text.chars().count(),
                "image_count": image_count,
            }),
        );
        Ok(PreparedSend {
            primary: transformed,
            after_success: Vec::new(),
            fallback: Some(message),
            suppress_final_reply: settings.config.send_tool_intercept,
            suppress_prior_reply: false,
        })
    }

    async fn prepare_forward_send(
        &self,
        context: &PlatformTurnContext,
        message: OutboundMessage,
        settings: &EffectiveSettings,
    ) -> Result<PreparedSend> {
        let OutboundBody::Segments(segments) = &message.body else {
            return Ok(PreparedSend::unchanged(message));
        };
        if segments
            .iter()
            .any(|segment| matches!(segment, OutboundSegment::FilePath { .. }))
        {
            return Ok(PreparedSend::unchanged(message));
        }
        let display_name = context
            .bot_display_name()
            .await
            .unwrap_or_else(|_| "Miyu".to_string());
        let mut transformed = OutboundMessage {
            body: OutboundBody::Forward(vec![ForwardNode {
                user_id: context.conversation.account_id.clone(),
                display_name,
                segments: segments.clone(),
            }]),
            response_target: message.response_target.clone(),
            origin: message.origin,
            metadata: message.metadata.clone(),
        };
        transformed
            .metadata
            .insert("reply_processor.forward".to_string(), Value::Bool(true));
        let mut after_success = Vec::new();
        if message.response_target.is_none()
            && settings.config.followup_mention
            && context.conversation.kind == ConversationKind::Group
        {
            after_success.push(OutboundMessage::segments(
                OutboundOrigin::Plugin,
                vec![
                    OutboundSegment::Mention(context.sender_id.clone()),
                    OutboundSegment::Text("\u{200b}".to_string()),
                ],
            ));
        }
        Ok(PreparedSend {
            primary: transformed,
            after_success,
            fallback: Some(message),
            suppress_final_reply: false,
            suppress_prior_reply: false,
        })
    }

    fn recent_notices(
        context: &PlatformTurnContext,
        config: &ReplyProcessorConfig,
    ) -> Result<Vec<ImageNotice>> {
        let scope = Self::scope(context);
        Ok(context
            .state_store
            .plugin_update_json(
                &scope,
                IMAGE_NOTICES_KEY,
                |stored: Option<Vec<ImageNotice>>| {
                    let recent = normalize_notices(stored.unwrap_or_default(), config);
                    Ok((!recent.is_empty()).then_some(recent))
                },
            )?
            .unwrap_or_default())
    }

    fn append_notice(
        context: &PlatformTurnContext,
        config: &ReplyProcessorConfig,
        notice: ImageNotice,
    ) -> Result<()> {
        let scope = Self::scope(context);
        context.state_store.plugin_update_json(
            &scope,
            IMAGE_NOTICES_KEY,
            |stored: Option<Vec<ImageNotice>>| {
                let mut notices = stored.unwrap_or_default();
                notices.push(notice);
                let notices = normalize_notices(notices, config);
                Ok((!notices.is_empty()).then_some(notices))
            },
        )?;
        Ok(())
    }

    fn context_notice(notices: &[ImageNotice]) -> String {
        let mut lines = vec![
            "[SystemInfo:LongReplyImageConversion]".to_string(),
            "以下是通讯平台发送层对你最近回复的处理记录，不是用户发言：".to_string(),
        ];
        for (index, notice) in notices.iter().enumerate() {
            lines.push(format!(
                "{}. 你的一条长回复（约 {} 字）已被自动渲染为 {} 张图片发送。",
                index + 1,
                notice.char_count,
                notice.image_count,
            ));
        }
        lines.push(
            "用户看到的是图片/长图；后续引用时请称作刚才图片里的内容。历史中的 assistant 文本表示图片内文字。"
                .to_string(),
        );
        lines.join("\n")
    }
}

impl PlatformPlugin for ReplyProcessorPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: PLUGIN_ID,
            priority: 100,
            default_enabled: true,
        }
    }

    fn handle_command<'a>(
        &'a self,
        context: &'a PlatformTurnContext,
        text: &'a str,
    ) -> BoxFuture<'a, Result<Option<OutboundMessage>>> {
        Box::pin(async move {
            let Some(command) = reply_command(text) else {
                return Ok(None);
            };
            Self::handle_admin_command(context, command).map(Some)
        })
    }

    fn before_turn<'a>(
        &'a self,
        context: &'a PlatformTurnContext,
        input: &'a mut PlatformTurnInput,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let settings = Self::effective_settings(context)?;
            if !settings.enabled || !settings.config.context_notice {
                return Ok(());
            }
            let notices = Self::recent_notices(context, &settings.config)?;
            if !notices.is_empty() {
                // Turn tail, not system prompt: the notice set changes whenever
                // a conversion happens or a record expires, and a changing
                // system prompt invalidates the whole history prefix. As a
                // fossilized tail block it appends instead; the agent skips it
                // when the identical text is already visible in the replay.
                input.turn_system_context.push(Self::context_notice(&notices));
            }
            Ok(())
        })
    }

    fn before_send<'a>(
        &'a self,
        context: &'a PlatformTurnContext,
        mut message: OutboundMessage,
    ) -> BoxFuture<'a, Result<PreparedSend>> {
        Box::pin(async move {
            let settings = Self::effective_settings(context)?;
            if !settings.enabled {
                return Ok(PreparedSend::unchanged(message));
            }
            if settings.mode == ReplyMode::Image && message_contains_file(&message) {
                return Ok(PreparedSend::unchanged(message));
            }
            if settings.config.strip_period {
                strip_trailing_chinese_period(&mut message);
            }
            if settings.mode == ReplyMode::Forward
                && message
                    .response_target
                    .as_ref()
                    .is_some_and(|target| !target.explicit_mention_user_ids.is_empty())
            {
                return Ok(PreparedSend::unchanged(message));
            }
            if matches!(message.body, OutboundBody::Forward(_)) {
                return Ok(PreparedSend::unchanged(message));
            }
            let text = message_text(&message);
            if text.chars().count() <= settings.threshold {
                return Ok(PreparedSend::unchanged(message));
            }
            match settings.mode {
                ReplyMode::Image
                    if message.origin != OutboundOrigin::Tool
                        || settings.config.send_tool_intercept =>
                {
                    match self
                        .prepare_image_send(message.clone(), &settings, text)
                        .await
                    {
                        Ok(prepared) => Ok(prepared),
                        Err(error) => {
                            tracing::warn!(
                                target: "miyu::qq",
                                error = %error,
                                "{}",
                                crate::i18n::text(
                                    "long-reply image rendering failed; keeping text output",
                                    "长回复图片渲染失败；保留文本输出"
                                )
                            );
                            Ok(PreparedSend::unchanged(message))
                        }
                    }
                }
                ReplyMode::Image => Ok(PreparedSend::unchanged(message)),
                ReplyMode::Forward => self.prepare_forward_send(context, message, &settings).await,
            }
        })
    }

    fn after_send<'a>(
        &'a self,
        context: &'a PlatformTurnContext,
        message: &'a OutboundMessage,
        receipt: &'a SendReceipt,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let Some(metadata) = message.metadata.get(IMAGE_METADATA_KEY) else {
                return Ok(());
            };
            let settings = Self::effective_settings(context)?;
            if !settings.config.context_notice {
                return Ok(());
            }
            let char_count = metadata
                .get("char_count")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let image_count = metadata
                .get("image_count")
                .and_then(Value::as_u64)
                .unwrap_or(1) as usize;
            Self::append_notice(
                context,
                &settings.config,
                ImageNotice {
                    timestamp: unix_timestamp(),
                    char_count,
                    image_count: image_count.max(1),
                    legacy_preview: None,
                    message_ids: receipt.message_ids.clone(),
                },
            )
        })
    }
}

#[cfg(test)]
mod tests;
