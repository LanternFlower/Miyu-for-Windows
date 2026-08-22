//! 渲染跑在独立子进程里。
//!
//! 理由是**隔离**：字体渲染要吃任意 Markdown，一次 OOM 或 panic 不该带走整个
//! 进程。子进程有地址空间上限（`WORKER_ADDRESS_SPACE_LIMIT`）、渲染超时、空闲
//! 超时自动退出。
//!
//! 协议是手写的长度前缀帧，两侧都对帧大小设了上限（`MAX_REQUEST_FRAME_BYTES` /
//! `MAX_ERROR_FRAME_BYTES`）——协议的另一端虽然是自己人，但读一个来自管道的
//! 长度就分配那么多内存，是标准的崩法。

use crate::platforms::plugins::renderer::*;

// 10 分钟：worker 常驻约 30 MB（字体 + 塑形缓存），闲置回收后下次渲染
// 自动重拉，冷启动只多付一次字体加载。
pub(in crate::platforms::plugins::renderer) const WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub(in crate::platforms::plugins::renderer) const RENDER_TIMEOUT: Duration = Duration::from_secs(60);

// debug 二进制未优化可到 550MB+,光映射自身就会撞 512MB 上限,worker
// 秒死只留下一句 "communication failed"——开发构建放宽到 2GB。
#[cfg(not(debug_assertions))]
pub(in crate::platforms::plugins::renderer) const WORKER_ADDRESS_SPACE_LIMIT: u64 = 512 * 1024 * 1024;

#[cfg(debug_assertions)]
pub(in crate::platforms::plugins::renderer) const WORKER_ADDRESS_SPACE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

pub(in crate::platforms::plugins::renderer) const MAX_REQUEST_FRAME_BYTES: usize = 512 * 1024;

pub(in crate::platforms::plugins::renderer) const MAX_ERROR_FRAME_BYTES: usize = 64 * 1024;

pub(in crate::platforms::plugins::renderer) const MAX_RESPONSE_IMAGES: usize = 1;

pub(in crate::platforms::plugins::renderer) const WORKER_ENV: &str = "MIYU_INTERNAL_RENDERER_WORKER";

pub(in crate::platforms::plugins::renderer) const WORKER_ARG: &str = "__renderer-worker";

pub(in crate::platforms::plugins::renderer) struct WorkerProcess {
    pub(in crate::platforms::plugins::renderer) child: Child,
    pub(in crate::platforms::plugins::renderer) stdin: ChildStdin,
    pub(in crate::platforms::plugins::renderer) stdout: ChildStdout,
}

#[derive(Default)]
pub(in crate::platforms::plugins::renderer) struct WorkerSlot {
    pub(in crate::platforms::plugins::renderer) process: Option<WorkerProcess>,
    pub(in crate::platforms::plugins::renderer) idle_task: Option<tokio::task::JoinHandle<()>>,
    pub(in crate::platforms::plugins::renderer) generation: u64,
}

impl WorkerSlot {
    pub(in crate::platforms::plugins::renderer) fn cancel_idle_timer(&mut self) {
        if let Some(task) = self.idle_task.take() {
            task.abort();
        }
    }
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        self.cancel_idle_timer();
    }
}

