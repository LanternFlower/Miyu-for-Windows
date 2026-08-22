//! 入站图片的取回与合并。
//!
//! 复杂度全在**引用消息**上：用户回复一张图并问「这是什么」，图不在当前消息
//! 里。`merge_quoted_message_images` 去把被引用的消息捞回来
//! （`QUOTED_MESSAGE_LOOKUP_TIMEOUT` 卡住这次额外往返），再按顺序并进当前消息。
//!
//! 合并要保序也要去重：同一张图既在引用里又在当前消息里时，只该给模型一份。

use crate::platforms::onebot::*;

pub(in crate::platforms::onebot) const IMAGE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15);

pub(in crate::platforms::onebot) const QUOTED_MESSAGE_LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

pub(in crate::platforms::onebot) fn value_id_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(in crate::platforms::onebot) fn value_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
}

pub(in crate::platforms::onebot) async fn get_message_data(
    conn: &ConnectionHandle,
    message_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let message_id = message_id.trim();
    if message_id.is_empty() || message_id.len() > MAX_ONEBOT_ID_BYTES {
        bail!("invalid OneBot message id");
    }
    conn.call_api_with_timeout(
        "get_msg",
        json!({ "message_id": onebot_id_value(message_id) }),
        timeout,
    )
    .await
}

/// Adds images from exactly one quoted message. A nested `reply` segment in
/// the fetched message is intentionally ignored, preventing recursive lookup.
pub(in crate::platforms::onebot) async fn merge_quoted_message_images(
    conn: &ConnectionHandle,
    current_message_id: &str,
    parsed: &mut InboundMessage,
    quoted_message_data: Option<&Value>,
) -> Result<usize> {
    let Some(quoted_message_id) = parsed.reply_to_message_id.clone() else {
        return Ok(0);
    };
    if quoted_message_id == current_message_id
        || parsed.images.len() >= MAX_INBOUND_IMAGES
        || parsed
            .images
            .iter()
            .map(MediaRef::inline_bytes)
            .sum::<usize>()
            >= MAX_INBOUND_IMAGE_TOTAL_BYTES
    {
        return Ok(0);
    }

    let fetched;
    let data = if let Some(data) = quoted_message_data {
        data
    } else {
        fetched = get_message_data(conn, &quoted_message_id, QUOTED_MESSAGE_LOOKUP_TIMEOUT).await?;
        &fetched
    };
    if data
        .get("message_id")
        .and_then(value_id_string)
        .is_some_and(|returned_id| returned_id != quoted_message_id)
    {
        bail!("OneBot get_msg returned a different message id");
    }
    let before = parsed.images.len();
    let unresolved =
        append_message_image_sources(parsed, data.get("message"), data.get("raw_message"));
    let lookups = unresolved.into_iter().map(|file| async move {
        let result = conn.call_api("get_image", json!({ "file": &file })).await;
        (file, result)
    });
    for (file, result) in join_all(lookups).await {
        match result {
            Ok(data) => {
                append_resolved_quoted_image(parsed, &data);
            }
            Err(error) => {
                tracing::warn!(
                    target: "miyu::qq",
                    error = %error,
                    image_file = %file,
                    "{}",
                    t("OneBot get_image lookup for a quoted image failed", "OneBot 查询引用图片的 get_image 失败")
                );
            }
        }
    }
    Ok(parsed.images.len().saturating_sub(before))
}

pub(in crate::platforms::onebot) async fn resolve_current_message_images(conn: &ConnectionHandle, parsed: &mut InboundMessage) {
    let unresolved = std::mem::take(&mut parsed.unresolved_image_files);
    let lookups = unresolved.into_iter().map(|file| async move {
        let result = conn
            .call_api_with_timeout(
                "get_image",
                json!({ "file": &file }),
                QUOTED_MESSAGE_LOOKUP_TIMEOUT,
            )
            .await;
        (file, result)
    });
    for (file, result) in join_all(lookups).await {
        match result {
            Ok(data) => {
                append_resolved_quoted_image(parsed, &data);
            }
            Err(error) => {
                tracing::warn!(
                    target: "miyu::qq",
                    error = %error,
                    image_file = %file,
                    "{}",
                    t("OneBot get_image lookup for an inbound image failed", "OneBot 查询传入图片的 get_image 失败")
                );
            }
        }
    }
}

