use anyhow::{bail, Result};
use serde::de::DeserializeOwned;

pub(super) const MAX_HTML_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

/// 进程级共享 HTTP 客户端：连接池/TLS 会话复用，避免各工具每次调用重建。
/// 客户端级 60s 总超时兜底；单个请求可用 `RequestBuilder::timeout` 覆盖。
pub(super) fn shared_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

pub(super) async fn read_text(response: reqwest::Response, max_bytes: usize) -> Result<String> {
    let bytes = read_bytes(response, max_bytes).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(super) async fn read_json<T>(response: reqwest::Response, max_bytes: usize) -> Result<T>
where
    T: DeserializeOwned,
{
    let bytes = read_bytes(response, max_bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) async fn read_bytes(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        bail!("response too large (exceeds configured byte limit)")
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > max_bytes.saturating_sub(body.len()) {
            bail!("response too large (exceeds configured byte limit)")
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