impl WorkerProcess {
    pub(in crate::platforms::plugins::renderer) async fn spawn() -> Result<Self> {
        let executable = crate::paths::miyu_executable()?;
        let executable_for_error = executable.clone();
        let mut command = tokio::process::Command::new(executable);
        command
            .arg(WORKER_ARG)
            .env(WORKER_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command.spawn().with_context(|| {
            format!(
                "starting the long-image renderer worker ({}); \
                 if Miyu was upgraded or rebuilt while running, restart the daemon",
                executable_for_error.display()
            )
        })?;
        let stdin = child
            .stdin
            .take()
            .context("renderer worker stdin was not piped")?;
        let stdout = child
            .stdout
            .take()
            .context("renderer worker stdout was not piped")?;
        Ok(Self {
            child,
            stdin,
            stdout,
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(in crate::platforms::plugins::renderer) struct RenderRequest {
    pub(in crate::platforms::plugins::renderer) markdown: String,
    pub(in crate::platforms::plugins::renderer) config: RenderConfig,
}

#[derive(Debug)]
pub(in crate::platforms::plugins::renderer) enum WorkerExchangeError {
    Transport(anyhow::Error),
    Render(String),
}

pub(in crate::platforms::plugins::renderer) async fn exchange_with_worker(
    worker: &mut WorkerProcess,
    request: &RenderRequest,
) -> std::result::Result<Vec<RenderedImage>, WorkerExchangeError> {
    let payload = serde_json::to_vec(request)
        .map_err(|error| WorkerExchangeError::Transport(error.into()))?;
    write_frame(&mut worker.stdin, &payload)
        .await
        .map_err(WorkerExchangeError::Transport)?;
    tokio::io::AsyncWriteExt::flush(&mut worker.stdin)
        .await
        .map_err(|error| WorkerExchangeError::Transport(error.into()))?;
    read_worker_response(&mut worker.stdout).await
}

pub(in crate::platforms::plugins::renderer) async fn stop_worker(mut worker: WorkerProcess) {
    let _ = worker.child.kill().await;
    let _ = worker.child.wait().await;
}

pub(crate) fn renderer_worker_requested() -> bool {
    std::env::var_os(WORKER_ENV).as_deref() == Some(std::ffi::OsStr::new("1"))
        && std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new(WORKER_ARG))
}

pub(crate) async fn run_renderer_worker() -> Result<()> {
    apply_worker_address_space_limit()?;
    let mut renderer = RendererState::new()?;
    let mut input = tokio::io::stdin();
    let mut output = tokio::io::stdout();

    loop {
        let payload = match tokio::time::timeout(
            WORKER_IDLE_TIMEOUT,
            read_frame(&mut input, MAX_REQUEST_FRAME_BYTES),
        )
        .await
        {
            Err(_) => return Ok(()),
            Ok(Ok(Some(payload))) => payload,
            Ok(Ok(None)) => return Ok(()),
            Ok(Err(error)) => return Err(error),
        };
        let result = serde_json::from_slice::<RenderRequest>(&payload)
            .context("decoding the renderer request")
            .and_then(|request| {
                validate_markdown(&request.markdown)?;
                let config = NormalizedConfig::new(&request.config);
                let blocks = collect_blocks(&request.markdown);
                let palette = Palette::for_theme(&config.theme);
                renderer.render(
                    blocks,
                    &config,
                    palette,
                    markdown_contains_emoji(&request.markdown),
                )
            });
        write_worker_response(&mut output, result).await?;
        tokio::io::AsyncWriteExt::flush(&mut output).await?;
    }
}

#[cfg(unix)]
pub(in crate::platforms::plugins::renderer) fn apply_worker_address_space_limit() -> Result<()> {
    let limit = libc::rlimit {
        rlim_cur: WORKER_ADDRESS_SPACE_LIMIT as libc::rlim_t,
        rlim_max: WORKER_ADDRESS_SPACE_LIMIT as libc::rlim_t,
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) } != 0 {
        return Err(io::Error::last_os_error()).context("limiting renderer worker address space");
    }
    Ok(())
}

#[cfg(not(unix))]
pub(in crate::platforms::plugins::renderer) fn apply_worker_address_space_limit() -> Result<()> {
    Ok(())
}

pub(in crate::platforms::plugins::renderer) async fn write_frame<W>(writer: &mut W, payload: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if payload.len() > MAX_REQUEST_FRAME_BYTES {
        bail!("renderer request frame exceeds the {MAX_REQUEST_FRAME_BYTES}-byte limit");
    }
    let length = u32::try_from(payload.len()).context("renderer request frame is too large")?;
    tokio::io::AsyncWriteExt::write_all(writer, &length.to_be_bytes()).await?;
    tokio::io::AsyncWriteExt::write_all(writer, payload).await?;
    Ok(())
}

pub(in crate::platforms::plugins::renderer) async fn read_frame<R>(reader: &mut R, limit: usize) -> Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut length = [0_u8; 4];
    match tokio::io::AsyncReadExt::read_exact(reader, &mut length).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_be_bytes(length) as usize;
    if length > limit {
        bail!("renderer frame exceeds the {limit}-byte limit");
    }
    let mut payload = vec![0_u8; length];
    tokio::io::AsyncReadExt::read_exact(reader, &mut payload).await?;
    Ok(Some(payload))
}

pub(in crate::platforms::plugins::renderer) async fn write_worker_response<W>(writer: &mut W, result: Result<Vec<RenderedImage>>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    match result {
        Ok(images) => {
            if images.len() > MAX_RESPONSE_IMAGES {
                bail!("renderer returned more than {MAX_RESPONSE_IMAGES} image");
            }
            tokio::io::AsyncWriteExt::write_all(writer, &[0]).await?;
            write_u32(writer, images.len(), "renderer image count").await?;
            for image in images {
                validate_page_dimensions(image.width, image.height)?;
                if image.png.len() > MAX_PAGE_PNG_BYTES {
                    bail!("renderer returned a PNG larger than its configured limit");
                }
                write_u32_value(writer, image.width).await?;
                write_u32_value(writer, image.height).await?;
                write_sized_bytes(writer, image.mime.as_bytes(), 64, "renderer MIME type").await?;
                write_sized_bytes(
                    writer,
                    &image.png,
                    MAX_PAGE_PNG_BYTES,
                    "renderer PNG payload",
                )
                .await?;
            }
        }
        Err(error) => {
            tokio::io::AsyncWriteExt::write_all(writer, &[1]).await?;
            let mut message = format!("{error:#}");
            if message.len() > MAX_ERROR_FRAME_BYTES {
                let mut end = MAX_ERROR_FRAME_BYTES;
                while !message.is_char_boundary(end) {
                    end = end.saturating_sub(1);
                }
                message.truncate(end);
            }
            write_sized_bytes(
                writer,
                message.as_bytes(),
                MAX_ERROR_FRAME_BYTES,
                "renderer error",
            )
            .await?;
        }
    }
    Ok(())
}

pub(in crate::platforms::plugins::renderer) async fn read_worker_response<R>(
    reader: &mut R,
) -> std::result::Result<Vec<RenderedImage>, WorkerExchangeError>
where
    R: AsyncRead + Unpin,
{
    let status = read_byte(reader)
        .await
        .map_err(WorkerExchangeError::Transport)?;
    match status {
        0 => {
            let count = read_u32(reader)
                .await
                .map_err(WorkerExchangeError::Transport)? as usize;
            if count > MAX_RESPONSE_IMAGES {
                return Err(WorkerExchangeError::Transport(anyhow!(
                    "renderer response contains too many images"
                )));
            }
            let mut images = Vec::with_capacity(count);
            let mut total_png_bytes = 0_usize;
            for _ in 0..count {
                let width = read_u32(reader)
                    .await
                    .map_err(WorkerExchangeError::Transport)?;
                let height = read_u32(reader)
                    .await
                    .map_err(WorkerExchangeError::Transport)?;
                validate_page_dimensions(width, height).map_err(WorkerExchangeError::Transport)?;
                let mime = read_sized_bytes(reader, 64)
                    .await
                    .map_err(WorkerExchangeError::Transport)?;
                let mime = String::from_utf8(mime)
                    .context("renderer returned a non-UTF-8 MIME type")
                    .map_err(WorkerExchangeError::Transport)?;
                let png = read_sized_bytes(reader, MAX_PAGE_PNG_BYTES)
                    .await
                    .map_err(WorkerExchangeError::Transport)?;
                total_png_bytes = total_png_bytes
                    .checked_add(png.len())
                    .context("renderer PNG byte count overflowed")
                    .map_err(WorkerExchangeError::Transport)?;
                if total_png_bytes > MAX_TOTAL_PNG_BYTES {
                    return Err(WorkerExchangeError::Transport(anyhow!(
                        "renderer response exceeds the total PNG byte limit"
                    )));
                }
                images.push(RenderedImage {
                    mime,
                    png,
                    width,
                    height,
                });
            }
            Ok(images)
        }
        1 => {
            let message = read_sized_bytes(reader, MAX_ERROR_FRAME_BYTES)
                .await
                .map_err(WorkerExchangeError::Transport)?;
            let message = String::from_utf8_lossy(&message).into_owned();
            Err(WorkerExchangeError::Render(message))
        }
        value => Err(WorkerExchangeError::Transport(anyhow!(
            "renderer response has unknown status byte {value}"
        ))),
    }
}

pub(in crate::platforms::plugins::renderer) async fn write_u32<W>(writer: &mut W, value: usize, label: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let value = u32::try_from(value).with_context(|| format!("{label} does not fit in u32"))?;
    write_u32_value(writer, value).await
}

pub(in crate::platforms::plugins::renderer) async fn write_u32_value<W>(writer: &mut W, value: u32) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    tokio::io::AsyncWriteExt::write_all(writer, &value.to_be_bytes()).await?;
    Ok(())
}

pub(in crate::platforms::plugins::renderer) async fn read_u32<R>(reader: &mut R) -> Result<u32>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = [0_u8; 4];
    tokio::io::AsyncReadExt::read_exact(reader, &mut bytes).await?;
    Ok(u32::from_be_bytes(bytes))
}

pub(in crate::platforms::plugins::renderer) async fn read_byte<R>(reader: &mut R) -> Result<u8>
where
    R: AsyncRead + Unpin,
{
    let mut byte = [0_u8; 1];
    tokio::io::AsyncReadExt::read_exact(reader, &mut byte).await?;
    Ok(byte[0])
}

pub(in crate::platforms::plugins::renderer) async fn write_sized_bytes<W>(writer: &mut W, bytes: &[u8], limit: usize, label: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if bytes.len() > limit {
        bail!("{label} exceeds the {limit}-byte limit");
    }
    write_u32(writer, bytes.len(), label).await?;
    tokio::io::AsyncWriteExt::write_all(writer, bytes).await?;
    Ok(())
}

pub(in crate::platforms::plugins::renderer) async fn read_sized_bytes<R>(reader: &mut R, limit: usize) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let length = read_u32(reader).await? as usize;
    if length > limit {
        bail!("renderer response field exceeds the {limit}-byte limit");
    }
    let mut bytes = vec![0_u8; length];
    tokio::io::AsyncReadExt::read_exact(reader, &mut bytes).await?;
    Ok(bytes)
}
