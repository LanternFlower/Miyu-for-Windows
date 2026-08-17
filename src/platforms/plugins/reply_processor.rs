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
const OVERRIDES_KEY: &str = "session_overrides";
const IMAGE_NOTICES_KEY: &str = "image_notices";
const IMAGE_METADATA_KEY: &str = "reply_processor.image_notice";
const MAX_THRESHOLD: usize = 100_000;

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReplyMode {
    Image,
    Forward,
}

impl ReplyMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "图片" | "图" | "转图片" | "文转图" | "image" | "img" => Some(Self::Image),
            "转发" | "合并转发" | "forward" => Some(Self::Forward),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Image => "长文转图片",
            Self::Forward => "合并转发",
        }
    }
}

#[derive(Clone, Debug)]
struct ReplyProcessorConfig {
    default_enabled: bool,
    threshold: usize,
    mode: ReplyMode,
    followup_mention: bool,
    strip_period: bool,
    theme: String,
    max_height: u32,
    font_size: u32,
    code_font_size: u32,
    padding: u32,
    context_notice: bool,
    ttl_hours: u64,
    max_records: usize,
    send_tool_intercept: bool,
    font: String,
    title_font: String,
    code_font: String,
    emoji_font: String,
}

impl Default for ReplyProcessorConfig {
    fn default() -> Self {
        Self {
            default_enabled: true,
            threshold: 300,
            mode: ReplyMode::Image,
            followup_mention: true,
            strip_period: true,
            theme: "paper".to_string(),
            max_height: 2600,
            font_size: 36,
            code_font_size: 30,
            padding: 64,
            context_notice: true,
            ttl_hours: 24,
            max_records: 3,
            send_tool_intercept: true,
            font: String::new(),
            title_font: String::new(),
            code_font: String::new(),
            emoji_font: String::new(),
        }
    }
}

impl ReplyProcessorConfig {
    fn from_context(context: &PlatformTurnContext) -> Self {
        let mut config = Self::default();
        let Some(instance) = context.config.platforms.qq.plugins.get(PLUGIN_ID) else {
            return config;
        };
        let settings = &instance.settings;
        config.default_enabled = bool_setting(settings, "default_enabled", config.default_enabled);
        config.threshold = usize_setting(settings, "threshold", config.threshold, 1, MAX_THRESHOLD);
        config.mode = string_setting(settings, "mode")
            .as_deref()
            .and_then(ReplyMode::parse)
            .unwrap_or(config.mode);
        config.followup_mention =
            bool_setting(settings, "followup_mention", config.followup_mention);
        config.strip_period = bool_setting(settings, "strip_period", config.strip_period);
        config.theme = match string_setting(settings, "theme").as_deref() {
            Some("light") => "light",
            Some("dark") => "dark",
            _ => "paper",
        }
        .to_string();
        config.max_height = usize_setting(
            settings,
            "max_height",
            config.max_height as usize,
            1000,
            5000,
        ) as u32;
        config.font_size =
            usize_setting(settings, "font_size", config.font_size as usize, 24, 56) as u32;
        config.code_font_size = usize_setting(
            settings,
            "code_font_size",
            config.code_font_size as usize,
            20,
            46,
        ) as u32;
        config.padding =
            usize_setting(settings, "padding", config.padding as usize, 36, 120) as u32;
        config.context_notice = bool_setting(settings, "context_notice", config.context_notice);
        config.ttl_hours =
            usize_setting(settings, "ttl_hours", config.ttl_hours as usize, 1, 168) as u64;
        config.max_records = usize_setting(settings, "max_records", config.max_records, 1, 10);
        config.send_tool_intercept =
            bool_setting(settings, "send_tool_intercept", config.send_tool_intercept);
        config.font = string_setting(settings, "font").unwrap_or_default();
        config.title_font = string_setting(settings, "title_font").unwrap_or_default();
        config.code_font = string_setting(settings, "code_font").unwrap_or_default();
        config.emoji_font = string_setting(settings, "emoji_font").unwrap_or_default();
        config
    }

