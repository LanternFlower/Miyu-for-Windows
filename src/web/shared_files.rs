//! 文件分享的 HTTP 面：列表、下载（Range 流式）、删除。
//!
//! 与 artifact 路由（SQLite blob 全量响应）不同，这里的字节始终来自磁盘并
//! 流式返回——大视频既不进内存也不进库。reference 模式在**每次**下载前校验
//! size/mtime 指纹：原文件被移动/修改后返回 410，绝不流出与分享时不一致的
//! 内容。凭证与 WebUI 完全一致（`require_auth`），能打开 WebUI 就能下载。

use crate::web::*;

/// 只有这三类允许内联预览；其余一律 attachment，杜绝 HTML/SVG 之类的
/// 活性内容在 WebUI 域下渲染。
fn inline_allowed(kind: &str) -> bool {
    matches!(kind, "video" | "audio" | "image")
}

fn valid_share_id(share_id: &str) -> bool {
    !share_id.is_empty()
        && share_id.len() <= 96
        && share_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn share_json(record: &crate::state::SharedFile) -> serde_json::Value {
    json!({
        "share_id": record.share_id,
        "file_name": record.file_name,
        "title": record.title,
        "mode": record.mode,
        "kind": record.kind,
        "mime": record.mime,
        "size_bytes": record.size_bytes,
        "created_at": record.created_at,
    })
}

pub(in crate::web) async fn shared_files_list(
    State(state): State<DaemonState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let shares = state
        .state_store
        .list_shared_files()
        .map_err(ApiError::internal)?;
    Ok(axum::Json(json!({
        "shares": shares.iter().map(share_json).collect::<Vec<_>>()
    }))
    .into_response())
}

pub(in crate::web) async fn shared_file_delete(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(share_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    if !valid_share_id(&share_id) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "share not found"));
    }
    let deleted = state
        .state_store
        .delete_shared_file(&share_id)
        .map_err(ApiError::internal)?;
    if !deleted {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "share not found"));
    }
    Ok(axum::Json(json!({ "ok": true })).into_response())
}

#[derive(Deserialize)]
pub(in crate::web) struct SharedDownloadQuery {
    #[serde(default)]
    download: Option<String>,
}

pub(in crate::web) async fn shared_file_download(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(share_id): Path<String>,
    Query(query): Query<SharedDownloadQuery>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    if !valid_share_id(&share_id) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "share not found"));
    }
    let Some(record) = state
        .state_store
        .load_shared_file(&share_id)
        .map_err(ApiError::internal)?
    else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "share not found"));
    };
    let path = std::path::PathBuf::from(&record.stored_path);
    let metadata = tokio::fs::metadata(&path).await.map_err(|_| {
        ApiError::new(
            StatusCode::GONE,
            "the shared file no longer exists on disk",
        )
    })?;
    if !metadata.is_file() {
        return Err(ApiError::new(
            StatusCode::GONE,
            "the shared file no longer exists on disk",
        ));
    }
    // reference 模式:分享的是「当时那份内容」,指纹变了就拒绝——宁可 410,
    // 不流出与分享时不一致的字节。快照模式的副本只属于托管区,不校验。
    if record.mode == "reference" {
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        if metadata.len() != record.size_bytes || mtime != record.mtime_unix {
            return Err(ApiError::new(
                StatusCode::GONE,
                "the shared file has changed or been removed since it was shared; ask for it to be shared again",
            ));
        }
    }
    let total = metadata.len();
    let range = headers
        .get(axum::http::header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(|value| parse_byte_range(value, total));
    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::new(StatusCode::GONE, "the shared file no longer exists on disk"))?;
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
    let force_download = query.download.as_deref().is_some_and(|value| value == "1");
    let inline = !force_download && inline_allowed(&record.kind);
    let disposition = format!(
        "{}; filename*=UTF-8''{}",
        if inline { "inline" } else { "attachment" },
        urlencoding::encode(&record.file_name)
    );
    let response_headers = response.headers_mut();
    response_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&record.mime).map_err(ApiError::internal)?,
    );
    response_headers.insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(ApiError::internal)?,
    );
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
    response_headers.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    response_headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}
