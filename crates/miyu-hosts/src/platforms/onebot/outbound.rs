//! 出站消息的分帧与投递。
//!
//! 长回复要切成多帧发（`push_message_frame` / `append_text_chunks`），于是「发
//! 送失败」不再是布尔值：可能前三帧成功第四帧超时。`partial_send_error` 保留
//! 这个区分——对用户来说「一条没发出去」和「发了一半」是两回事。
//!
//! 超时按内容量算（`send_timeout_for`）：一张大图和一行字用同一个超时，要么小
//! 图等太久，要么大图必然失败。

use crate::platforms::onebot::*;

pub(in crate::platforms::onebot) const MAX_OUTBOUND_IMAGE_BYTES: usize = 20 * 1024 * 1024;

pub(in crate::platforms::onebot) const MAX_OUTBOUND_IMAGE_DIMENSION: u32 = 16_384;

pub(in crate::platforms::onebot) const MAX_OUTBOUND_IMAGE_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// 校验字节可解码为图片(解码结果仅校验即丢)。带解码限额:几十 KB 的
/// 30000×30000 像素炸弹解压时会分配数 GB,分配失败直接 abort 全进程。
/// 同步解码放 spawn_blocking,不占 actor 单线程 runtime。
pub(in crate::platforms::onebot) async fn validate_outbound_image(
    bytes: Vec<u8>,
    path: PathBuf,
) -> Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .with_context(|| format!("decoding image {}", path.display()))?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_OUTBOUND_IMAGE_DIMENSION);
        limits.max_image_height = Some(MAX_OUTBOUND_IMAGE_DIMENSION);
        limits.max_alloc = Some(MAX_OUTBOUND_IMAGE_DECODE_ALLOC);
        reader.limits(limits);
        reader
            .decode()
            .with_context(|| format!("decoding image {}", path.display()))?;
        Ok(bytes)
    })
    .await
    .context("outbound image validation task failed")?
}

pub(in crate::platforms::onebot) const MAX_OUTBOUND_FILE_BYTES: usize = 50 * 1024 * 1024;

/// Backstop for attachment sends (see `send_timeout_for`). Not a budget: it
/// only exists so a connected-but-silent NapCat cannot wedge a conversation.
pub(in crate::platforms::onebot) const MAX_SEND_TIMEOUT: Duration = Duration::from_secs(180);

pub(in crate::platforms::onebot) struct MessageFrame {
    pub(in crate::platforms::onebot) segments: Vec<Value>,
    pub(in crate::platforms::onebot) image_digests: Vec<blake3::Hash>,
}

pub(in crate::platforms::onebot) fn push_message_frame(
    frames: &mut Vec<MessageFrame>,
    current: &mut Vec<Value>,
    current_image_digests: &mut Vec<blake3::Hash>,
) {
    if current.is_empty() {
        return;
    }
    frames.push(MessageFrame {
        segments: std::mem::take(current),
        image_digests: std::mem::take(current_image_digests),
    });
}

pub(in crate::platforms::onebot) fn append_text_chunks(
    frames: &mut Vec<MessageFrame>,
    current: &mut Vec<Value>,
    current_image_digests: &mut Vec<blake3::Hash>,
    text: &str,
    max_reply_chars: usize,
) {
    let chunks = split_reply(text, max_reply_chars);
    let count = chunks.len();
    for (index, chunk) in chunks.into_iter().enumerate() {
        current.push(text_segment(&chunk));
        if index + 1 < count {
            push_message_frame(frames, current, current_image_digests);
        }
    }
}

pub(in crate::platforms::onebot) fn partial_send_error(
    error: anyhow::Error,
    receipt: SendReceipt,
) -> anyhow::Error {
    if receipt.has_delivery() {
        anyhow::Error::new(PartialSendError::new(error, receipt))
    } else {
        error
    }
}

