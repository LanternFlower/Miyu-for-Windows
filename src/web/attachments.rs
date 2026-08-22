//! 用户上传的附件与媒体流。
//!
//! 附件 ID 是用户可控的字符串，最终会拼进文件路径，所以 `validate_attachment_id`
//! 与 `sanitize_attachment_file_name` 是安全边界而不是格式检查。
//!
//! `AttachmentRunGuard` 管的是生命周期：附件在回合真正跑起来之前只是暂存，
//! 回合失败或被撤回就该清掉，否则每次发送失败都会在磁盘上留一份垃圾。
//!
//! `media_stream` 支持 Range 请求——网页端的音视频要能拖进度条，一次性全量返
//! 回在大文件上是不可接受的。

use crate::web::*;

pub(in crate::web) const ATTACHMENT_BODY_LIMIT: usize = 10 * 1024 * 1024;

pub(in crate::web) const MAX_ATTACHMENT_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

pub(in crate::web) const MAX_TEXT_ATTACHMENT_BYTES: usize = 1024 * 1024;

pub(in crate::web) const MAX_ATTACHMENTS_PER_MESSAGE: usize = 12;

#[derive(Deserialize)]
pub(in crate::web) struct AttachmentQuery {
    pub(in crate::web) session_id: String,
}

#[derive(Deserialize)]
pub(in crate::web) struct MediaQuery {
    pub(in crate::web) path: String,
}

/// 视频扩展名 → MIME。清单外一律拒绝:这个端点只做媒体流,
/// 不做通用文件下载器(尽管登录态本就有 read_file 同级能力)。
pub(in crate::web) fn media_mime(path: &std::path::Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4v" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "mov" => Some("video/quicktime"),
        "mkv" => Some("video/x-matroska"),
        "ogv" | "ogg" => Some("video/ogg"),
        "mp3" => Some("audio/mpeg"),
        "flac" => Some("audio/flac"),
        "wav" => Some("audio/wav"),
        "m4a" => Some("audio/mp4"),
        "opus" => Some("audio/ogg"),
        _ => None,
    }
}

/// 解析 `Range: bytes=start-end`(单段)。返回 (start, inclusive_end)。
pub(in crate::web) fn parse_byte_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let spec = value.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
    let (start, end) = spec.split_once('-')?;
    if start.is_empty() {
        // 后缀形式 bytes=-N:最后 N 字节
        let suffix: u64 = end.parse().ok()?;
        if suffix == 0 || total == 0 {
            return None;
        }
        return Some((total.saturating_sub(suffix), total - 1));
    }
    let start: u64 = start.parse().ok()?;
    let end: u64 = if end.is_empty() { total.saturating_sub(1) } else { end.parse().ok()? };
    (start <= end && start < total).then(|| (start, end.min(total.saturating_sub(1))))
}

/// 本地媒体流:登录态可播放本机音视频文件,带 HTTP Range(拖进度条)。
pub(in crate::web) async fn media_stream(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<MediaQuery>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let raw = if let Some(rest) = query.path.strip_prefix("~/") {
        crate::platform_dirs::PlatformDirs::new()
            .map(|dirs| dirs.home_dir().join(rest))
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "media not found"))?
    } else {
        std::path::PathBuf::from(&query.path)
    };
    let path = tokio::fs::canonicalize(&raw)
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "media not found"))?;
    let mime = media_mime(&path)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unsupported media type"))?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "media not found"))?;
    if !metadata.is_file() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "media not found"));
    }
    let total = metadata.len();
    let range = headers
        .get(axum::http::header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(|value| parse_byte_range(value, total));
    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "media not found"))?;

    let (status, start, end) = match range {
        None => (StatusCode::OK, 0, total.saturating_sub(1)),
        Some(Some((start, end))) => (StatusCode::PARTIAL_CONTENT, start, end),
        Some(None) => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            response.headers_mut().insert(
                axum::http::header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{total}")).unwrap(),
            );
            return Ok(response);
        }
    };
    let length = if total == 0 { 0 } else { end - start + 1 };
    use tokio::io::AsyncSeekExt;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(ApiError::internal)?;
    let stream = tokio_util::io::ReaderStream::new(tokio::io::AsyncReadExt::take(file, length));
    let mut response = Response::new(axum::body::Body::from_stream(stream));
    *response.status_mut() = status;
    let response_headers = response.headers_mut();
    response_headers.insert(CONTENT_TYPE, HeaderValue::from_static(mime));
    response_headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
    response_headers.insert(
        axum::http::header::ACCEPT_RANGES,
        HeaderValue::from_static("bytes"),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        response_headers.insert(
            axum::http::header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")).unwrap(),
        );
    }
    response_headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(in crate::web) async fn upload_user_attachment(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<AttachmentQuery>,
    body: Bytes,
) -> std::result::Result<Json<SafeUserAttachment>, ApiError> {
    require_mutation(&headers, &state)?;
    let session_id =
        resolve_turn_session(&state, Some(query.session_id)).map_err(session_api_error)?;
    if body.is_empty() || body.len() > ATTACHMENT_BODY_LIMIT {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachment must be between 1 byte and 10 MiB",
        ));
    }
    let encoded_name = headers
        .get("x-miyu-filename")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "attachment filename is required"))?;
    let decoded_name = urlencoding::decode(encoded_name)
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "attachment filename is invalid"))?;
    let file_name = sanitize_attachment_file_name(&decoded_name)?;
    let (kind, mime, width, height) = inspect_user_attachment(&file_name, &body)?;
    let attachment = UserAttachment {
        attachment_id: random_id("att", 24),
        file_name,
        mime,
        kind,
        size_bytes: body.len() as u64,
        width,
        height,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let store = state.state_store.pinned(&session_id);
    store
        .purge_stale_user_attachments()
        .map_err(ApiError::internal)?;
    store
        .save_user_attachment(&attachment, &body)
        .map_err(ApiError::internal)?;
    Ok(Json(SafeUserAttachment::from(attachment)))
}