pub(in crate::platforms::onebot) fn append_resolved_quoted_image(parsed: &mut InboundMessage, data: &Value) -> bool {
    let before = parsed.images.len();
    push_inbound_image_source(
        parsed,
        data.get("file").and_then(Value::as_str).unwrap_or(""),
        data.get("url").and_then(Value::as_str),
    );
    if parsed.images.len() == before {
        if let Some(encoded) = data.get("base64").and_then(Value::as_str) {
            push_inbound_base64(parsed, encoded);
        }
    }
    parsed.images.len() > before
}

pub(in crate::platforms::onebot) struct PreparedInboundImages {
    pub(in crate::platforms::onebot) attachments: Vec<Option<ImageAttachment>>,
    pub(in crate::platforms::onebot) attempted: usize,
    pub(in crate::platforms::onebot) failed: usize,
    pub(in crate::platforms::onebot) duplicates: usize,
    pub(in crate::platforms::onebot) total_bytes: usize,
}

pub(in crate::platforms::onebot) async fn prepare_inbound_images(
    state: &DaemonState,
    media_refs: Vec<MediaRef>,
) -> Result<PreparedInboundImages> {
    let attempted = media_refs.len().min(MAX_INBOUND_IMAGES);
    let mut attachments = Vec::with_capacity(attempted);
    let mut failed = 0usize;
    let mut duplicates = 0usize;
    let mut total_bytes = 0usize;
    let mut seen_content = HashSet::<[u8; 32]>::with_capacity(attempted);

    for media in media_refs.into_iter().take(MAX_INBOUND_IMAGES) {
        let remaining = MAX_INBOUND_IMAGE_TOTAL_BYTES.saturating_sub(total_bytes);
        if remaining == 0 {
            failed += 1;
            continue;
        }
        let maximum = MAX_INBOUND_IMAGE_BYTES.min(remaining);
        let bytes = match media {
            MediaRef::Bytes(bytes) if bytes.len() <= maximum => bytes,
            MediaRef::Bytes(_) => {
                failed += 1;
                continue;
            }
            MediaRef::Url(url) => {
                let http = state.platforms.http_client()?;
                match download_capped(&http, &url, maximum, IMAGE_DOWNLOAD_TIMEOUT).await {
                    Ok((bytes, _)) => bytes,
                    Err(error) => {
                        failed += 1;
                        tracing::warn!(error = %error, "{}", t("OneBot image download failed", "OneBot 图片下载失败"));
                        continue;
                    }
                }
            }
        };
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if !seen_content.insert(digest) {
            duplicates += 1;
            continue;
        }
        total_bytes += bytes.len();
        let mime = sniff_image_mime(&bytes).to_string();
        attachments.push(Some(ImageAttachment::Binary { mime, data: bytes }));
    }

    Ok(PreparedInboundImages {
        attachments,
        attempted,
        failed,
        duplicates,
        total_bytes,
    })
}

pub(in crate::platforms::onebot) fn image_only_prompt(count: usize) -> String {
    // 该占位文本进模型上下文,恒英文;不随 UI locale 变。
    if count == 1 {
        "(The user sent 1 image. Inspect it and respond naturally.)".to_string()
    } else {
        format!("(The user sent {count} images. Inspect them and respond naturally.)")
    }
}

pub(in crate::platforms::onebot) fn quoted_image_prompt(count: usize) -> String {
    if crate::i18n::is_zh() {
        format!("\n（输入图片中有 {count} 张来自对方引用的消息。）")
    } else if count == 1 {
        "\n(1 input image came from the message the user quoted.)".to_string()
    } else {
        format!("\n({count} input images came from the message the user quoted.)")
    }
}