/// Sends carrying base64 images need far longer than a plain text call: a
/// 2 MiB picture is ~2.9 MB of JSON that NapCat has to receive, decode and
/// upload to QQ. Timing out early is worse than waiting — the message is
/// still delivered, but Miyu treats the send as failed and posts the plain
/// text fallback, so the group gets the picture *and* the text.
///
/// Size-scaling the budget only moved the cliff, and it moved it unevenly: the
/// old `div_ceil` step gave 0.99 MiB the same 30s as 64 KiB, so payloads just
/// under a megabyte boundary had the tightest work-to-budget ratio of all. An
/// attachment send now simply waits for NapCat instead of guessing how long it
/// should take.
///
/// `MAX_SEND_TIMEOUT` stays as a backstop rather than a budget. Losing the
/// connection already frees an in-flight call — `connection_loop` explicitly
/// drains the per-connection `pending` map on exit (clones of the handle held
/// by message tasks keep the Arc alive, so dropping alone would not do it),
/// so every waiting `oneshot` resolves immediately. The backstop only covers
/// a NapCat that stays connected but never answers this one echo, which would
/// otherwise wedge the conversation forever (same-conversation turns are
/// serialized and each in-flight message holds one of `MAX_IN_FLIGHT_MESSAGES`).
pub(in crate::platforms::onebot) fn send_timeout_for(segments: &[Value]) -> Duration {
    let carries_attachment = segments.iter().any(|segment| {
        segment
            .get("data")
            .and_then(|data| data.get("file"))
            .and_then(Value::as_str)
            .is_some_and(|file| !file.is_empty())
    });
    if carries_attachment {
        MAX_SEND_TIMEOUT
    } else {
        API_CALL_TIMEOUT
    }
}

/// 语音消息段。NapCat 收到 wav/mp3 会自己转 silk(需要它那边有 ffmpeg)。
pub(in crate::platforms::onebot) fn record_segment(bytes: &[u8]) -> Value {
    json!({
        "type": "record",
        "data": { "file": format!("base64://{}", BASE64.encode(bytes)) },
    })
}

pub(in crate::platforms::onebot) fn image_segment(bytes: &[u8]) -> Value {
    json!({
        "type": "image",
        "data": { "file": format!("base64://{}", BASE64.encode(bytes)) },
    })
}

pub(in crate::platforms::onebot) async fn read_file_capped(
    path: &std::path::Path,
    cap: usize,
) -> Result<Vec<u8>> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening attachment: {}", path.display()))?;
    let metadata = file
        .metadata()
        .await
        .with_context(|| format!("reading attachment metadata: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("attachment is not a regular file: {}", path.display());
    }
    if metadata.len() > cap as u64 {
        bail!("attachment exceeds the {} MiB limit", cap / 1024 / 1024);
    }
    let limit = u64::try_from(cap.saturating_add(1)).unwrap_or(u64::MAX);
    let mut reader = file.take(limit);
    let mut bytes = Vec::with_capacity(metadata.len().min(cap as u64) as usize);
    reader
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("reading attachment: {}", path.display()))?;
    if bytes.len() > cap {
        bail!("attachment exceeds the {} MiB limit", cap / 1024 / 1024);
    }
    Ok(bytes)
}