pub(in crate::web) async fn user_attachment(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(attachment_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    validate_attachment_id(&attachment_id)?;
    let Some(attachment) = state
        .state_store
        .load_user_attachment_by_id(&attachment_id)
        .map_err(ApiError::internal)?
    else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "attachment not found"));
    };
    let inline = attachment.attachment.kind == "image";
    let mut response = attachment.bytes.into_response();
    let content_type = if inline {
        attachment.attachment.mime.as_str()
    } else {
        "application/octet-stream"
    };
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_str(content_type).map_err(ApiError::internal)?,
    );
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&attachment.attachment.size_bytes.to_string())
            .map_err(ApiError::internal)?,
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        attachment_content_disposition(&attachment.attachment.file_name, inline)?,
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400"),
    );
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

pub(in crate::web) async fn delete_user_attachment(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<AttachmentQuery>,
    Path(attachment_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    require_mutation(&headers, &state)?;
    validate_attachment_id(&attachment_id)?;
    let session_id =
        resolve_turn_session(&state, Some(query.session_id)).map_err(session_api_error)?;
    let deleted = state
        .state_store
        .pinned(&session_id)
        .delete_staged_user_attachment(&attachment_id)
        .map_err(ApiError::internal)?;
    if !deleted {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "attachment not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(in crate::web) fn validate_attachment_id(attachment_id: &str) -> std::result::Result<(), ApiError> {
    if attachment_id.len() <= 96
        && !attachment_id.is_empty()
        && attachment_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Ok(());
    }
    Err(ApiError::new(StatusCode::NOT_FOUND, "attachment not found"))
}

pub(in crate::web) fn sanitize_attachment_file_name(value: &str) -> std::result::Result<String, ApiError> {
    let name = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(180)
        .collect::<String>();
    if name.is_empty() || name == "." || name == ".." {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "attachment filename is invalid",
        ));
    }
    Ok(name)
}

pub(in crate::web) fn inspect_user_attachment(
    file_name: &str,
    bytes: &[u8],
) -> std::result::Result<(String, String, u32, u32), ApiError> {
    if let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format() {
        if let Some(format) = reader.format() {
            if matches!(
                format,
                image::ImageFormat::Png
                    | image::ImageFormat::Jpeg
                    | image::ImageFormat::WebP
                    | image::ImageFormat::Gif
            ) {
                let (width, height) = reader.into_dimensions().map_err(|_| {
                    ApiError::new(StatusCode::BAD_REQUEST, "attachment image is invalid")
                })?;
                if width == 0
                    || height == 0
                    || width > 40_000
                    || height > 40_000
                    || u64::from(width) * u64::from(height) > 40_000_000
                {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "attachment image dimensions are outside the safety limit",
                    ));
                }
                return Ok((
                    "image".to_string(),
                    format.to_mime_type().to_string(),
                    width,
                    height,
                ));
            }
        }
    }
    if bytes.len() > MAX_TEXT_ATTACHMENT_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "text attachment exceeds the 1 MiB limit",
        ));
    }
    std::str::from_utf8(bytes).map_err(|_| {
        ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "attachment is not UTF-8 text",
        )
    })?;
    let extension = FilePath::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    const TEXT_EXTENSIONS: &[&str] = &[
        "txt", "md", "markdown", "json", "jsonl", "csv", "tsv", "log", "rs", "js", "jsx", "ts",
        "tsx", "py", "go", "java", "c", "cc", "cpp", "h", "hpp", "cs", "rb", "php", "swift", "kt",
        "kts", "sh", "bash", "zsh", "fish", "toml", "yaml", "yml", "xml", "html", "css", "scss",
        "sql",
    ];
    if !TEXT_EXTENSIONS.contains(&extension.as_str()) {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported attachment type",
        ));
    }
    let mime = match extension.as_str() {
        "md" | "markdown" => "text/markdown",
        "json" | "jsonl" => "application/json",
        "csv" => "text/csv",
        "html" => "text/html",
        "css" => "text/css",
        _ => "text/plain",
    };
    Ok(("text".to_string(), mime.to_string(), 0, 0))
}

