use super::types::{ConversationKind, OutboundMessage, OutboundOrigin, OutboundSegment};
use super::PlatformTurnContext;
use crate::i18n::agent_text as t;
use crate::tools::{ToolRegistry, ToolSpec};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_TOOL_IMAGES: usize = 4;
const MAX_TOOL_FILES: usize = 4;
const MAX_TOOL_MENTIONS: usize = 32;

pub(crate) fn register(registry: &mut ToolRegistry, context: Arc<PlatformTurnContext>) {
    if context.conversation.kind == ConversationKind::Group {
        register_mention(registry, context.clone());
    }
    register_usage_query(registry, context.clone());
    let host_tools_allowed = context.host_tools_allowed();
    let parameters = if host_tools_allowed {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": t("Optional text or Markdown.", "可选文本或 Markdown。") },
                "images": {
                    "type": "array",
                    "maxItems": MAX_TOOL_IMAGES,
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "alt": { "type": "string" }
                        },
                        "required": ["path"],
                        "additionalProperties": false
                    }
                },
                "files": {
                    "type": "array",
                    "maxItems": MAX_TOOL_FILES,
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "name": { "type": "string" }
                        },
                        "required": ["path"],
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        })
    } else {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": t("Text or Markdown to send.", "要发送的文本或 Markdown。") }
            },
            "required": ["text"],
            "additionalProperties": false
        })
    };
    registry.register(
        ToolSpec::new(
            "send_message_to_user",
            t(
                "Send text, local images, or local files to the current messaging-platform conversation. Use native attachments only for content that another tool has not already emitted in this turn; the host delivers tool-emitted images automatically. This tool cannot target another conversation.",
                "向当前通讯平台会话发送文本、本地图片或本地文件。仅发送本轮中尚未由其他工具发布的原生附件；工具已发布的图片会由宿主自动投递。此工具不能指定其他会话。",
            ),
            parameters,
            move |arguments| {
                let context = context.clone();
                async move { send(arguments, context).await }
            },
        )
        .writes()
        .with_display_name(t("Send message", "发送消息")),
    );
}

fn register_mention(registry: &mut ToolRegistry, context: Arc<PlatformTurnContext>) {
    registry.register(
        ToolSpec::new(
            "qq_mention_users",
            t(
                "Make Miyu's next outgoing message in the current QQ group natively @ one or more members. Provide exact QQ IDs; use get_group_members_info first when only names are known. These explicit targets replace the automatic reply mention but preserve its message-quote behavior. This tool does not send a separate message.",
                "让 Miyu 在当前 QQ 群的下一条消息中原生艾特一名或多名成员。请提供准确 QQ 号；只知道名字时先调用 get_group_members_info。显式目标会覆盖自动回复艾特，但保留消息引用策略。此工具不会单独发送消息。",
            ),
            json!({
                "type": "object",
                "properties": {
                    "user_ids": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_TOOL_MENTIONS,
                        "items": {
                            "type": "string",
                            "pattern": "^[1-9][0-9]{4,11}$"
                        },
                        "description": t("Exact QQ IDs of the members to mention, in display order.", "要艾特的群成员 QQ 号列表，按显示顺序填写。")
                    }
                },
                "required": ["user_ids"],
                "additionalProperties": false
            }),
            move |arguments| {
                let context = context.clone();
                async move { mention(arguments, &context).await }
            },
        )
        .writes()
        .with_display_name(t("Mention group members", "艾特群成员")),
    );
}

async fn mention(arguments: Value, context: &PlatformTurnContext) -> Result<String> {
    if context.conversation.kind != ConversationKind::Group {
        bail!("mentioning users is only available in a group conversation");
    }
    let object = arguments
        .as_object()
        .context("arguments must be an object containing only user_ids")?;
    if object.len() != 1 || !object.contains_key("user_ids") {
        bail!("only user_ids may be specified");
    }
    let raw_user_ids = array(&arguments, "user_ids")?;
    if raw_user_ids.is_empty() {
        bail!("user_ids must contain at least one QQ ID");
    }
    if raw_user_ids.len() > MAX_TOOL_MENTIONS {
        bail!("at most {MAX_TOOL_MENTIONS} users may be mentioned at once");
    }

    let mut seen = HashSet::new();
    let mut user_ids = Vec::with_capacity(raw_user_ids.len());
    for (index, value) in raw_user_ids.iter().enumerate() {
        let user_id = value
            .as_str()
            .filter(|user_id| !user_id.is_empty())
            .with_context(|| format!("user_ids[{index}] must be a QQ ID string"))?;
        if !(5..=12).contains(&user_id.len())
            || !user_id
                .as_bytes()
                .first()
                .is_some_and(|byte| matches!(byte, b'1'..=b'9'))
            || !user_id.bytes().all(|byte| byte.is_ascii_digit())
        {
            bail!("user_ids[{index}] must be a 5-12 digit QQ ID");
        }
        let user_id = user_id.to_string();
        if seen.insert(user_id.clone()) {
            user_ids.push(user_id);
        }
    }
    let member_ids = context
        .group_members()
        .await
        .context("looking up current group members before mentioning them")?
        .into_iter()
        .map(|member| member.user_id)
        .collect::<HashSet<_>>();
    let missing = user_ids
        .iter()
        .filter(|user_id| !member_ids.contains(*user_id))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "these QQ IDs are not members of the current group: {}",
            missing.join(", ")
        );
    }
    context.set_explicit_response_mentions(user_ids.clone());

    Ok(json!({
        "ok": true,
        "user_ids": user_ids,
        "conversation": context.conversation.scope_key(),
        "message": t(
            "The next outgoing message will mention these members. Write that message normally.",
            "下一条发出的消息会艾特这些成员，请正常撰写消息内容。"
        )
    })
    .to_string())
}