pub(in crate::platforms::onebot) async fn deliver_dispatch(
    state: &DaemonState,
    context: &Arc<PlatformTurnContext>,
    dispatch: TurnDispatch,
) -> Result<bool> {
    match dispatch {
        TurnDispatch::Cancelled => {
            context.after_turn_aborted().await;
            tracing::debug!(
                target: "miyu::qq",
                conversation_kind = context.conversation.kind.as_str(),
                "{}",
                t("OneBot turn cancelled; nothing to deliver", "OneBot 回合已取消,无需投递")
            );
            return Ok(false);
        }
        TurnDispatch::Failed(message) => {
            context.after_turn_aborted().await;
            if context.conversation.kind == ConversationKind::Group {
                tracing::info!(
                    target: "miyu::qq",
                    error = %message,
                    "{}",
                    t("suppressed an internal OneBot group error", "已抑制 OneBot 群聊内部错误")
                );
                return Ok(false);
            }
            context
                .send_bypass_plugins(OutboundMessage::text(
                    OutboundOrigin::Command,
                    format!("{}{message}", t("Something went wrong: ", "出错了：")),
                ))
                .await?;
        }
        TurnDispatch::Completed(mut outcome) => {
            if context.turn_is_superseded() {
                context.after_turn_aborted().await;
                return Ok(false);
            }
            let mut segments = Vec::new();
            // 表情包单独成一条消息(09-21 用户要求):真人不会把一句话和一个表情
            // 塞进同一条。生图/图表不拆——「给你画了这个」配图在一条里读着正常。
            let mut meme_segments = Vec::new();
            let reply_text = final_reply_text(&outcome);
            let delivered_image_digests = context.delivered_image_digests();
            let mut image_digests = delivered_image_digests.clone();
            let mut matched_delivered_image = false;
            let mut unresolved_image_count = 0usize;
            let mut image_count = 0usize;
            for asset_id in &outcome.image_assets {
                match state.state_store.load_image_asset(asset_id) {
                    Ok(Some(asset)) => {
                        let digest = blake3::hash(&asset.bytes);
                        if !image_digests.insert(digest) {
                            let already_delivered = delivered_image_digests.contains(&digest);
                            if already_delivered {
                                matched_delivered_image = true;
                            }
                            tracing::debug!(
                                target: "miyu::qq",
                                asset_id,
                                "{}",
                                if already_delivered {
                                    t(
                                        "suppressed a OneBot reply image already delivered to this conversation",
                                        "已抑制本会话中先前已投递的 OneBot 回复图片",
                                    )
                                } else {
                                    t(
                                        "suppressed a duplicate OneBot reply image",
                                        "已抑制重复的 OneBot 回复图片",
                                    )
                                }
                            );
                            continue;
                        }
                        let segment = OutboundSegment::ImageBytes {
                            mime: asset.asset.mime,
                            data: Arc::from(asset.bytes),
                            alt: asset.asset.alt,
                        };
                        if outcome.meme_assets.contains(asset_id) {
                            meme_segments.push(segment);
                        } else {
                            segments.push(segment);
                        }
                        image_count += 1;
                    }
                    Ok(None) => {
                        unresolved_image_count += 1;
                        tracing::warn!(
                            target: "miyu::qq",
                            asset_id,
                            "{}",
                            t(
                                "a OneBot reply image asset was not found",
                                "未找到 OneBot 回复图片资源",
                            )
                        );
                    }
                    Err(error) => {
                        unresolved_image_count += 1;
                        tracing::warn!(error = %error, asset_id, "{}", t("loading an image asset for OneBot failed", "为 OneBot 加载图片资源失败"));
                    }
                }
            }
            if matched_delivered_image && image_count == 0 && unresolved_image_count == 0 {
                outcome.final_reply_already_sent = true;
            }
            let readable = crate::platforms::format_platform_final_reply_log(
                &outcome,
                context,
                &reply_text,
                image_count,
            );
            // 零宽空格之类的"看起来是空"也算空,别发空气泡。
            if crate::platforms::visibly_blank(&reply_text) {
            } else if context.repeats_delivered_reply_text(&reply_text) {
                // 工具(send_message_to_user)本回合已经把这句话发出去了,最终
                // 回复再发就是用户看到的"重复发送"。图片闸在上面同样处理。
                tracing::info!(
                    target: "miyu::qq",
                    "{}",
                    t(
                        "suppressed a OneBot final reply already delivered by a tool this turn",
                        "已抑制本回合工具已投递过的 OneBot 最终回复文本",
                    )
                );
                if segments.is_empty() {
                    outcome.final_reply_already_sent = true;
                }
            } else {
                segments.insert(0, OutboundSegment::Markdown(reply_text));
            }
            if segments.is_empty() && meme_segments.is_empty() {
                if outcome.final_reply_already_sent {
                    tracing::info!(target: "miyu::qq", "\n{readable}");
                    return Ok(true);
                }
                tracing::info!(
                    target: "miyu::qq",
                    "{}",
                    t("suppressed an empty OneBot model reply", "已抑制空的 OneBot 模型回复")
                );
                return Ok(false);
            }
            send_reply_and_memes(context, segments, meme_segments).await?;
            tracing::info!(target: "miyu::qq", "\n{readable}");
        }
    }
    Ok(true)
}

