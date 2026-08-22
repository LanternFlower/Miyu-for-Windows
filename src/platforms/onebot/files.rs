//! 群文件的下载与本地暂存。
//!
//! 三重上限缺一不可：单文件字节数（`download_platform_file_capped` 边下边数，
//! 不信任 Content-Length）、目录总量、条目数。少了任何一重，一个群里刷文件就
//! 能把磁盘填满。超量时按时间淘汰，见 `ensure_platform_file_capacity`。

use crate::platforms::onebot::*;

pub(in crate::platforms::onebot) const MAX_INBOUND_FILE_BYTES: usize = 50 * 1024 * 1024;

pub(in crate::platforms::onebot) const FILE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

pub(in crate::platforms::onebot) const PLATFORM_FILE_STORAGE_BYTES: u64 = 1024 * 1024 * 1024;

pub(in crate::platforms::onebot) const PLATFORM_FILE_STORAGE_ENTRIES: usize = 4096;

pub(in crate::platforms::onebot) const PLATFORM_FILE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// QQ files are cached under `<cache>/platform_files/qq/`, never under the
/// durable data tree. Downloads are lazy: only `read_platform_file` asks for
/// them, so merely receiving a file costs no disk growth.
pub(in crate::platforms::onebot) fn platform_file_storage_root(base_dir: &std::path::Path) -> PathBuf {
    base_dir.join("platform_files").join("qq")
}

/// One-time best-effort move of the old eager-download cache from
/// `<data>/platform_files/` to `<cache>/platform_files/qq/`.
pub(in crate::platforms::onebot) async fn migrate_legacy_platform_file_cache(paths: &crate::paths::MiyuPaths) {
    let legacy = paths.data_dir.join("platform_files");
    if !legacy.exists() {
        return;
    }
    let target = platform_file_storage_root(&paths.cache_dir);
    let result = async {
        tokio::fs::create_dir_all(&target).await?;
        let mut entries = tokio::fs::read_dir(&legacy).await?;
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_file() {
                continue;
            }
            let destination = target.join(entry.file_name());
            if destination.exists() {
                continue;
            }
            tokio::fs::rename(entry.path(), destination).await?;
        }
        let _ = tokio::fs::remove_dir(&legacy).await;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(error = %error, legacy = %legacy.display(), "legacy platform file cache migration incomplete");
    }
}

/// 扫描配额目录:顺带清理过期文件,返回 (存量字节, 存量条数)。
pub(in crate::platforms::onebot) async fn scan_platform_file_storage(
    data_dir: &std::path::Path,
    ttl: Duration,
) -> Result<(u64, usize)> {
    let dir = platform_file_storage_root(data_dir);
    tokio::fs::create_dir_all(&dir).await?;
    let mut entries = tokio::fs::read_dir(&dir).await?;
    let mut bytes = 0_u64;
    let mut count = 0usize;
    while let Some(entry) = entries.next_entry().await? {
        let metadata = match entry.metadata().await {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };
        let expired = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > ttl);
        if expired {
            let _ = tokio::fs::remove_file(entry.path()).await;
            continue;
        }
        bytes = bytes
            .checked_add(metadata.len())
            .context("platform file storage size overflow")?;
        count = count.saturating_add(1);
    }
    Ok((bytes, count))
}

pub(in crate::platforms::onebot) async fn ensure_platform_file_capacity(
    data_dir: &std::path::Path,
    reserve: u64,
    max_bytes: u64,
    max_entries: usize,
    ttl: Duration,
) -> Result<()> {
    let (bytes, count) = scan_platform_file_storage(data_dir, ttl).await?;
    if count >= max_entries || bytes.saturating_add(reserve) > max_bytes {
        bail!("platform file storage quota is full");
    }
    Ok(())
}

pub(in crate::platforms::onebot) async fn download_platform_file_capped(
    client: &reqwest::Client,
    url: &str,
    data_dir: &std::path::Path,
    name: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Result<PathBuf> {
    let response = client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?
        .error_for_status()
        .with_context(|| format!("downloading {url}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        bail!(
            "the file is larger than the {}MB limit",
            max_bytes / 1024 / 1024
        );
    }
    let (path, mut output) = create_platform_file(data_dir, name).await?;
    let result = async {
        let mut total = 0usize;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("reading {url}"))?;
            total = total
                .checked_add(chunk.len())
                .context("platform file size overflow")?;
            if total > max_bytes {
                bail!(
                    "the file is larger than the {}MB limit",
                    max_bytes / 1024 / 1024
                );
            }
            output.write_all(&chunk).await?;
        }
        output.flush().await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        drop(output);
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error);
    }
    Ok(path)
}

/// Saves inbound bytes under `<cache>/platform_files/qq/`, keeping only
/// the basename (no path traversal) and suffixing on collision.
///
/// **返回前必须 flush。** `tokio::fs::File` 的 `write_all` 只把数据拷进内部
/// 缓冲、把真正的写 `spawn_mandatory_blocking` 扔给阻塞线程池，然后立刻返回
/// Ok（tokio `fs/file.rs` 的 `poll_write`：copy_from → spawn → `Poll::Ready`）。
/// drop 不等它完成——tokio 自己的文档写着「要保证 drop 时文件立即关闭，必须先
/// 调 flush」。
///
/// 不 flush 的后果不是「慢一点」，是**调用方拿到路径时文件可能还是空的**：
/// 入站文件的路径会直接交给模型去读。线程池空闲时看不出来，繁忙时就丢数据。
/// `download_platform_file` 那条路一直有 flush，这条漏了。
pub(in crate::platforms::onebot) async fn save_platform_file(
    data_dir: &std::path::Path,
    name: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let (path, mut output) = create_platform_file(data_dir, name).await?;
    let written = async {
        output.write_all(bytes).await?;
        output.flush().await
    }
    .await;
    if let Err(error) = written {
        drop(output);
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error).context("writing the inbound platform file");
    }
    Ok(path)
}

pub(in crate::platforms::onebot) async fn create_platform_file(
    data_dir: &std::path::Path,
    name: &str,
) -> Result<(PathBuf, tokio::fs::File)> {
    let dir = platform_file_storage_root(data_dir);
    tokio::fs::create_dir_all(&dir).await?;
    let safe = sanitize_file_name(name);
    for counter in 0..=1000 {
        let path = std::path::Path::new(&safe);
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("file");
        let file_name = match (counter, path.extension().and_then(|ext| ext.to_str())) {
            (0, _) => safe.clone(),
            (_, Some(ext)) => format!("{stem}-{counter}.{ext}"),
            (_, None) => format!("{stem}-{counter}"),
        };
        let candidate = dir.join(file_name);
        let output = match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
            .await
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("creating the inbound platform file"),
        };
        return Ok((candidate, output));
    }
    bail!("too many files with the same name")
}

pub(in crate::platforms::onebot) fn sanitize_file_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("file")
        .replace(['\0', '\n', '\r'], "");
    let trimmed = base.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return "file".to_string();
    }
    trimmed.chars().take(120).collect()
}