async fn send(arguments: Value, context: Arc<PlatformTurnContext>) -> Result<String> {
    let mut segments = Vec::new();
    if let Some(text) = arguments
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        segments.push(OutboundSegment::Markdown(text.to_string()));
    }
    let images = array(&arguments, "images")?;
    if images.len() > MAX_TOOL_IMAGES {
        bail!("at most {MAX_TOOL_IMAGES} images may be sent at once");
    }
    let files = array(&arguments, "files")?;
    if files.len() > MAX_TOOL_FILES {
        bail!("at most {MAX_TOOL_FILES} files may be sent at once");
    }
    if (!images.is_empty() || !files.is_empty()) && !context.host_tools_allowed() {
        bail!("local attachments require an authorized platform administrator");
    }
    for image in images {
        let path = required_path(image, "path")?;
        let alt = image
            .get("alt")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        segments.push(OutboundSegment::ImagePath { path, alt });
    }
    for file in files {
        let path = required_path(file, "path")?;
        let name = file
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        segments.push(OutboundSegment::FilePath { path, name });
    }
    if segments.is_empty() {
        bail!("text, images, or files is required");
    }
    let receipt = context
        .send(OutboundMessage::segments(OutboundOrigin::Tool, segments))
        .await?;
    Ok(json!({
        "ok": true,
        "message_ids": receipt.message_ids,
        "conversation": context.conversation.scope_key(),
    })
    .to_string())
}

fn array<'a>(arguments: &'a Value, key: &str) -> Result<&'a [Value]> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(values)) => Ok(values),
        Some(_) => bail!("{key} must be an array"),
    }
}

fn required_path(value: &Value, key: &str) -> Result<PathBuf> {
    let raw = value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .with_context(|| format!("{key} is required"))?;
    let path = if let Some(rest) = raw.strip_prefix("~/") {
        directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().join(rest))
            .unwrap_or_else(|| PathBuf::from(raw))
    } else {
        PathBuf::from(raw)
    };
    let path = if path.is_absolute() {
        path
    } else {
        crate::tools::workspace::effective_workdir().join(path)
    };
    let metadata = std::fs::metadata(&path)
        .with_context(|| format!("reading attachment metadata: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("attachment is not a regular file: {}", path.display());
    }
    Ok(Path::new(&path).to_path_buf())
}

/// 平台侧的 token 消耗查询:读 usage-history 聚合出适合聊天气泡的摘要。
/// 口径与 WebUI 控制台一致——智能体与各平台的全部 LLM 请求都在账上。
fn register_usage_query(registry: &mut ToolRegistry, context: Arc<PlatformTurnContext>) {
    registry.register(
        ToolSpec::new(
            "query_token_usage",
            t(
                "Query Miyu's token usage statistics: totals, request count, cache hit rate, and the per-source (agent / messaging platforms) model breakdown. range: 1d (rolling 24h, default) / 7d / 30d / all.",
                "查询 Miyu 的 token 消耗统计:总量、请求数、缓存命中率,以及按来源(智能体/通讯平台)的模型构成。range 可选 1d(近 24 小时,默认)/ 7d / 30d / all。",
            ),
            json!({
                "type": "object",
                "properties": {
                    "range": {
                        "type": "string",
                        "enum": ["1d", "7d", "30d", "all"],
                        "description": t("Time range, defaults to 1d (rolling 24h).", "统计范围,默认 1d(近 24 小时)。")
                    }
                },
                "additionalProperties": false
            }),
            move |arguments| {
                let context = context.clone();
                async move { query_token_usage(arguments, context).await }
            },
        )
        .with_display_name(t("Token usage", "用量统计")),
    );
}

async fn query_token_usage(arguments: Value, context: Arc<PlatformTurnContext>) -> Result<String> {
    let range_key = arguments
        .get("range")
        .and_then(Value::as_str)
        .unwrap_or("1d")
        .to_string();
    let range = crate::state::UsageRange::parse(&range_key);
    let store = context.state_store.clone();
    let config = context.config.clone();
    let stats = tokio::task::spawn_blocking(move || store.usage_stats(range, Some(&config)))
        .await
        .context("usage stats task panicked")??;
    Ok(crate::tools::usage_query::format_usage_summary(&stats, &range_key))
}