/// 正文与表情包分两条发(09-21 用户拍板)。
///
/// 三条规矩，都是为了像人：
/// - **分开发**：真人不会把一句话和一个表情塞进同一条消息。
/// - **顺序随机**：有时先说话再补表情，有时先甩表情再解释。
/// - **停顿随机**：连着发两条仍然像机器一次吐完。
///
/// 表情那条永远带 [`ResponseTarget::silent`]：它不引用、不艾特，也**不消耗**
/// 本回合预留的那个引用目标——否则「先发表情」的那一半会把引用挂到表情上，
/// 正文反而没有。
async fn send_reply_and_memes(
    context: &crate::platforms::PlatformTurnContext,
    text_segments: Vec<OutboundSegment>,
    meme_segments: Vec<OutboundSegment>,
) -> Result<()> {
    let mut text_message = (!text_segments.is_empty())
        .then(|| OutboundMessage::segments(OutboundOrigin::FinalReply, text_segments));
    let mut meme_message = (!meme_segments.is_empty()).then(|| {
        let mut message = OutboundMessage::segments(OutboundOrigin::FinalReply, meme_segments);
        message.response_target = Some(ResponseTarget::silent());
        message
    });
    let meme_first = meme_message.is_some() && text_message.is_some() && rand::random::<bool>();
    let first = if meme_first {
        meme_message.take()
    } else {
        text_message.take()
    };
    let second = if meme_first {
        text_message.take()
    } else {
        meme_message.take()
    };
    if let Some(message) = first {
        context.send(message).await?;
    }
    if let Some(message) = second {
        tokio::time::sleep(meme_gap()).await;
        context.send(message).await?;
    }
    Ok(())
}

/// 两条之间的停顿范围：「看得出是两次动作、又不至于让人以为掉线」。
const MEME_GAP_MIN: Duration = Duration::from_millis(400);
const MEME_GAP_MAX: Duration = Duration::from_millis(1200);

fn meme_gap() -> Duration {
    // 测试里不真睡：被测的是「拆不拆、谁在前」，不是睡多久。睡满真停顿会让
    // 一条随机性用例跑两分钟(实测 122s)。范围本身由单测直接钉常量。
    if cfg!(test) {
        return Duration::from_millis(1);
    }
    let span = (MEME_GAP_MAX.as_millis() - MEME_GAP_MIN.as_millis()) as u64;
    MEME_GAP_MIN + Duration::from_millis(rand::random::<u64>() % (span + 1))
}

#[cfg(test)]
mod meme_gap_tests {
    use super::*;

    /// 停顿范围写错(min>max 会让取模 panic、或范围小到看不出是两次动作)在
    /// 集成用例里看不出来——那边不真睡。
    #[test]
    fn the_gap_range_stays_human() {
        assert!(MEME_GAP_MIN < MEME_GAP_MAX);
        assert!(MEME_GAP_MIN >= Duration::from_millis(300));
        assert!(MEME_GAP_MAX <= Duration::from_secs(3));
    }
}

pub(in crate::platforms::onebot) fn final_reply_text(
    outcome: &crate::platforms::TurnOutcome,
) -> String {
    crate::platforms::cut_suppressed_ranges(&outcome.text, &outcome.suppressed_reply_ranges)
}

pub(in crate::platforms::onebot) fn text_segment(text: &str) -> Value {
    json!({ "type": "text", "data": { "text": text } })
}
