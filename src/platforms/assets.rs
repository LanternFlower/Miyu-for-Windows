use crate::runtime::random_id;
use crate::runtime::DaemonState;
use anyhow::{bail, Context, Result};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::path::{Path as FilePath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

const LEASE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_ACTIVE_LEASES: usize = 128;
pub(crate) const MAX_LEASE_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct AssetLeaseStore {
    inner: Arc<Mutex<HashMap<String, AssetLease>>>,
}

#[derive(Clone)]
struct AssetLease {
    path: PathBuf,
    name: String,
    size: u64,
    expires_at: Instant,
    created_at: Instant,
}

pub(crate) struct AssetLeaseHandle {
    pub(crate) url: String,
}

impl AssetLeaseStore {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn create(
        &self,
        base_url: &str,
        path: &FilePath,
        display_name: &str,
    ) -> Result<AssetLeaseHandle> {
        let metadata = tokio::fs::metadata(path)
            .await
            .with_context(|| format!("reading outbound file metadata: {}", path.display()))?;
        if !metadata.is_file() {
            bail!(
                "outbound attachment is not a regular file: {}",
                path.display()
            );
        }
        if metadata.len() > MAX_LEASE_BYTES {
            bail!("outbound attachment exceeds the 50 MiB limit");
        }
        let path = tokio::fs::canonicalize(path)
            .await
            .with_context(|| format!("resolving outbound file: {}", path.display()))?;
        let now = Instant::now();
        let token = random_id("asset", 32);
        let mut leases = self.inner.lock().unwrap();
        leases.retain(|_, lease| lease.expires_at > now);
        if leases.len() >= MAX_ACTIVE_LEASES {
            if let Some(oldest) = leases
                .iter()
                .min_by_key(|(_, lease)| lease.created_at)
                .map(|(token, _)| token.clone())
            {
                leases.remove(&oldest);
            }
        }
        leases.insert(
            token.clone(),
            AssetLease {
                path,
                name: sanitize_header_filename(display_name),
                size: metadata.len(),
                expires_at: now + LEASE_TTL,
                created_at: now,
            },
        );
        Ok(AssetLeaseHandle {
            url: format!(
                "{}/api/platform-assets/{token}",
                base_url.trim_end_matches('/')
            ),
        })
    }

    fn get(&self, token: &str) -> Option<AssetLease> {
        if token.len() > 96
            || token.is_empty()
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return None;
        }
        let now = Instant::now();
        let mut leases = self.inner.lock().unwrap();
        leases.retain(|_, lease| lease.expires_at > now);
        leases.get(token).cloned()
    }
}

pub(crate) async fn platform_asset(
    State(state): State<DaemonState>,
    Path(token): Path<String>,
) -> Response {
    let Some(lease) = state.platforms.assets.get(&token) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let file = match tokio::fs::File::open(&lease.path).await {
        Ok(file) => file,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let metadata = match file.metadata().await {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_LEASE_BYTES => metadata,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    // Refuse a replaced/growing file instead of streaming different bytes
    // than the sender validated when it created the lease.
    if metadata.len() != lease.size {
        return StatusCode::CONFLICT.into_response();
    }
    let stream = ReaderStream::new(file.take(lease.size));
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, no-store, max-age=0"),
    );
    if let Ok(value) = HeaderValue::from_str(&metadata.len().to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, value);
    }
    let disposition = format!("attachment; filename=\"{}\"", lease.name);
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        response.headers_mut().insert(CONTENT_DISPOSITION, value);
    }
    response
}

fn sanitize_header_filename(name: &str) -> String {
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("file")
        .chars()
        .filter(|character| character.is_ascii_graphic() && !matches!(character, '"' | '\\' | ';'))
        .take(120)
        .collect::<String>();
    if name.is_empty() || name.starts_with('.') {
        "file".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_file_name_drops_control_and_path_content() {
        assert_eq!(sanitize_header_filename("../a b.txt"), "ab.txt");
        assert_eq!(sanitize_header_filename("报告.pdf"), "file");
        assert_eq!(sanitize_header_filename("evil\";x"), "evilx");
    }
}