pub(in crate::web) fn attachment_content_disposition(
    file_name: &str,
    inline: bool,
) -> std::result::Result<HeaderValue, ApiError> {
    let fallback = file_name
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
        .take(80)
        .collect::<String>();
    let fallback = if fallback.is_empty() {
        "attachment"
    } else {
        &fallback
    };
    let disposition = if inline { "inline" } else { "attachment" };
    let value = format!(
        "{disposition}; filename=\"{fallback}\"; filename*=UTF-8''{}",
        urlencoding::encode(file_name)
    );
    HeaderValue::from_str(&value).map_err(ApiError::internal)
}

pub(in crate::web) struct PreparedWebAttachments {
    pub(in crate::web) content: String,
    pub(in crate::web) images: Vec<Option<ImageAttachment>>,
}

pub(in crate::web) fn prepare_web_attachments(
    store: &StateStore,
    display_content: &str,
    attachment_ids: &[String],
) -> std::result::Result<PreparedWebAttachments, ApiError> {
    if attachment_ids.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("a message can include at most {MAX_ATTACHMENTS_PER_MESSAGE} attachments"),
        ));
    }
    let unique = attachment_ids.iter().collect::<HashSet<_>>();
    if unique.len() != attachment_ids.len()
        || attachment_ids
            .iter()
            .any(|id| validate_attachment_id(id).is_err())
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "attachment ids are invalid",
        ));
    }
    let attachments = store
        .load_staged_user_attachments(attachment_ids)
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
    prepare_web_attachment_data(display_content, attachments)
}

pub(in crate::web) fn prepare_web_attachment_data(
    display_content: &str,
    attachments: Vec<crate::state::UserAttachmentData>,
) -> std::result::Result<PreparedWebAttachments, ApiError> {
    if attachments.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("a message can include at most {MAX_ATTACHMENTS_PER_MESSAGE} attachments"),
        ));
    }
    let total_bytes = attachments
        .iter()
        .map(|attachment| attachment.attachment.size_bytes)
        .sum::<u64>();
    if total_bytes > MAX_ATTACHMENT_TOTAL_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachments exceed the 32 MiB per-message limit",
        ));
    }
    let mut content = if display_content.is_empty() {
        "请查看附件。".to_string()
    } else {
        display_content.to_string()
    };
    let mut images = Vec::new();
    for attachment in attachments {
        if attachment.attachment.kind == "image" {
            images.push(Some(ImageAttachment::Binary {
                mime: attachment.attachment.mime,
                data: attachment.bytes,
            }));
            continue;
        }
        let text = std::str::from_utf8(&attachment.bytes)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "text attachment is not UTF-8"))?;
        let name = escape_attachment_attribute(&attachment.attachment.file_name);
        let mime = escape_attachment_attribute(&attachment.attachment.mime);
        content.push_str(&format!(
            "\n\n<user-attachment name=\"{name}\" mime=\"{mime}\">\n{text}\n</user-attachment>"
        ));
    }
    Ok(PreparedWebAttachments { content, images })
}

pub(in crate::web) fn escape_attachment_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(in crate::web) struct AttachmentRunGuard {
    pub(in crate::web) store: StateStore,
    pub(in crate::web) run_id: Option<String>,
}

impl AttachmentRunGuard {
    pub(in crate::web) fn new(store: StateStore, run_id: Option<String>) -> Self {
        Self { store, run_id }
    }
}

impl Drop for AttachmentRunGuard {
    fn drop(&mut self) {
        if let Some(run_id) = self.run_id.as_deref() {
            let _ = self.store.release_user_attachments_for_run(run_id);
        }
    }
}