    fn render_config(&self) -> RenderConfig {
        RenderConfig {
            theme: self.theme.clone(),
            max_height: self.max_height,
            font_size: self.font_size,
            code_font_size: self.code_font_size,
            padding: self.padding,
            font: self.font.clone(),
            title_font: self.title_font.clone(),
            code_font: self.code_font.clone(),
            emoji_font: self.emoji_font.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct EffectiveSettings {
    enabled: bool,
    threshold: usize,
    mode: ReplyMode,
    custom: bool,
    config: ReplyProcessorConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct SessionOverrides {
    enabled: Option<bool>,
    threshold: Option<usize>,
    mode: Option<ReplyMode>,
}

impl SessionOverrides {
    fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.threshold.is_none() && self.mode.is_none()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct ImageNotice {
    timestamp: i64,
    char_count: usize,
    image_count: usize,
    #[serde(default, rename = "preview", skip_serializing_if = "Option::is_none")]
    legacy_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    message_ids: Vec<String>,
}

fn normalize_notices(notices: Vec<ImageNotice>, config: &ReplyProcessorConfig) -> Vec<ImageNotice> {
    let cutoff = unix_timestamp().saturating_sub((config.ttl_hours * 60 * 60) as i64);
    let mut recent = notices
        .into_iter()
        .filter(|notice| notice.timestamp >= cutoff)
        .map(|mut notice| {
            notice.legacy_preview = None;
            notice
        })
        .collect::<Vec<_>>();
    recent.sort_by_key(|notice| notice.timestamp);
    if recent.len() > config.max_records {
        recent.drain(..recent.len() - config.max_records);
    }
    recent
}

fn reply_command(text: &str) -> Option<&str> {
    let text = text.trim();
    let command = text
        .strip_prefix("/回复处理")
        .or_else(|| text.strip_prefix("回复处理"))?;
    if !command.is_empty() && !command.starts_with(char::is_whitespace) {
        return None;
    }
    Some(command.trim())
}

fn bool_setting(settings: &serde_json::Map<String, Value>, key: &str, default: bool) -> bool {
    settings
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn usize_setting(
    settings: &serde_json::Map<String, Value>,
    key: &str,
    default: usize,
    min: usize,
    max: usize,
) -> usize {
    settings
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (min..=max).contains(value))
        .unwrap_or(default)
}

fn string_setting(settings: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    settings
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn message_text(message: &OutboundMessage) -> String {
    let OutboundBody::Segments(segments) = &message.body else {
        return String::new();
    };
    segments
        .iter()
        .filter_map(|segment| match segment {
            OutboundSegment::Markdown(text) | OutboundSegment::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn message_contains_file(message: &OutboundMessage) -> bool {
    matches!(
        &message.body,
        OutboundBody::Segments(segments)
            if segments
                .iter()
                .any(|segment| matches!(segment, OutboundSegment::FilePath { .. }))
    )
}

fn replace_text_segments(
    mut message: OutboundMessage,
    mut replacement: Vec<OutboundSegment>,
) -> OutboundMessage {
    let OutboundBody::Segments(segments) = &mut message.body else {
        return message;
    };
    let mut output = Vec::with_capacity(segments.len() + replacement.len());
    let mut inserted = false;
    for segment in std::mem::take(segments) {
        if matches!(
            segment,
            OutboundSegment::Markdown(_) | OutboundSegment::Text(_)
        ) {
            if !inserted {
                output.append(&mut replacement);
                inserted = true;
            }
        } else {
            output.push(segment);
        }
    }
    *segments = output;
    message
}

fn strip_trailing_chinese_period(message: &mut OutboundMessage) {
    let OutboundBody::Segments(segments) = &mut message.body else {
        return;
    };
    for segment in segments.iter_mut().rev() {
        let text = match segment {
            OutboundSegment::Markdown(text) | OutboundSegment::Text(text) => text,
            OutboundSegment::Mention(_)
            | OutboundSegment::ImageBytes { .. }
            | OutboundSegment::ImagePath { .. }
            | OutboundSegment::FilePath { .. } => continue,
        };
        let trimmed_len = text.trim_end().len();
        if trimmed_len == 0 {
            continue;
        }
        if text[..trimmed_len].ends_with('。') {
            let period_start = trimmed_len - '。'.len_utf8();
            text.replace_range(period_start..trimmed_len, "");
        }
        break;
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::paths::MiyuPaths;
    use crate::platforms::{PlatformAdapter, PlatformConversation, ResponseTarget};
    use crate::state::StateStore;
    use futures_util::future::BoxFuture;
    use std::path::PathBuf;

    struct NoopAdapter;

    impl PlatformAdapter for NoopAdapter {
        fn send<'a>(&'a self, _message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
            Box::pin(async { Ok(SendReceipt::default()) })
        }

        fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
            Box::pin(async { Ok("Miyu".to_string()) })
        }
    }

    fn test_context(is_admin: bool) -> (tempfile::TempDir, PlatformTurnContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let paths = MiyuPaths {
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
        };
        let context = PlatformTurnContext::new(
            PlatformConversation {
                platform: "onebot".to_string(),
                account_id: "10000".to_string(),
                kind: ConversationKind::Group,
                conversation_id: "20000".to_string(),
            },
            "30000".to_string(),
            "tester".to_string(),
            is_admin,
            AppConfig::default(),
            paths.clone(),
            StateStore::new(&paths).unwrap(),
            Arc::new(NoopAdapter),
            Arc::new(super::super::PlatformPluginRegistry::default()),
        );
        (temp, context)
    }

    #[test]
    fn command_prefix_requires_a_boundary() {
        assert_eq!(reply_command("/回复处理 状态"), Some("状态"));
        assert_eq!(reply_command("回复处理"), Some(""));
        assert_eq!(reply_command("/回复处理器 状态"), None);
        assert_eq!(reply_command("普通消息"), None);
    }

    #[test]
    fn strips_only_the_last_visible_chinese_period() {
        let mut message = OutboundMessage::segments(
            OutboundOrigin::FinalReply,
            vec![
                OutboundSegment::Text("第一段。".to_string()),
                OutboundSegment::ImageBytes {
                    mime: "image/png".to_string(),
                    data: Arc::from([1_u8, 2, 3]),
                    alt: String::new(),
                },
                OutboundSegment::Markdown("最后一段。  \n".to_string()),
            ],
        );
        strip_trailing_chinese_period(&mut message);
        assert_eq!(message_text(&message), "第一段。\n最后一段  \n");

        let mut english = OutboundMessage::text(OutboundOrigin::FinalReply, "keep.");
        strip_trailing_chinese_period(&mut english);
        assert_eq!(message_text(&english), "keep.");
    }

    #[test]
    fn text_replacement_keeps_non_text_segments_in_order() {
        let message = OutboundMessage::segments(
            OutboundOrigin::Tool,
            vec![
                OutboundSegment::Mention("1".to_string()),
                OutboundSegment::Text("long".to_string()),
                OutboundSegment::ImagePath {
                    path: "a.png".into(),
                    alt: "a".to_string(),
                },
                OutboundSegment::Markdown("more".to_string()),
                OutboundSegment::FilePath {
                    path: "b.txt".into(),
                    name: None,
                },
            ],
        );
        let replaced = replace_text_segments(
            message,
            vec![OutboundSegment::ImageBytes {
                mime: "image/png".to_string(),
                data: Arc::from([9_u8]),
                alt: "rendered".to_string(),
            }],
        );
        let OutboundBody::Segments(segments) = replaced.body else {
            panic!("expected segments");
        };
        assert!(matches!(segments[0], OutboundSegment::Mention(_)));
        assert!(matches!(segments[1], OutboundSegment::ImageBytes { .. }));
        assert!(matches!(segments[2], OutboundSegment::ImagePath { .. }));
        assert!(matches!(segments[3], OutboundSegment::FilePath { .. }));
    }

    #[test]
    fn session_override_round_trip_shape_is_stable() {
        let overrides = SessionOverrides {
            enabled: Some(true),
            threshold: Some(500),
            mode: Some(ReplyMode::Forward),
        };
        let json = serde_json::to_value(&overrides).unwrap();
        assert_eq!(json["mode"], "forward");
        assert_eq!(
            serde_json::from_value::<SessionOverrides>(json)
                .unwrap()
                .threshold,
            Some(500)
        );
    }

    #[test]
    fn admin_commands_update_and_restore_only_the_current_scope() {
        let (_temp, context) = test_context(true);
        ReplyProcessorPlugin::handle_admin_command(&context, "阈值 500").unwrap();
        let settings = ReplyProcessorPlugin::effective_settings(&context).unwrap();
        assert!(settings.enabled);
        assert_eq!(settings.threshold, 500);
        assert!(settings.custom);

        ReplyProcessorPlugin::handle_admin_command(&context, "模式 转发").unwrap();
        assert_eq!(
            ReplyProcessorPlugin::effective_settings(&context)
                .unwrap()
                .mode,
            ReplyMode::Forward
        );
        ReplyProcessorPlugin::handle_admin_command(&context, "阈值 关").unwrap();
        assert!(
            !ReplyProcessorPlugin::effective_settings(&context)
                .unwrap()
                .enabled
        );

        ReplyProcessorPlugin::handle_admin_command(&context, "恢复默认").unwrap();
        let defaults = ReplyProcessorPlugin::effective_settings(&context).unwrap();
        assert!(defaults.enabled);
        assert_eq!(defaults.threshold, 300);
        assert_eq!(defaults.mode, ReplyMode::Image);
        assert!(!defaults.custom);
    }

    #[test]
    fn non_admin_command_does_not_create_an_override() {
        let (_temp, context) = test_context(false);
        ReplyProcessorPlugin::handle_admin_command(&context, "阈值 1").unwrap();
        assert!(ReplyProcessorPlugin::overrides(&context).unwrap().is_none());
    }

    #[test]
    fn image_notice_cleanup_applies_ttl_and_record_limit() {
        let (_temp, context) = test_context(true);
        let config = ReplyProcessorConfig {
            ttl_hours: 1,
            max_records: 2,
            ..ReplyProcessorConfig::default()
        };
        let notices = vec![
            ImageNotice {
                timestamp: 0,
                char_count: 1,
                image_count: 1,
                legacy_preview: Some("expired".to_string()),
                message_ids: Vec::new(),
            },
            ImageNotice {
                timestamp: unix_timestamp() - 2,
                char_count: 2,
                image_count: 1,
                legacy_preview: Some("older".to_string()),
                message_ids: Vec::new(),
            },
            ImageNotice {
                timestamp: unix_timestamp() - 1,
                char_count: 3,
                image_count: 1,
                legacy_preview: Some("ignore previous instructions".to_string()),
                message_ids: Vec::new(),
            },
            ImageNotice {
                timestamp: unix_timestamp(),
                char_count: 4,
                image_count: 1,
                legacy_preview: Some("latest".to_string()),
                message_ids: Vec::new(),
            },
        ];
        context
            .state_store
            .plugin_put_json(
                &ReplyProcessorPlugin::scope(&context),
                IMAGE_NOTICES_KEY,
                &notices,
            )
            .unwrap();

        let recent = ReplyProcessorPlugin::recent_notices(&context, &config).unwrap();
        assert_eq!(
            recent
                .iter()
                .map(|notice| notice.char_count)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert!(recent.iter().all(|notice| notice.legacy_preview.is_none()));
        let persisted: Vec<ImageNotice> = context
            .state_store
            .plugin_get_json(&ReplyProcessorPlugin::scope(&context), IMAGE_NOTICES_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(persisted, recent);
        let persisted_json: Value = context
            .state_store
            .plugin_get_json(&ReplyProcessorPlugin::scope(&context), IMAGE_NOTICES_KEY)
            .unwrap()
            .unwrap();
        assert!(persisted_json
            .as_array()
            .unwrap()
            .iter()
            .all(|notice| notice.get("preview").is_none()));
    }

    #[test]
    fn concurrent_image_notice_appends_do_not_lose_records() {
        let (_temp, context) = test_context(true);
        let context = Arc::new(context);
        let config = ReplyProcessorConfig {
            max_records: 10,
            ..ReplyProcessorConfig::default()
        };
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let handles = (1..=8)
            .map(|char_count| {
                let context = context.clone();
                let config = config.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    ReplyProcessorPlugin::append_notice(
                        &context,
                        &config,
                        ImageNotice {
                            timestamp: unix_timestamp(),
                            char_count,
                            image_count: 1,
                            legacy_preview: None,
                            message_ids: Vec::new(),
                        },
                    )
                    .unwrap();
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }

        let mut char_counts = ReplyProcessorPlugin::recent_notices(&context, &config)
            .unwrap()
            .into_iter()
            .map(|notice| notice.char_count)
            .collect::<Vec<_>>();
        char_counts.sort_unstable();
        assert_eq!(char_counts, (1..=8).collect::<Vec<_>>());
    }

    fn set_plugin_setting(context: &mut PlatformTurnContext, key: &str, value: Value) {
        context
            .config
            .platforms
            .qq
            .plugins
            .entry(PLUGIN_ID.to_string())
            .or_default()
            .settings
            .insert(key.to_string(), value);
    }

    #[tokio::test]
    async fn default_threshold_converts_only_after_three_hundred_characters() {
        let (_temp, mut context) = test_context(true);
        set_plugin_setting(&mut context, "mode", json!("forward"));
        let plugin = ReplyProcessorPlugin::new().unwrap();

        let boundary = OutboundMessage::markdown(OutboundOrigin::FinalReply, "x".repeat(300));
        let unchanged = plugin.before_send(&context, boundary).await.unwrap();
        assert!(unchanged.fallback.is_none());
        assert!(matches!(unchanged.primary.body, OutboundBody::Segments(_)));

        let over = OutboundMessage::markdown(OutboundOrigin::FinalReply, "x".repeat(301));
        let converted = plugin.before_send(&context, over).await.unwrap();
        assert!(converted.fallback.is_some());
        assert!(matches!(converted.primary.body, OutboundBody::Forward(_)));

        set_plugin_setting(&mut context, "threshold", json!(150));
        assert_eq!(
            ReplyProcessorPlugin::effective_settings(&context)
                .unwrap()
                .threshold,
            150
        );
    }

    #[tokio::test]
    async fn forward_mode_preserves_the_selected_response_target_without_guessing_sender() {
        let (_temp, mut context) = test_context(true);
        set_plugin_setting(&mut context, "threshold", json!(1));
        set_plugin_setting(&mut context, "mode", json!("forward"));
        let plugin = ReplyProcessorPlugin::new().unwrap();
        let mut message = OutboundMessage::markdown(OutboundOrigin::FinalReply, "long。");
        message.response_target = Some(ResponseTarget {
            message_id: "9".to_string(),
            user_id: "40000".to_string(),
            quote: true,
            mention: true,
            explicit_mention_user_ids: Vec::new(),
        });

        let prepared = plugin.before_send(&context, message).await.unwrap();
        assert!(prepared.fallback.is_some());
        assert_eq!(
            prepared.primary.response_target,
            Some(ResponseTarget {
                message_id: "9".to_string(),
                user_id: "40000".to_string(),
                quote: true,
                mention: true,
                explicit_mention_user_ids: Vec::new(),
            })
        );
        let OutboundBody::Forward(nodes) = prepared.primary.body else {
            panic!("expected a forward message");
        };
        assert_eq!(nodes.len(), 1);
        assert!(matches!(
            &nodes[0].segments[0],
            OutboundSegment::Markdown(text) if text == "long"
        ));
        assert!(prepared.after_success.is_empty());
    }

    #[tokio::test]
    async fn forward_mode_keeps_explicit_mentions_in_the_regular_message() {
        let (_temp, mut context) = test_context(true);
        set_plugin_setting(&mut context, "threshold", json!(1));
        set_plugin_setting(&mut context, "mode", json!("forward"));
        let plugin = ReplyProcessorPlugin::new().unwrap();
        let mut message = OutboundMessage::markdown(OutboundOrigin::FinalReply, "long。");
        message.response_target = Some(ResponseTarget {
            message_id: "9".to_string(),
            user_id: "40000".to_string(),
            quote: true,
            mention: false,
            explicit_mention_user_ids: vec!["50000".to_string(), "60000".to_string()],
        });

        let prepared = plugin.before_send(&context, message).await.unwrap();

        assert!(matches!(prepared.primary.body, OutboundBody::Segments(_)));
        assert!(prepared.fallback.is_none());
        assert_eq!(
            prepared
                .primary
                .response_target
                .unwrap()
                .explicit_mention_user_ids,
            ["50000", "60000"]
        );
    }

    #[tokio::test]
    async fn image_mode_leaves_messages_with_files_untouched() {
        let (_temp, mut context) = test_context(true);
        set_plugin_setting(&mut context, "threshold", json!(1));
        set_plugin_setting(&mut context, "mode", json!("image"));
        let plugin = ReplyProcessorPlugin::new().unwrap();
        let message = OutboundMessage::segments(
            OutboundOrigin::Tool,
            vec![
                OutboundSegment::Markdown("long。".to_string()),
                OutboundSegment::FilePath {
                    path: PathBuf::from("/tmp/report.txt"),
                    name: Some("report.txt".to_string()),
                },
            ],
        );

        let prepared = plugin.before_send(&context, message).await.unwrap();

        assert!(prepared.fallback.is_none());
        assert!(!prepared.suppress_final_reply);
        assert!(prepared.primary.metadata.is_empty());
        let OutboundBody::Segments(segments) = prepared.primary.body else {
            panic!("expected untouched segments");
        };
        assert!(matches!(
            &segments[0],
            OutboundSegment::Markdown(text) if text == "long。"
        ));
        assert!(matches!(segments[1], OutboundSegment::FilePath { .. }));
    }

    #[tokio::test]
    async fn image_render_limit_failure_keeps_the_text_reply() {
        let (_temp, mut context) = test_context(true);
        set_plugin_setting(&mut context, "threshold", json!(1));
        set_plugin_setting(&mut context, "mode", json!("image"));
        let plugin = ReplyProcessorPlugin::new().unwrap();
        let text = "x".repeat(20_001);
        let message = OutboundMessage::markdown(OutboundOrigin::FinalReply, text.clone());

        let prepared = plugin.before_send(&context, message).await.unwrap();

        assert!(prepared.fallback.is_none());
        assert!(!prepared.suppress_final_reply);
        assert!(prepared.primary.metadata.is_empty());
        let OutboundBody::Segments(segments) = prepared.primary.body else {
            panic!("expected text fallback");
        };
        assert!(matches!(
            &segments[0],
            OutboundSegment::Markdown(value) if value == &text
        ));
    }

    #[tokio::test]
    async fn image_mode_records_only_a_confirmed_tool_render_and_injects_notice() {
        let (_temp, mut context) = test_context(true);
        set_plugin_setting(&mut context, "threshold", json!(1));
        set_plugin_setting(&mut context, "mode", json!("image"));
        let plugin = ReplyProcessorPlugin::new().unwrap();
        let message = OutboundMessage::markdown(OutboundOrigin::Tool, "# rendered long reply。");

        let prepared = plugin.before_send(&context, message).await.unwrap();
        assert!(prepared.suppress_final_reply);
        assert!(prepared.fallback.is_some());
        assert!(prepared.primary.metadata.contains_key(IMAGE_METADATA_KEY));
        let OutboundBody::Segments(segments) = &prepared.primary.body else {
            panic!("expected rendered image segments");
        };
        assert!(matches!(
            &segments[0],
            OutboundSegment::ImageBytes { data, .. }
                if data.starts_with(b"\x89PNG\r\n\x1a\n")
        ));

        let scope = ReplyProcessorPlugin::scope(&context);
        let before: Option<Vec<ImageNotice>> = context
            .state_store
            .plugin_get_json(&scope, IMAGE_NOTICES_KEY)
            .unwrap();
        assert!(before.is_none());
        plugin
            .after_send(
                &context,
                &prepared.primary,
                &SendReceipt {
                    message_ids: vec!["sent-1".to_string()],
                    image_message_ids: Vec::new(),
                    delivered_parts: 1,
                    image_digests: Vec::new(),
                    response_target_delivered: false,
                },
            )
            .await
            .unwrap();
        let stored: Vec<ImageNotice> = context
            .state_store
            .plugin_get_json(&scope, IMAGE_NOTICES_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].message_ids, vec!["sent-1"]);

        let mut input = PlatformTurnInput {
            content: "next".to_string(),
            memory_content: "next".to_string(),
            system_context: Vec::new(),
            turn_system_context: Vec::new(),
            context_images: Vec::new(),
            context_files: Vec::new(),
        };
        plugin.before_turn(&context, &mut input).await.unwrap();
        assert_eq!(input.content, "next");
        // 通知走 turn 尾部通道,system prompt 保持字节稳定
        assert!(input.system_context.is_empty());
        assert_eq!(input.turn_system_context.len(), 1);
        assert!(input.turn_system_context[0].contains("LongReplyImageConversion"));
        assert!(!input.turn_system_context[0].contains("rendered long reply"));
    }
}
