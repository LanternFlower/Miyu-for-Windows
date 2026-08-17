use anyhow::{anyhow, bail, Context, Result};
use cosmic_text::{
    Align as TextAlign, Attrs, Buffer, Color, Family, FontSystem, LayoutGlyph, Metrics, Shaping,
    Style as FontStyle, SwashCache, Weight, Wrap,
};
use fontdb::Database as FontDatabase;
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Pixel as _, Rgba, RgbaImage};
use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use unicode_segmentation::UnicodeSegmentation;

const MAX_INPUT_CHARS: usize = 20_000;
const MAX_PAGE_PIXELS: u64 = 20_000_000;
const MAX_TOTAL_PAGE_PIXELS: u64 = 48_000_000;
const MAX_PAGE_PNG_BYTES: usize = 20 * 1024 * 1024;
const MAX_TOTAL_PNG_BYTES: usize = 48 * 1024 * 1024;
const MIN_CONFIGURED_HEIGHT: u32 = 1000;
const MIN_RENDERED_HEIGHT: u32 = 360;
const MAX_PAGE_HEIGHT: u32 = 5000;
const MAX_CACHED_GLYPHS: usize = 2048;
const MAX_CUSTOM_FONT_FILES: usize = 8;
const COLUMN_WIDTH: u32 = 960;
const COLUMN_GAP: u32 = 32;
const TARGET_ASPECT_RATIO: f32 = 4.0 / 3.0;
const ASPECT_TIE_EPSILON: f32 = 0.01;
const TABLE_CELL_PADDING: u32 = 14;
const WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const RENDER_TIMEOUT: Duration = Duration::from_secs(60);
// debug 二进制未优化可到 550MB+,光映射自身就会撞 512MB 上限,worker
// 秒死只留下一句 "communication failed"——开发构建放宽到 2GB。
#[cfg(not(debug_assertions))]
const WORKER_ADDRESS_SPACE_LIMIT: u64 = 512 * 1024 * 1024;
#[cfg(debug_assertions)]
const WORKER_ADDRESS_SPACE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const MAX_REQUEST_FRAME_BYTES: usize = 512 * 1024;
const MAX_ERROR_FRAME_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_IMAGES: usize = 1;
const WORKER_ENV: &str = "MIYU_INTERNAL_RENDERER_WORKER";
const WORKER_ARG: &str = "__renderer-worker";
const DEFAULT_BODY_FONT: &str = "Noto Sans CJK SC";
const DEFAULT_CODE_FONT: &str = "Noto Sans Mono CJK SC";
const DEFAULT_EMOJI_FONT: &str = "Noto Color Emoji";
const CJK_FONT_FILE: &str = "NotoSansCJK-Regular.ttc";
const EMOJI_FONT_FILE: &str = "NotoColorEmoji.ttf";
const RENDERER_FONTS_ENV: &str = "MIYU_RENDERER_FONTS_DIR";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RenderConfig {
    pub(crate) theme: String,
    pub(crate) max_height: u32,
    pub(crate) font_size: u32,
    pub(crate) code_font_size: u32,
    pub(crate) padding: u32,
    pub(crate) font: String,
    pub(crate) title_font: String,
    pub(crate) code_font: String,
    pub(crate) emoji_font: String,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            theme: "paper".to_string(),
            max_height: 2600,
            font_size: 36,
            code_font_size: 30,
            padding: 64,
            font: String::new(),
            title_font: String::new(),
            code_font: String::new(),
            emoji_font: String::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct RenderedImage {
    pub(crate) mime: String,
    pub(crate) png: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone)]
pub(crate) struct MarkdownImageRenderer {
    worker: Arc<Mutex<WorkerSlot>>,
}

struct RendererState {
    font_system: FontSystem,
    swash_cache: SwashCache,
    resolved_fonts: HashMap<String, Option<String>>,
    emoji_font_path: PathBuf,
    emoji_loaded: bool,
}

impl MarkdownImageRenderer {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            worker: Arc::new(Mutex::new(WorkerSlot::default())),
        })
    }

    pub(crate) async fn render(
        &self,
        markdown: &str,
        config: &RenderConfig,
    ) -> Result<Vec<RenderedImage>> {
        validate_markdown(markdown)?;
        #[cfg(test)]
        {
            render_in_process_for_test(markdown, config)
        }
        #[cfg(not(test))]
        {
            self.render_with_worker(markdown, config).await
        }
    }

    #[cfg(not(test))]
    async fn render_with_worker(
        &self,
        markdown: &str,
        config: &RenderConfig,
    ) -> Result<Vec<RenderedImage>> {
        let request = RenderRequest {
            markdown: markdown.to_string(),
            config: config.clone(),
        };
        let mut slot = self.worker.lock().await;
        slot.cancel_idle_timer();

        for attempt in 0..2 {
            let mut worker = match slot.process.take() {
                Some(worker) => worker,
                None => WorkerProcess::spawn().await?,
            };
            let result =
                tokio::time::timeout(RENDER_TIMEOUT, exchange_with_worker(&mut worker, &request))
                    .await;
            match result {
                Ok(Ok(images)) => {
                    self.recycle_worker(&mut slot, worker);
                    return Ok(images);
                }
                Ok(Err(WorkerExchangeError::Render(message))) => {
                    self.recycle_worker(&mut slot, worker);
                    return Err(anyhow!(
                        "long-image renderer rejected the request: {message}"
                    ));
                }
                Ok(Err(WorkerExchangeError::Transport(error))) => {
                    stop_worker(worker).await;
                    if attempt == 1 {
                        return Err(error)
                            .context("long-image renderer worker communication failed");
                    }
                }
                Err(_) => {
                    stop_worker(worker).await;
                    bail!(
                        "long-image renderer exceeded its {}-second timeout",
                        RENDER_TIMEOUT.as_secs()
                    );
                }
            }
        }
        unreachable!("renderer worker retry loop always returns")
    }

    #[cfg(not(test))]
    fn recycle_worker(&self, slot: &mut WorkerSlot, worker: WorkerProcess) {
        slot.process = Some(worker);
        slot.generation = slot.generation.wrapping_add(1);
        let generation = slot.generation;
        let weak_slot = Arc::downgrade(&self.worker);
        slot.idle_task = Some(tokio::spawn(async move {
            tokio::time::sleep(WORKER_IDLE_TIMEOUT).await;
            let Some(shared_slot) = weak_slot.upgrade() else {
                return;
            };
            let mut slot = shared_slot.lock().await;
            if slot.generation != generation {
                return;
            }
            if let Some(worker) = slot.process.take() {
                stop_worker(worker).await;
            }
            slot.idle_task.take();
        }));
    }
}

#[cfg(test)]
fn render_in_process_for_test(
    markdown: &str,
    raw_config: &RenderConfig,
) -> Result<Vec<RenderedImage>> {
    static RENDERER: std::sync::OnceLock<std::sync::Mutex<RendererState>> =
        std::sync::OnceLock::new();
    let renderer = RENDERER.get_or_init(|| std::sync::Mutex::new(RendererState::new().unwrap()));
    let mut renderer = renderer.lock().unwrap();
    validate_markdown(markdown)?;
    let config = NormalizedConfig::new(raw_config);
    let blocks = collect_blocks(markdown);
    let palette = Palette::for_theme(&config.theme);
    renderer.render(blocks, &config, palette, markdown_contains_emoji(markdown))
}

struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

#[derive(Default)]
struct WorkerSlot {
    process: Option<WorkerProcess>,
    idle_task: Option<tokio::task::JoinHandle<()>>,
    generation: u64,
}

impl WorkerSlot {
    fn cancel_idle_timer(&mut self) {
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
    async fn spawn() -> Result<Self> {
        let executable = std::env::current_exe().context("locating the Miyu executable")?;
        let mut command = tokio::process::Command::new(executable);
        command
            .arg(WORKER_ARG)
            .env(WORKER_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .context("starting the long-image renderer worker")?;
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
struct RenderRequest {
    markdown: String,
    config: RenderConfig,
}

#[derive(Debug)]
enum WorkerExchangeError {
    Transport(anyhow::Error),
    Render(String),
}

async fn exchange_with_worker(
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

async fn stop_worker(mut worker: WorkerProcess) {
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
fn apply_worker_address_space_limit() -> Result<()> {
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
fn apply_worker_address_space_limit() -> Result<()> {
    Ok(())
}

impl RendererState {
    fn new() -> Result<Self> {
        Self::from_font_dir(&renderer_fonts_dir()?)
    }

    fn from_font_dir(font_dir: &std::path::Path) -> Result<Self> {
        let mut database = FontDatabase::new();
        let cjk_font = font_dir.join(CJK_FONT_FILE);
        database
            .load_font_file(&cjk_font)
            .with_context(|| format!("loading renderer font {}", cjk_font.display()))?;
        if database.faces().next().is_none() {
            bail!("renderer font {} contains no faces", cjk_font.display());
        }
        database.set_sans_serif_family(DEFAULT_BODY_FONT);
        database.set_monospace_family(DEFAULT_CODE_FONT);
        Ok(Self {
            font_system: FontSystem::new_with_locale_and_db("zh-CN".to_string(), database),
            swash_cache: SwashCache::new(),
            resolved_fonts: HashMap::new(),
            emoji_font_path: font_dir.join(EMOJI_FONT_FILE),
            emoji_loaded: false,
        })
    }
}

fn renderer_fonts_dir() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os(RENDERER_FONTS_ENV) {
        candidates.push(PathBuf::from(path));
    }
    #[cfg(debug_assertions)]
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"));
    candidates.push(PathBuf::from("/usr/share/miyu/fonts"));
    if let Ok(executable) = std::env::current_exe() {
        if let Some(prefix) = executable.parent().and_then(std::path::Path::parent) {
            candidates.push(prefix.join("share/miyu/fonts"));
        }
        if let Some(workspace) = executable
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::parent)
        {
            candidates.push(workspace.join("assets/fonts"));
        }
    }
    // 兜底:发行版 noto-fonts-cjk 的标准安装路径。miyu 专用字体目录缺失
    // (比如误装了不带字体的 release 资产包)时,长文转图靠系统字体继续工作。
    candidates.push(PathBuf::from("/usr/share/fonts/noto-cjk"));
    for candidate in &candidates {
        if candidate.join(CJK_FONT_FILE).is_file() {
            return Ok(candidate.clone());
        }
    }
    let searched = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "renderer font is missing; install {CJK_FONT_FILE} in /usr/share/miyu/fonts or set {RENDERER_FONTS_ENV} (searched: {searched})"
    )
}

async fn write_frame<W>(writer: &mut W, payload: &[u8]) -> Result<()>
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

async fn read_frame<R>(reader: &mut R, limit: usize) -> Result<Option<Vec<u8>>>
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

async fn write_worker_response<W>(writer: &mut W, result: Result<Vec<RenderedImage>>) -> Result<()>
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

async fn read_worker_response<R>(
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

async fn write_u32<W>(writer: &mut W, value: usize, label: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let value = u32::try_from(value).with_context(|| format!("{label} does not fit in u32"))?;
    write_u32_value(writer, value).await
}

async fn write_u32_value<W>(writer: &mut W, value: u32) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    tokio::io::AsyncWriteExt::write_all(writer, &value.to_be_bytes()).await?;
    Ok(())
}

async fn read_u32<R>(reader: &mut R) -> Result<u32>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = [0_u8; 4];
    tokio::io::AsyncReadExt::read_exact(reader, &mut bytes).await?;
    Ok(u32::from_be_bytes(bytes))
}

async fn read_byte<R>(reader: &mut R) -> Result<u8>
where
    R: AsyncRead + Unpin,
{
    let mut byte = [0_u8; 1];
    tokio::io::AsyncReadExt::read_exact(reader, &mut byte).await?;
    Ok(byte[0])
}

async fn write_sized_bytes<W>(writer: &mut W, bytes: &[u8], limit: usize, label: &str) -> Result<()>
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

async fn read_sized_bytes<R>(reader: &mut R, limit: usize) -> Result<Vec<u8>>
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

impl RendererState {
    fn render(
        &mut self,
        blocks: Vec<Block>,
        config: &NormalizedConfig,
        palette: Palette,
        needs_emoji: bool,
    ) -> Result<Vec<RenderedImage>> {
        if self.swash_cache.image_cache.len() > MAX_CACHED_GLYPHS {
            self.swash_cache.image_cache.clear();
        }
        let fonts = self.resolve_config_fonts(config, needs_emoji)?;
        let layouts = layout_blocks(&mut self.font_system, blocks, config, palette, &fonts)?;
        let columns = plan_balanced_columns(&layouts, config)?;
        let rendered = render_pages(
            &mut self.font_system,
            &mut self.swash_cache,
            &layouts,
            &columns,
            config,
            palette,
        );
        if self.swash_cache.image_cache.len() > MAX_CACHED_GLYPHS {
            self.swash_cache.image_cache.clear();
        }
        rendered
    }

    fn resolve_config_fonts(
        &mut self,
        config: &NormalizedConfig,
        needs_emoji: bool,
    ) -> Result<ResolvedFonts> {
        let body = self
            .resolve_font(&config.font)
            .or_else(|| Some(DEFAULT_BODY_FONT.to_string()));
        let title = if config.title_font.trim().is_empty() {
            body.clone()
        } else {
            self.resolve_font(&config.title_font)
        };
        let emoji = if needs_emoji {
            let configured = config.emoji_font.trim();
            if configured.is_empty() || configured.eq_ignore_ascii_case(DEFAULT_EMOJI_FONT) {
                self.ensure_bundled_emoji_font()?;
                Some(DEFAULT_EMOJI_FONT.to_string())
            } else if let Some(font) = self.resolve_font(configured) {
                Some(font)
            } else {
                self.ensure_bundled_emoji_font()?;
                Some(DEFAULT_EMOJI_FONT.to_string())
            }
        } else {
            None
        };
        Ok(ResolvedFonts {
            body,
            title,
            code: self
                .resolve_font(&config.code_font)
                .or_else(|| Some(DEFAULT_CODE_FONT.to_string())),
            emoji,
        })
    }

    fn ensure_bundled_emoji_font(&mut self) -> Result<()> {
        if self.emoji_loaded {
            return Ok(());
        }

        let previous_faces = self.font_system.db().faces().count();
        self.font_system
            .db_mut()
            .load_font_file(&self.emoji_font_path)
            .with_context(|| {
                format!(
                    "loading renderer Emoji font {}",
                    self.emoji_font_path.display()
                )
            })?;
        let has_emoji_family = self
            .font_system
            .db()
            .faces()
            .skip(previous_faces)
            .any(|face| {
                face.families
                    .iter()
                    .any(|(family, _)| family.eq_ignore_ascii_case(DEFAULT_EMOJI_FONT))
            });
        if !has_emoji_family {
            bail!(
                "renderer Emoji font {} does not contain the {DEFAULT_EMOJI_FONT} family",
                self.emoji_font_path.display()
            );
        }
        self.emoji_loaded = true;
        Ok(())
    }

    fn resolve_font(&mut self, configured: &str) -> Option<String> {
        let configured = configured.trim();
        if configured.is_empty() {
            return None;
        }
        let path = PathBuf::from(configured);
        if !path.is_file() {
            let bundled_family = self.font_system.db().faces().any(|face| {
                face.families
                    .iter()
                    .any(|(family, _)| family.eq_ignore_ascii_case(configured))
            });
            if !bundled_family {
                tracing::warn!(
                    font = configured,
                    "{}",
                    crate::i18n::text(
                        "long-image renderer font is not a bundled family or readable file; using the default font",
                        "长图渲染器字体不是内置字体族或可读文件；使用默认字体"
                    )
                );
                return None;
            }
            return Some(configured.to_string());
        }
        let path = path.canonicalize().unwrap_or(path);
        let cache_key = path.to_string_lossy().into_owned();
        if let Some(cached) = self.resolved_fonts.get(&cache_key) {
            return cached.clone();
        }
        if self.resolved_fonts.len() >= MAX_CUSTOM_FONT_FILES {
            tracing::warn!(
                font = %path.display(),
                limit = MAX_CUSTOM_FONT_FILES,
                "{}",
                crate::i18n::text(
                    "long-image renderer custom font limit reached; using the default font",
                    "长图渲染器已达到自定义字体上限；使用默认字体"
                )
            );
            return None;
        }

        let previous_faces = self.font_system.db().faces().count();
        let resolved = self
            .font_system
            .db_mut()
            .load_font_file(&path)
            .ok()
            .and_then(|()| {
                self.font_system
                    .db()
                    .faces()
                    .skip(previous_faces)
                    .find_map(|face| face.families.first().map(|(name, _)| name.clone()))
            });
        self.resolved_fonts.insert(cache_key, resolved.clone());
        resolved
    }
}

#[derive(Clone)]
struct NormalizedConfig {
    theme: String,
    max_height: u32,
    font_size: u32,
    code_font_size: u32,
    padding: u32,
    font: String,
    title_font: String,
    code_font: String,
    emoji_font: String,
}

impl NormalizedConfig {
    fn new(config: &RenderConfig) -> Self {
        Self {
            theme: config.theme.trim().to_ascii_lowercase(),
            max_height: config
                .max_height
                .clamp(MIN_CONFIGURED_HEIGHT, MAX_PAGE_HEIGHT),
            font_size: config.font_size.clamp(14, 56),
            code_font_size: config.code_font_size.clamp(12, 52),
            padding: config.padding.clamp(24, 160),
            font: config.font.clone(),
            title_font: config.title_font.clone(),
            code_font: config.code_font.clone(),
            emoji_font: config.emoji_font.clone(),
        }
    }
}

#[derive(Clone)]
struct ResolvedFonts {
    body: Option<String>,
    title: Option<String>,
    code: Option<String>,
    emoji: Option<String>,
}

#[derive(Clone, Copy)]
struct Palette {
    background: [u8; 4],
    text: [u8; 4],
    heading: [u8; 4],
    muted: [u8; 4],
    link: [u8; 4],
    code_background: [u8; 4],
    code_text: [u8; 4],
    quote_background: [u8; 4],
    quote_bar: [u8; 4],
    table_header_background: [u8; 4],
    table_background: [u8; 4],
    border: [u8; 4],
    rule: [u8; 4],
}

impl Palette {
    fn for_theme(theme: &str) -> Self {
        match theme {
            "dark" => Self {
                background: [28, 29, 32, 255],
                text: [231, 232, 235, 255],
                heading: [255, 255, 255, 255],
                muted: [164, 168, 176, 255],
                link: [104, 179, 255, 255],
                code_background: [43, 45, 51, 255],
                code_text: [239, 240, 244, 255],
                quote_background: [37, 40, 45, 255],
                quote_bar: [93, 168, 143, 255],
                table_header_background: [19, 20, 23, 255],
                table_background: [34, 36, 40, 255],
                border: [72, 76, 84, 255],
                rule: [83, 87, 95, 255],
            },
            "light" => Self {
                background: [250, 250, 248, 255],
                text: [30, 34, 40, 255],
                heading: [18, 20, 24, 255],
                muted: [92, 96, 104, 255],
                link: [48, 101, 190, 255],
                code_background: [226, 229, 235, 255],
                code_text: [34, 38, 45, 255],
                quote_background: [244, 247, 255, 255],
                quote_bar: [74, 116, 214, 255],
                table_header_background: [238, 240, 244, 255],
                table_background: [246, 247, 249, 255],
                border: [218, 222, 230, 255],
                rule: [218, 222, 230, 255],
            },
            _ => Self {
                background: [244, 239, 229, 255],
                text: [48, 46, 41, 255],
                heading: [37, 34, 29, 255],
                muted: [104, 98, 88, 255],
                link: [112, 82, 43, 255],
                code_background: [225, 219, 208, 255],
                code_text: [42, 39, 34, 255],
                quote_background: [236, 229, 214, 255],
                quote_bar: [134, 101, 54, 255],
                table_header_background: [232, 226, 215, 255],
                table_background: [239, 233, 222, 255],
                border: [211, 201, 184, 255],
                rule: [211, 201, 184, 255],
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Paragraph,
    Heading(u8),
    ListItem { depth: u8 },
    Quote,
    Code,
    Table,
    Rule,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    code: bool,
    link: bool,
    muted: bool,
}

#[derive(Clone, Debug)]
struct RichSpan {
    text: String,
    style: InlineStyle,
}

#[derive(Clone, Debug)]
struct Block {
    kind: BlockKind,
    spans: Vec<RichSpan>,
    table: Option<TableBlock>,
    task: Option<bool>,
}

impl Block {
    fn new(kind: BlockKind) -> Self {
        Self {
            kind,
            spans: Vec::new(),
            table: None,
            task: None,
        }
    }

    fn push(&mut self, text: &str, style: InlineStyle) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.spans.last_mut().filter(|last| last.style == style) {
            last.text.push_str(text);
        } else {
            self.spans.push(RichSpan {
                text: text.to_string(),
                style,
            });
        }
    }

    fn has_content(&self) -> bool {
        self.kind == BlockKind::Rule
            || self.spans.iter().any(|span| !span.text.is_empty())
            || self.table.as_ref().is_some_and(TableBlock::has_content)
            || self.task.is_some()
    }
}

#[derive(Clone, Debug)]
struct TableBlock {
    alignments: Vec<Alignment>,
    header: Vec<Vec<RichSpan>>,
    rows: Vec<Vec<Vec<RichSpan>>>,
}

impl TableBlock {
    fn has_content(&self) -> bool {
        !self.header.is_empty() || !self.rows.is_empty()
    }
}

#[derive(Default)]
struct TableBuilder {
    alignments: Vec<Alignment>,
    header: Vec<Vec<RichSpan>>,
    rows: Vec<Vec<Vec<RichSpan>>>,
    current_row: Vec<Vec<RichSpan>>,
    current_cell: Vec<RichSpan>,
    in_cell: bool,
}

impl TableBuilder {
    fn push(&mut self, text: &str, style: InlineStyle) {
        if text.is_empty() || !self.in_cell {
            return;
        }
        if let Some(last) = self
            .current_cell
            .last_mut()
            .filter(|last| last.style == style)
        {
            last.text.push_str(text);
        } else {
            self.current_cell.push(RichSpan {
                text: text.to_string(),
                style,
            });
        }
    }

    fn start_row(&mut self) {
        self.current_row.clear();
        self.current_cell.clear();
        self.in_cell = false;
    }

    fn start_cell(&mut self) {
        self.current_cell.clear();
        self.in_cell = true;
    }

    fn finish_cell(&mut self) {
        if self.in_cell {
            self.current_row
                .push(std::mem::take(&mut self.current_cell));
            self.in_cell = false;
        }
    }

    fn finish_row(&mut self, header: bool) {
        self.finish_cell();
        let row = std::mem::take(&mut self.current_row);
        if row.is_empty() {
            return;
        }
        if header {
            self.header = row;
        } else {
            self.rows.push(row);
        }
    }

    fn finish(mut self) -> TableBlock {
        self.finish_cell();
        TableBlock {
            alignments: self.alignments,
            header: self.header,
            rows: self.rows,
        }
    }
}

struct ListState {
    ordered: bool,
    next: u64,
    in_item: bool,
    prefix_used: bool,
}

#[derive(Default)]
struct MarkdownCollector {
    blocks: Vec<Block>,
    current: Option<Block>,
    lists: Vec<ListState>,
    quote_depth: usize,
    heading: Option<u8>,
    code_block: bool,
    table: Option<TableBuilder>,
    table_header: bool,
    strong_depth: usize,
    emphasis_depth: usize,
    link_depth: usize,
    strike_depth: usize,
}

impl MarkdownCollector {
    fn collect(mut self, markdown: &str) -> Vec<Block> {
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        for event in Parser::new_ext(markdown, options) {
            self.event(event);
        }
        self.finish_current();
        self.blocks
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text, self.style()),
            Event::Code(text) => {
                let mut style = self.style();
                style.code = true;
                self.push_text(&text, style);
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                let mut style = self.style();
                style.code = true;
                self.push_text(&text, style);
            }
            Event::SoftBreak => {
                let separator = if self.code_block || self.table.is_some() {
                    "\n"
                } else {
                    " "
                };
                self.push_text(separator, self.style());
            }
            Event::HardBreak => self.push_text("\n", self.style()),
            Event::Rule => {
                self.finish_current();
                self.blocks.push(Block::new(BlockKind::Rule));
            }
            Event::TaskListMarker(done) => {
                self.mark_task(done);
            }
            Event::FootnoteReference(label) => {
                self.push_text(&format!("[{label}]"), self.style());
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                self.push_text(&text, self.style());
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.ensure_current(),
            Tag::Heading { level, .. } => {
                self.finish_current();
                let level = level as u8;
                self.heading = Some(level);
                self.current = Some(Block::new(BlockKind::Heading(level)));
            }
            Tag::BlockQuote(_) => {
                self.finish_current();
                self.quote_depth = self.quote_depth.saturating_add(1);
            }
            Tag::CodeBlock(_) => {
                self.finish_current();
                self.code_block = true;
                self.current = Some(Block::new(BlockKind::Code));
            }
            Tag::List(start) => {
                self.finish_current();
                self.lists.push(ListState {
                    ordered: start.is_some(),
                    next: start.unwrap_or(1),
                    in_item: false,
                    prefix_used: false,
                });
            }
            Tag::Item => {
                self.finish_current();
                if let Some(list) = self.lists.last_mut() {
                    list.in_item = true;
                    list.prefix_used = false;
                }
                self.ensure_current();
            }
            Tag::Table(alignments) => {
                self.finish_current();
                self.table = Some(TableBuilder {
                    alignments,
                    ..TableBuilder::default()
                });
                self.table_header = false;
            }
            Tag::TableHead => {
                self.table_header = true;
                if let Some(table) = self.table.as_mut() {
                    table.start_row();
                }
            }
            Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.start_row();
                }
            }
            Tag::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.start_cell();
                }
            }
            Tag::Strong => self.strong_depth = self.strong_depth.saturating_add(1),
            Tag::Emphasis => self.emphasis_depth = self.emphasis_depth.saturating_add(1),
            Tag::Strikethrough => self.strike_depth = self.strike_depth.saturating_add(1),
            Tag::Link { .. } | Tag::Image { .. } => {
                self.link_depth = self.link_depth.saturating_add(1)
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                if !self.code_block && self.table.is_none() && self.heading.is_none() {
                    self.finish_current();
                }
            }
            TagEnd::Heading(_) => {
                self.finish_current();
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => {
                self.finish_current();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                self.finish_current();
                self.code_block = false;
            }
            TagEnd::List(_) => {
                self.finish_current();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.finish_current();
                if let Some(list) = self.lists.last_mut() {
                    list.in_item = false;
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    let mut block = Block::new(BlockKind::Table);
                    block.table = Some(table.finish());
                    if block.has_content() {
                        self.blocks.push(block);
                    }
                }
                self.table_header = false;
            }
            TagEnd::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_row(true);
                }
                self.table_header = false;
            }
            TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_row(self.table_header);
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_cell();
                }
            }
            TagEnd::Strong => self.strong_depth = self.strong_depth.saturating_sub(1),
            TagEnd::Emphasis => self.emphasis_depth = self.emphasis_depth.saturating_sub(1),
            TagEnd::Strikethrough => self.strike_depth = self.strike_depth.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => self.link_depth = self.link_depth.saturating_sub(1),
            _ => {}
        }
    }

    fn ensure_current(&mut self) {
        if self.current.is_some() {
            return;
        }
        if self.code_block {
            self.current = Some(Block::new(BlockKind::Code));
            return;
        }
        if self.table.is_some() {
            return;
        }
        if let Some(level) = self.heading {
            self.current = Some(Block::new(BlockKind::Heading(level)));
            return;
        }

        if let Some(index) = self.lists.iter().rposition(|list| list.in_item) {
            let depth = u8::try_from(index + 1).unwrap_or(u8::MAX);
            let list = &mut self.lists[index];
            let prefix = if list.prefix_used {
                "    ".to_string()
            } else if list.ordered {
                let number = list.next;
                list.next = list.next.saturating_add(1);
                list.prefix_used = true;
                format!("{number}. ")
            } else {
                list.prefix_used = true;
                "• ".to_string()
            };
            let mut block = Block::new(BlockKind::ListItem { depth });
            block.push(&prefix, InlineStyle::default());
            self.current = Some(block);
        } else if self.quote_depth > 0 {
            self.current = Some(Block::new(BlockKind::Quote));
        } else {
            self.current = Some(Block::new(BlockKind::Paragraph));
        }
    }

    fn push_text(&mut self, text: &str, style: InlineStyle) {
        if let Some(table) = self.table.as_mut() {
            table.push(text, style);
            return;
        }
        self.ensure_current();
        if let Some(block) = self.current.as_mut() {
            block.push(text, style);
        }
    }

    fn mark_task(&mut self, done: bool) {
        self.ensure_current();
        let Some(block) = self.current.as_mut() else {
            return;
        };
        if let Some(first) = block.spans.first_mut() {
            if let Some(rest) = first.text.strip_prefix("• ") {
                first.text = rest.to_string();
                if first.text.is_empty() {
                    block.spans.remove(0);
                }
            }
        }
        block.task = Some(done);
    }

    fn style(&self) -> InlineStyle {
        InlineStyle {
            bold: self.strong_depth > 0 || self.table_header,
            italic: self.emphasis_depth > 0,
            code: self.code_block,
            link: self.link_depth > 0,
            muted: self.strike_depth > 0,
        }
    }

    fn finish_current(&mut self) {
        let Some(block) = self.current.take() else {
            return;
        };
        if block.has_content() {
            self.blocks.push(block);
        }
    }
}

fn collect_blocks(markdown: &str) -> Vec<Block> {
    MarkdownCollector::default().collect(markdown)
}

fn validate_markdown(markdown: &str) -> Result<()> {
    let count = markdown.chars().take(MAX_INPUT_CHARS + 1).count();
    if count > MAX_INPUT_CHARS {
        bail!("Markdown image input exceeds the {MAX_INPUT_CHARS}-character limit");
    }
    Ok(())
}

struct LayoutBlock {
    kind: BlockKind,
    buffer: Option<Buffer>,
    table: Option<LayoutTable>,
    task: Option<TaskBox>,
    total_height: u32,
    vertical_padding: u32,
    inset_left: u32,
    boundaries: Vec<u32>,
    margin_before: u32,
    margin_after: u32,
    default_color: Color,
    inline_code_background: [u8; 4],
}

struct LayoutTable {
    rows: Vec<LayoutTableRow>,
    header_height: u32,
}

struct LayoutTableRow {
    cells: Vec<LayoutTableCell>,
    source_start: u32,
    source_end: u32,
    header: bool,
    stripe: bool,
}

struct LayoutTableCell {
    buffer: Buffer,
    x: u32,
    width: u32,
    default_color: Color,
    inline_code_background: [u8; 4],
}

#[derive(Clone, Copy)]
struct TaskBox {
    checked: bool,
    x: u32,
    y: u32,
    size: u32,
}

fn layout_blocks(
    font_system: &mut FontSystem,
    blocks: Vec<Block>,
    config: &NormalizedConfig,
    palette: Palette,
    fonts: &ResolvedFonts,
) -> Result<Vec<LayoutBlock>> {
    blocks
        .into_iter()
        .map(|block| layout_block(font_system, block, config, palette, fonts))
        .collect()
}

fn layout_block(
    font_system: &mut FontSystem,
    block: Block,
    config: &NormalizedConfig,
    palette: Palette,
    fonts: &ResolvedFonts,
) -> Result<LayoutBlock> {
    if block.kind == BlockKind::Rule {
        return Ok(LayoutBlock {
            kind: block.kind,
            buffer: None,
            table: None,
            task: None,
            total_height: 28,
            vertical_padding: 0,
            inset_left: 0,
            boundaries: vec![28],
            margin_before: 20,
            margin_after: 20,
            default_color: color(palette.text),
            inline_code_background: palette.code_background,
        });
    }

    if block.kind == BlockKind::Table {
        return layout_table(
            font_system,
            block
                .table
                .ok_or_else(|| anyhow!("Markdown table is missing its structured rows"))?,
            config,
            palette,
            fonts,
        );
    }

    let (mut inset_left, inset_right, vertical_padding) = block_insets(block.kind);
    let task = block.task.map(|checked| {
        let size = (config.font_size * 3 / 5).clamp(18, 30);
        let marker_x = inset_left.saturating_add(4);
        let marker_y = vertical_padding.saturating_add(
            ((metrics_for(block.kind, InlineStyle::default(), config).line_height as u32)
                .saturating_sub(size))
                / 2,
        );
        inset_left = inset_left.saturating_add(size).saturating_add(16);
        TaskBox {
            checked,
            x: marker_x,
            y: marker_y,
            size,
        }
    });
    let content_width = COLUMN_WIDTH
        .saturating_sub(inset_left)
        .saturating_sub(inset_right)
        .max(64);
    let metrics = metrics_for(block.kind, InlineStyle::default(), config);
    let default_attrs = attrs_for(
        block.kind,
        InlineStyle::default(),
        false,
        metrics,
        palette,
        fonts,
    );
    let expanded = expand_spans(&block.spans, fonts.emoji.is_some());
    let rich_spans = expanded
        .iter()
        .map(|span| {
            let metrics = metrics_for(block.kind, span.style, config);
            let attrs = attrs_for(block.kind, span.style, span.emoji, metrics, palette, fonts);
            (span.text.clone(), attrs)
        })
        .collect::<Vec<_>>();

    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(Some(content_width as f32), None);
    buffer.set_wrap(Wrap::WordOrGlyph);
    buffer.set_rich_text(
        rich_spans
            .iter()
            .map(|(text, attrs)| (text.as_str(), attrs.clone())),
        &default_attrs,
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(font_system, true);

    let mut boundaries = Vec::new();
    let mut text_height = 1_u32;
    for run in buffer.layout_runs() {
        let bottom = (run.line_top + run.line_height).ceil().max(1.0) as u32;
        text_height = text_height.max(bottom);
        let boundary = vertical_padding.saturating_add(bottom);
        if boundaries.last().copied() != Some(boundary) {
            boundaries.push(boundary);
        }
    }
    let total_height = text_height.saturating_add(vertical_padding.saturating_mul(2));
    if let Some(last) = boundaries.last_mut() {
        *last = total_height;
    } else {
        boundaries.push(total_height);
    }
    let (margin_before, margin_after) = block_margins(block.kind, config.font_size);
    let default_color = if block.kind == BlockKind::Code {
        palette.code_text
    } else {
        palette.text
    };
    Ok(LayoutBlock {
        kind: block.kind,
        buffer: Some(buffer),
        table: None,
        task,
        total_height,
        vertical_padding,
        inset_left,
        boundaries,
        margin_before,
        margin_after,
        default_color: color(default_color),
        inline_code_background: palette.code_background,
    })
}

fn layout_table(
    font_system: &mut FontSystem,
    table: TableBlock,
    config: &NormalizedConfig,
    palette: Palette,
    fonts: &ResolvedFonts,
) -> Result<LayoutBlock> {
    let column_count = table
        .alignments
        .len()
        .max(table.header.len())
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
    if column_count == 0 {
        bail!("Markdown table has no columns");
    }
    let column_count_u32 =
        u32::try_from(column_count).context("too many Markdown table columns")?;
    let base_width = COLUMN_WIDTH / column_count_u32;
    let remainder = COLUMN_WIDTH % column_count_u32;
    if base_width <= TABLE_CELL_PADDING.saturating_mul(2) {
        bail!("Markdown table has too many columns to render safely");
    }

    let mut widths = Vec::with_capacity(column_count);
    for index in 0..column_count_u32 {
        widths.push(base_width + u32::from(index < remainder));
    }

    let mut rows = Vec::with_capacity(table.rows.len().saturating_add(1));
    let mut source_y = 0_u32;
    if !table.header.is_empty() {
        let row = layout_table_row(
            font_system,
            &table.header,
            &table.alignments,
            &widths,
            true,
            false,
            source_y,
            config,
            palette,
            fonts,
        )?;
        source_y = row.source_end;
        rows.push(row);
    }
    let header_height = source_y;
    for (index, cells) in table.rows.iter().enumerate() {
        let row = layout_table_row(
            font_system,
            cells,
            &table.alignments,
            &widths,
            false,
            index % 2 == 1,
            source_y,
            config,
            palette,
            fonts,
        )?;
        source_y = row.source_end;
        rows.push(row);
    }
    let boundaries = rows.iter().map(|row| row.source_end).collect::<Vec<_>>();
    let (margin_before, margin_after) = block_margins(BlockKind::Table, config.font_size);
    Ok(LayoutBlock {
        kind: BlockKind::Table,
        buffer: None,
        table: Some(LayoutTable {
            rows,
            header_height,
        }),
        task: None,
        total_height: source_y,
        vertical_padding: 0,
        inset_left: 0,
        boundaries,
        margin_before,
        margin_after,
        default_color: color(palette.text),
        inline_code_background: palette.code_background,
    })
}

#[allow(clippy::too_many_arguments)]
fn layout_table_row(
    font_system: &mut FontSystem,
    cells: &[Vec<RichSpan>],
    alignments: &[Alignment],
    widths: &[u32],
    header: bool,
    stripe: bool,
    source_start: u32,
    config: &NormalizedConfig,
    palette: Palette,
    fonts: &ResolvedFonts,
) -> Result<LayoutTableRow> {
    let metrics = metrics_for(BlockKind::Table, InlineStyle::default(), config);
    let mut x = 0_u32;
    let mut row_height = metrics.line_height.ceil().max(1.0) as u32;
    let mut laid_out = Vec::with_capacity(widths.len());
    for (index, width) in widths.iter().copied().enumerate() {
        let content_width = width.saturating_sub(TABLE_CELL_PADDING.saturating_mul(2));
        let spans = cells.get(index).map(Vec::as_slice).unwrap_or(&[]);
        let alignment = alignments.get(index).copied().unwrap_or(Alignment::None);
        let (buffer, text_height, default_color) = layout_rich_buffer(
            font_system,
            spans,
            BlockKind::Table,
            content_width,
            header,
            alignment,
            config,
            palette,
            fonts,
        );
        row_height = row_height.max(text_height);
        laid_out.push(LayoutTableCell {
            buffer,
            x,
            width,
            default_color,
            inline_code_background: palette.code_background,
        });
        x = x
            .checked_add(width)
            .context("Markdown table width overflowed")?;
    }
    row_height = row_height.saturating_add(TABLE_CELL_PADDING.saturating_mul(2));
    let source_end = source_start
        .checked_add(row_height)
        .context("Markdown table height overflowed")?;
    Ok(LayoutTableRow {
        cells: laid_out,
        source_start,
        source_end,
        header,
        stripe,
    })
}

#[allow(clippy::too_many_arguments)]
fn layout_rich_buffer(
    font_system: &mut FontSystem,
    spans: &[RichSpan],
    kind: BlockKind,
    width: u32,
    force_bold: bool,
    alignment: Alignment,
    config: &NormalizedConfig,
    palette: Palette,
    fonts: &ResolvedFonts,
) -> (Buffer, u32, Color) {
    let metrics = metrics_for(kind, InlineStyle::default(), config);
    let default_attrs = attrs_for(
        kind,
        InlineStyle {
            bold: force_bold,
            ..InlineStyle::default()
        },
        false,
        metrics,
        palette,
        fonts,
    );
    let mut expanded = expand_spans(spans, fonts.emoji.is_some());
    if expanded.is_empty() {
        expanded.push(ExpandedSpan {
            text: " ".to_string(),
            style: InlineStyle::default(),
            emoji: false,
        });
    }
    let rich_spans = expanded
        .iter()
        .map(|span| {
            let mut style = span.style;
            style.bold |= force_bold;
            let metrics = metrics_for(kind, style, config);
            let attrs = attrs_for(kind, style, span.emoji, metrics, palette, fonts);
            (span.text.clone(), attrs)
        })
        .collect::<Vec<_>>();
    let alignment = match alignment {
        Alignment::Right => Some(TextAlign::Right),
        Alignment::Center => Some(TextAlign::Center),
        Alignment::Left => Some(TextAlign::Left),
        Alignment::None => None,
    };
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(Some(width.max(1) as f32), None);
    buffer.set_wrap(Wrap::WordOrGlyph);
    buffer.set_rich_text(
        rich_spans
            .iter()
            .map(|(text, attrs)| (text.as_str(), attrs.clone())),
        &default_attrs,
        Shaping::Advanced,
        alignment,
    );
    buffer.shape_until_scroll(font_system, true);
    let text_height = buffer
        .layout_runs()
        .map(|run| (run.line_top + run.line_height).ceil().max(1.0) as u32)
        .max()
        .unwrap_or_else(|| metrics.line_height.ceil().max(1.0) as u32);
    (buffer, text_height, color(palette.text))
}

#[derive(Clone)]
struct ExpandedSpan {
    text: String,
    style: InlineStyle,
    emoji: bool,
}

fn expand_spans(spans: &[RichSpan], split_emoji: bool) -> Vec<ExpandedSpan> {
    let mut expanded: Vec<ExpandedSpan> = Vec::new();
    for span in spans {
        if !split_emoji {
            expanded.push(ExpandedSpan {
                text: span.text.clone(),
                style: span.style,
                emoji: false,
            });
            continue;
        }
        for grapheme in span.text.graphemes(true) {
            let emoji = grapheme_is_emoji(grapheme);
            if let Some(last) = expanded
                .last_mut()
                .filter(|last| last.style == span.style && last.emoji == emoji)
            {
                last.text.push_str(grapheme);
            } else {
                expanded.push(ExpandedSpan {
                    text: grapheme.to_string(),
                    style: span.style,
                    emoji,
                });
            }
        }
    }
    expanded
}

fn markdown_contains_emoji(markdown: &str) -> bool {
    markdown.graphemes(true).any(grapheme_is_emoji)
}

fn grapheme_is_emoji(grapheme: &str) -> bool {
    grapheme.chars().any(|ch| {
        matches!(
            ch as u32,
            0x1F000..=0x1FAFF
                | 0x2300..=0x23FF
                | 0x2600..=0x27BF
                | 0x2B00..=0x2BFF
                | 0xFE0F
                | 0x200D
        )
    })
}

fn attrs_for<'a>(
    kind: BlockKind,
    style: InlineStyle,
    emoji: bool,
    metrics: Metrics,
    palette: Palette,
    fonts: &'a ResolvedFonts,
) -> Attrs<'a> {
    let named = if emoji {
        fonts.emoji.as_deref()
    } else if style.code || matches!(kind, BlockKind::Code) {
        fonts.code.as_deref()
    } else if matches!(kind, BlockKind::Heading(_)) {
        fonts.title.as_deref().or(fonts.body.as_deref())
    } else {
        fonts.body.as_deref()
    };
    let fallback = if style.code || matches!(kind, BlockKind::Code) {
        Family::Monospace
    } else {
        Family::SansSerif
    };
    let family = named.map(Family::Name).unwrap_or(fallback);
    let foreground = if matches!(kind, BlockKind::Code) {
        palette.code_text
    } else if style.code {
        palette.code_text
    } else if style.link {
        palette.link
    } else if style.muted {
        palette.muted
    } else if matches!(kind, BlockKind::Heading(_)) {
        palette.heading
    } else {
        palette.text
    };
    let mut attrs = Attrs::new()
        .family(family)
        .color(color(foreground))
        .metrics(metrics);
    if style.bold || matches!(kind, BlockKind::Heading(_)) {
        attrs = attrs.weight(Weight::BOLD);
    }
    if style.italic {
        attrs = attrs.style(FontStyle::Italic);
    }
    if style.code && !matches!(kind, BlockKind::Code) {
        // 行内代码经 metadata 传到 LayoutGlyph,绘制时据此画底色小块;
        // 代码块整块已有背景,不标。
        attrs = attrs.metadata(INLINE_CODE_METADATA);
    }
    attrs
}

const INLINE_CODE_METADATA: usize = 1;

/// 一条 layout 行内行内代码字形的连续 x 区间(相邻区间合并)。
fn inline_code_chip_ranges(glyphs: &[LayoutGlyph]) -> Vec<(f32, f32)> {
    let mut ranges: Vec<(f32, f32)> = Vec::new();
    for glyph in glyphs {
        if glyph.metadata != INLINE_CODE_METADATA {
            continue;
        }
        let start = glyph.x;
        let end = glyph.x + glyph.w;
        match ranges.last_mut() {
            Some((_, last_end)) if start - *last_end <= 0.5 => *last_end = end.max(*last_end),
            _ => ranges.push((start, end)),
        }
    }
    ranges
}

/// 行内代码底色块的水平/垂直留白。
const INLINE_CODE_CHIP_PAD_X: f32 = 5.0;
const INLINE_CODE_CHIP_INSET_RATIO: f32 = 0.10;

fn metrics_for(kind: BlockKind, style: InlineStyle, config: &NormalizedConfig) -> Metrics {
    let body = config.font_size as f32;
    let code = config.code_font_size as f32;
    let size = match kind {
        BlockKind::Heading(level) => {
            let scale = match level {
                1 => 1.55,
                2 => 1.35,
                3 => 1.20,
                4 => 1.10,
                _ => 1.0,
            };
            (body * scale).min(76.0)
        }
        BlockKind::Code => code,
        BlockKind::Table => (body * 0.92).max(14.0),
        _ if style.code => code,
        _ => body,
    };
    Metrics::new(size, (size * 1.42).ceil())
}

fn block_insets(kind: BlockKind) -> (u32, u32, u32) {
    match kind {
        BlockKind::Code => (32, 32, 24),
        BlockKind::Table => (20, 20, 16),
        BlockKind::Quote => (32, 14, 12),
        BlockKind::ListItem { depth } => {
            (u32::from(depth.saturating_sub(1)).saturating_mul(18), 0, 0)
        }
        _ => (0, 0, 0),
    }
}

fn block_margins(kind: BlockKind, font_size: u32) -> (u32, u32) {
    let small = (font_size / 4).max(6);
    match kind {
        BlockKind::Heading(1) => (font_size, font_size / 2),
        BlockKind::Heading(_) => (font_size / 2, small),
        BlockKind::Code | BlockKind::Table => (font_size / 2, font_size / 2),
        BlockKind::Rule => (font_size / 2, font_size / 2),
        BlockKind::Quote => (small, small),
        BlockKind::ListItem { .. } => (small / 2, small / 2),
        BlockKind::Paragraph => (small, small),
    }
}

#[derive(Default)]
struct ColumnPlan {
    placements: Vec<Placement>,
    used_height: u32,
}

struct Placement {
    block_index: usize,
    source_start: u32,
    source_end: u32,
    y: u32,
}

fn plan_columns(layouts: &[LayoutBlock], config: &NormalizedConfig) -> Result<Vec<ColumnPlan>> {
    let usable_height = config
        .max_height
        .saturating_sub(config.padding.saturating_mul(2));
    plan_columns_with_height(layouts, usable_height)
}

fn plan_columns_with_height(
    layouts: &[LayoutBlock],
    usable_height: u32,
) -> Result<Vec<ColumnPlan>> {
    if usable_height < 128 {
        bail!("page height leaves too little room for rendered content");
    }
    let mut columns = vec![ColumnPlan::default()];

    for (block_index, block) in layouts.iter().enumerate() {
        if let Some(table) = block.table.as_ref() {
            if table.header_height > usable_height {
                bail!("a Markdown table header exceeds the usable image height");
            }
            for row in table.rows.iter().filter(|row| !row.header) {
                let row_height = row.source_end.saturating_sub(row.source_start);
                if table.header_height.saturating_add(row_height) > usable_height {
                    bail!("a Markdown table row exceeds the usable image height");
                }
            }
        }
        let mut source_start = 0;
        let mut first_fragment = true;
        while source_start < block.total_height {
            if source_start > 0 {
                if let Some(table) = block
                    .table
                    .as_ref()
                    .filter(|table| table.header_height > 0 && source_start >= table.header_height)
                {
                    let column = columns
                        .last_mut()
                        .ok_or_else(|| anyhow!("renderer column planner lost its active column"))?;
                    if column.used_height == 0 {
                        column.placements.push(Placement {
                            block_index,
                            source_start: 0,
                            source_end: table.header_height,
                            y: 0,
                        });
                        column.used_height = table.header_height;
                    }
                }
            }
            let column = columns
                .last_mut()
                .ok_or_else(|| anyhow!("renderer column planner lost its active column"))?;
            let margin = if first_fragment && column.used_height > 0 {
                block.margin_before
            } else {
                0
            };
            let remaining = block.total_height.saturating_sub(source_start);
            let available = usable_height
                .saturating_sub(column.used_height)
                .saturating_sub(margin);

            if first_fragment && column.used_height > 0 {
                if let Some(table) = block.table.as_ref() {
                    let first_body_height = table
                        .rows
                        .iter()
                        .find(|row| !row.header)
                        .map(|row| row.source_end.saturating_sub(row.source_start))
                        .unwrap_or(0);
                    let first_table_chunk = table.header_height.saturating_add(first_body_height);
                    if first_table_chunk > available && first_table_chunk <= usable_height {
                        push_column(&mut columns)?;
                        continue;
                    }
                }
            }

            if first_fragment
                && block.kind != BlockKind::Code
                && block.total_height <= usable_height
                && remaining > available
                && column.used_height > 0
            {
                push_column(&mut columns)?;
                continue;
            }
            if available == 0 {
                push_column(&mut columns)?;
                continue;
            }

            let limit = source_start.saturating_add(available);
            let source_end = if remaining <= available {
                block.total_height
            } else {
                block
                    .boundaries
                    .iter()
                    .copied()
                    .take_while(|boundary| *boundary <= limit)
                    .last()
                    .unwrap_or(source_start)
            };
            if source_end <= source_start {
                if column.used_height == 0 {
                    bail!("a rendered text line exceeds the usable page height");
                }
                push_column(&mut columns)?;
                continue;
            }

            let y = column.used_height.saturating_add(margin);
            column.placements.push(Placement {
                block_index,
                source_start,
                source_end,
                y,
            });
            column.used_height = y.saturating_add(source_end.saturating_sub(source_start));
            source_start = source_end;
            first_fragment = false;
            if source_start < block.total_height {
                push_column(&mut columns)?;
            } else {
                column.used_height = column
                    .used_height
                    .saturating_add(block.margin_after)
                    .min(usable_height);
            }
        }
    }
    Ok(columns)
}

fn push_column(columns: &mut Vec<ColumnPlan>) -> Result<()> {
    columns
        .len()
        .checked_add(1)
        .context("rendered Markdown column count overflowed")?;
    columns.push(ColumnPlan::default());
    Ok(())
}

/// Plans columns and then rebalances them so multi-column images approach the
/// target aspect ratio instead of leaving a nearly empty trailing column.
///
/// The full-height greedy plan fixes the column-count ceiling `n_max` (and
/// propagates any planning error unchanged). For every candidate column count
/// a binary search finds the smallest usable column height that still fits in
/// that many columns; planner errors or overflowing column counts during the
/// search are treated as "too short" rather than fatal. The candidate whose
/// overall image is closest to `TARGET_ASPECT_RATIO` (log-distance, ties going
/// to fewer columns) wins.
fn plan_balanced_columns(
    layouts: &[LayoutBlock],
    config: &NormalizedConfig,
) -> Result<Vec<ColumnPlan>> {
    let max_usable = config
        .max_height
        .saturating_sub(config.padding.saturating_mul(2));
    let full_plan = plan_columns_with_height(layouts, max_usable)?;
    let column_ceiling = full_plan.len();
    if column_ceiling <= 1 {
        return Ok(full_plan);
    }

    let total_content: u64 = layouts
        .iter()
        .map(|block| u64::from(block.total_height))
        .sum();
    let height_floor = u64::from(
        MIN_RENDERED_HEIGHT
            .saturating_sub(config.padding.saturating_mul(2))
            .max(128),
    );
    let mut best: Option<(Vec<ColumnPlan>, f32)> = None;
    for candidate in 1..=column_ceiling {
        let low = total_content
            .div_ceil(candidate as u64)
            .max(height_floor)
            .min(u64::from(max_usable)) as u32;
        let Some(plan) = balanced_plan_for_count(layouts, candidate, low, max_usable) else {
            continue;
        };
        let distance = aspect_distance(&plan, config);
        let improves = best
            .as_ref()
            .map(|(_, best_distance)| distance + ASPECT_TIE_EPSILON < *best_distance)
            .unwrap_or(true);
        if improves {
            best = Some((plan, distance));
        }
    }
    Ok(best.map(|(plan, _)| plan).unwrap_or(full_plan))
}

/// Binary-searches the smallest usable height in `[low, high]` whose plan fits
/// in at most `target_columns` columns. Returns `None` when even the full
/// height `high` cannot satisfy the target.
fn balanced_plan_for_count(
    layouts: &[LayoutBlock],
    target_columns: usize,
    low: u32,
    high: u32,
) -> Option<Vec<ColumnPlan>> {
    let mut best = match plan_columns_with_height(layouts, high) {
        Ok(plan) if plan.len() <= target_columns => plan,
        _ => return None,
    };
    let mut low = low.min(high);
    let mut high = high;
    while low < high {
        let mid = low + (high - low) / 2;
        match plan_columns_with_height(layouts, mid) {
            Ok(plan) if plan.len() <= target_columns => {
                best = plan;
                high = mid;
            }
            _ => low = mid.saturating_add(1),
        }
    }
    Some(best)
}

/// Log-space distance between the finished image's aspect ratio (using the
/// same width/height rules as `render_pages`) and `TARGET_ASPECT_RATIO`.
fn aspect_distance(columns: &[ColumnPlan], config: &NormalizedConfig) -> f32 {
    let count = columns.len() as u64;
    let width = u64::from(config.padding) * 2
        + u64::from(COLUMN_WIDTH) * count
        + u64::from(COLUMN_GAP) * count.saturating_sub(1);
    let content_height = columns
        .iter()
        .map(|column| column.used_height)
        .max()
        .unwrap_or(0);
    let height = content_height
        .saturating_add(config.padding.saturating_mul(2))
        .clamp(MIN_RENDERED_HEIGHT, config.max_height);
    ((width as f32 / height as f32).ln() - TARGET_ASPECT_RATIO.ln()).abs()
}

fn render_pages(
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    layouts: &[LayoutBlock],
    columns: &[ColumnPlan],
    config: &NormalizedConfig,
    palette: Palette,
) -> Result<Vec<RenderedImage>> {
    let column_count = u32::try_from(columns.len()).context("too many image columns")?;
    let columns_width = COLUMN_WIDTH
        .checked_mul(column_count)
        .context("rendered image width overflowed")?;
    let gaps_width = COLUMN_GAP
        .checked_mul(column_count.saturating_sub(1))
        .context("rendered image gap width overflowed")?;
    let width = config
        .padding
        .checked_mul(2)
        .and_then(|padding| padding.checked_add(columns_width))
        .and_then(|width| width.checked_add(gaps_width))
        .context("rendered image width overflowed")?;
    let content_height = columns
        .iter()
        .map(|column| column.used_height)
        .max()
        .unwrap_or(0);
    let height = content_height
        .saturating_add(config.padding.saturating_mul(2))
        .clamp(MIN_RENDERED_HEIGHT, config.max_height);
    validate_page_dimensions(width, height)?;
    let pixels = u64::from(width) * u64::from(height);
    checked_total_page_pixels(0, pixels)?;

    let mut image = RgbaImage::from_pixel(width, height, Rgba(palette.background));
    for (column_index, column) in columns.iter().enumerate() {
        let column_index =
            u32::try_from(column_index).context("image column index does not fit in u32")?;
        let column_x = config
            .padding
            .saturating_add(column_index.saturating_mul(COLUMN_WIDTH.saturating_add(COLUMN_GAP)));
        for placement in &column.placements {
            let block = layouts
                .get(placement.block_index)
                .ok_or_else(|| anyhow!("renderer placement references a missing block"))?;
            let destination_y = config.padding.saturating_add(placement.y);
            if block.table.is_some() {
                draw_table_fragment(
                    &mut image,
                    font_system,
                    swash_cache,
                    block,
                    placement,
                    column_x,
                    destination_y,
                    palette,
                );
                continue;
            }
            draw_decoration(
                &mut image,
                block,
                placement,
                column_x,
                destination_y,
                palette,
            );
            draw_text_fragment(
                &mut image,
                font_system,
                swash_cache,
                block,
                placement,
                column_x,
                destination_y,
            );
        }
    }

    let png_limit = MAX_PAGE_PNG_BYTES.min(MAX_TOTAL_PNG_BYTES);
    let mut writer = CappedVecWriter::new(png_limit);
    let encoded = PngEncoder::new(&mut writer).write_image(
        image.as_raw(),
        width,
        height,
        ColorType::Rgba8.into(),
    );
    if let Err(error) = encoded {
        if writer.exceeded() {
            bail!("rendered image exceeds the {png_limit}-byte PNG limit");
        }
        return Err(error).context("failed to encode rendered Markdown as PNG");
    }
    let png = writer.into_inner();
    Ok(vec![RenderedImage {
        mime: "image/png".to_string(),
        png,
        width,
        height,
    }])
}

struct CappedVecWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl CappedVecWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }

    fn exceeded(&self) -> bool {
        self.exceeded
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for CappedVecWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(next_len) = self.bytes.len().checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(io::Error::other("rendered PNG byte budget exceeded"));
        };
        if next_len > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("rendered PNG byte budget exceeded"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn validate_page_dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0 {
        bail!("rendered image width must be non-zero");
    }
    if !(MIN_RENDERED_HEIGHT..=MAX_PAGE_HEIGHT).contains(&height) {
        bail!("rendered image height {height} is outside the supported range");
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > MAX_PAGE_PIXELS {
        bail!("rendered image would exceed the {MAX_PAGE_PIXELS}-pixel limit");
    }
    Ok(())
}

fn checked_total_page_pixels(current: u64, page: u64) -> Result<u64> {
    let total = current
        .checked_add(page)
        .context("rendered page pixel count overflowed")?;
    if total > MAX_TOTAL_PAGE_PIXELS {
        bail!("rendered Markdown exceeds the {MAX_TOTAL_PAGE_PIXELS}-pixel total limit");
    }
    Ok(total)
}

fn draw_decoration(
    image: &mut RgbaImage,
    block: &LayoutBlock,
    placement: &Placement,
    x: u32,
    y: u32,
    palette: Palette,
) {
    let height = placement.source_end.saturating_sub(placement.source_start);
    match block.kind {
        BlockKind::Code => {
            fill_rect(image, x, y, COLUMN_WIDTH, height, palette.code_background);
        }
        BlockKind::Quote => {
            fill_rect(image, x, y, COLUMN_WIDTH, height, palette.quote_background);
            fill_rect(image, x, y, 6, height, palette.quote_bar);
        }
        BlockKind::Rule => {
            let line_y = y.saturating_add(height / 2);
            fill_rect(image, x, line_y, COLUMN_WIDTH, 2, palette.rule);
        }
        BlockKind::Heading(1) if placement.source_end == block.total_height => {
            let line_y = y.saturating_add(height).saturating_sub(2);
            fill_rect(image, x, line_y, COLUMN_WIDTH, 2, palette.rule);
        }
        _ => {}
    }
    if placement.source_start == 0 {
        if let Some(task) = block.task {
            draw_checkbox(
                image,
                x.saturating_add(task.x),
                y.saturating_add(task.y),
                task.size,
                task.checked,
                palette.text,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_table_fragment(
    image: &mut RgbaImage,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    block: &LayoutBlock,
    placement: &Placement,
    column_x: u32,
    destination_y: u32,
    palette: Palette,
) {
    let Some(table) = block.table.as_ref() else {
        return;
    };
    for row in table.rows.iter().filter(|row| {
        row.source_start >= placement.source_start && row.source_end <= placement.source_end
    }) {
        let row_y =
            destination_y.saturating_add(row.source_start.saturating_sub(placement.source_start));
        let row_height = row.source_end.saturating_sub(row.source_start);
        let background = if row.header {
            palette.table_header_background
        } else if row.stripe {
            palette.quote_background
        } else {
            palette.table_background
        };
        fill_rect(image, column_x, row_y, COLUMN_WIDTH, row_height, background);
        fill_rect(image, column_x, row_y, COLUMN_WIDTH, 1, palette.border);
        fill_rect(
            image,
            column_x,
            row_y.saturating_add(row_height.saturating_sub(1)),
            COLUMN_WIDTH,
            1,
            palette.border,
        );
        for cell in &row.cells {
            let cell_x = column_x.saturating_add(cell.x);
            fill_rect(image, cell_x, row_y, 1, row_height, palette.border);
            if cell.x.saturating_add(cell.width) == COLUMN_WIDTH {
                fill_rect(
                    image,
                    cell_x.saturating_add(cell.width.saturating_sub(1)),
                    row_y,
                    1,
                    row_height,
                    palette.border,
                );
            }
            draw_table_cell_text(
                image,
                font_system,
                swash_cache,
                cell,
                cell_x.saturating_add(TABLE_CELL_PADDING),
                row_y.saturating_add(TABLE_CELL_PADDING),
                cell_x.saturating_add(cell.width.saturating_sub(TABLE_CELL_PADDING)),
                row_y.saturating_add(row_height.saturating_sub(TABLE_CELL_PADDING)),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_table_cell_text(
    image: &mut RgbaImage,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    cell: &LayoutTableCell,
    origin_x: u32,
    origin_y: u32,
    clip_x_end: u32,
    clip_y_end: u32,
) {
    for run in cell.buffer.layout_runs() {
        for (start_x, end_x) in inline_code_chip_ranges(run.glyphs) {
            let inset = (run.line_height * INLINE_CODE_CHIP_INSET_RATIO).max(2.0);
            let top = i64::from(origin_y) + (run.line_top + inset) as i64;
            let bottom = i64::from(origin_y) + (run.line_top + run.line_height - inset) as i64;
            let x0 = (i64::from(origin_x) + (start_x - INLINE_CODE_CHIP_PAD_X).floor() as i64)
                .max(i64::from(origin_x));
            let x1 = (i64::from(origin_x) + (end_x + INLINE_CODE_CHIP_PAD_X).ceil() as i64)
                .min(i64::from(clip_x_end));
            let bottom = bottom.min(i64::from(clip_y_end));
            let (Ok(x0), Ok(top)) = (u32::try_from(x0), u32::try_from(top)) else {
                continue;
            };
            if x1 <= i64::from(x0) || bottom <= i64::from(top) {
                continue;
            }
            fill_rect(
                image,
                x0,
                top,
                (x1 - i64::from(x0)) as u32,
                (bottom - i64::from(top)) as u32,
                cell.inline_code_background,
            );
        }
        for glyph in run.glyphs {
            if swash_cache.image_cache.len() >= MAX_CACHED_GLYPHS {
                swash_cache.image_cache.clear();
            }
            let physical = glyph.physical((0.0, 0.0), 1.0);
            let glyph_color = glyph.color_opt.unwrap_or(cell.default_color);
            swash_cache.with_pixels(
                font_system,
                physical.cache_key,
                glyph_color,
                |pixel_x, pixel_y, pixel_color| {
                    let global_x = i64::from(origin_x) + i64::from(physical.x) + i64::from(pixel_x);
                    let global_y = i64::from(origin_y)
                        + run.line_y as i64
                        + i64::from(physical.y)
                        + i64::from(pixel_y);
                    let (Ok(global_x), Ok(global_y)) =
                        (u32::try_from(global_x), u32::try_from(global_y))
                    else {
                        return;
                    };
                    if global_x < origin_x
                        || global_x >= clip_x_end
                        || global_y < origin_y
                        || global_y >= clip_y_end
                    {
                        return;
                    }
                    if let Some(destination) = image.get_pixel_mut_checked(global_x, global_y) {
                        destination.blend(&Rgba(pixel_color.as_rgba()));
                    }
                },
            );
        }
    }
}

fn draw_checkbox(image: &mut RgbaImage, x: u32, y: u32, size: u32, checked: bool, color: [u8; 4]) {
    if size < 4 {
        return;
    }
    fill_rect(image, x, y, size, 2, color);
    fill_rect(
        image,
        x,
        y.saturating_add(size.saturating_sub(2)),
        size,
        2,
        color,
    );
    fill_rect(image, x, y, 2, size, color);
    fill_rect(
        image,
        x.saturating_add(size.saturating_sub(2)),
        y,
        2,
        size,
        color,
    );
    if checked {
        draw_line(
            image,
            x.saturating_add(size / 5),
            y.saturating_add(size / 2),
            x.saturating_add(size * 2 / 5),
            y.saturating_add(size * 3 / 4),
            3,
            color,
        );
        draw_line(
            image,
            x.saturating_add(size * 2 / 5),
            y.saturating_add(size * 3 / 4),
            x.saturating_add(size * 4 / 5),
            y.saturating_add(size / 4),
            3,
            color,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    image: &mut RgbaImage,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    width: u32,
    color: [u8; 4],
) {
    let dx = i64::from(x1).saturating_sub(i64::from(x0));
    let dy = i64::from(y1).saturating_sub(i64::from(y0));
    let steps = dx.unsigned_abs().max(dy.unsigned_abs()).max(1);
    for step in 0..=steps {
        let x = i64::from(x0).saturating_add(
            dx.saturating_mul(step as i64)
                .checked_div(steps as i64)
                .unwrap_or(0),
        );
        let y = i64::from(y0).saturating_add(
            dy.saturating_mul(step as i64)
                .checked_div(steps as i64)
                .unwrap_or(0),
        );
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            continue;
        };
        fill_rect(image, x, y, width, width, color);
    }
}

fn draw_text_fragment(
    image: &mut RgbaImage,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    block: &LayoutBlock,
    placement: &Placement,
    column_x: u32,
    destination_y: u32,
) {
    let Some(buffer) = block.buffer.as_ref() else {
        return;
    };
    let clip_x_end = column_x.saturating_add(COLUMN_WIDTH);
    let clip_y_end =
        destination_y.saturating_add(placement.source_end.saturating_sub(placement.source_start));
    for run in buffer.layout_runs() {
        let run_top = block.vertical_padding as f32 + run.line_top;
        let run_bottom = run_top + run.line_height;
        if run_bottom <= placement.source_start as f32 || run_top >= placement.source_end as f32 {
            continue;
        }
        for (start_x, end_x) in inline_code_chip_ranges(run.glyphs) {
            let inset = (run.line_height * INLINE_CODE_CHIP_INSET_RATIO).max(2.0);
            let top = (run_top + inset).max(placement.source_start as f32);
            let bottom = (run_bottom - inset).min(placement.source_end as f32);
            if bottom <= top {
                continue;
            }
            let global_y = i64::from(destination_y) + top as i64
                - i64::from(placement.source_start);
            let x_base = i64::from(column_x) + i64::from(block.inset_left);
            let x0 = (x_base + (start_x - INLINE_CODE_CHIP_PAD_X).floor() as i64)
                .max(i64::from(column_x));
            let x1 = (x_base + (end_x + INLINE_CODE_CHIP_PAD_X).ceil() as i64)
                .min(i64::from(clip_x_end));
            let (Ok(x0), Ok(global_y)) = (u32::try_from(x0), u32::try_from(global_y)) else {
                continue;
            };
            if x1 <= i64::from(x0) {
                continue;
            }
            fill_rect(
                image,
                x0,
                global_y,
                (x1 - i64::from(x0)) as u32,
                (bottom - top) as u32,
                block.inline_code_background,
            );
        }
        for glyph in run.glyphs {
            if swash_cache.image_cache.len() >= MAX_CACHED_GLYPHS {
                swash_cache.image_cache.clear();
            }
            let physical = glyph.physical((0.0, 0.0), 1.0);
            let glyph_color = glyph.color_opt.unwrap_or(block.default_color);
            swash_cache.with_pixels(
                font_system,
                physical.cache_key,
                glyph_color,
                |pixel_x, pixel_y, pixel_color| {
                    let global_x = i64::from(column_x)
                        + i64::from(block.inset_left)
                        + i64::from(physical.x)
                        + i64::from(pixel_x);
                    let global_block_y = i64::from(block.vertical_padding)
                        + run.line_y as i64
                        + i64::from(physical.y)
                        + i64::from(pixel_y);
                    if global_block_y < i64::from(placement.source_start)
                        || global_block_y >= i64::from(placement.source_end)
                    {
                        return;
                    }
                    let global_y = i64::from(destination_y) + global_block_y
                        - i64::from(placement.source_start);
                    let (Ok(global_x), Ok(global_y)) =
                        (u32::try_from(global_x), u32::try_from(global_y))
                    else {
                        return;
                    };
                    if global_x < column_x
                        || global_x >= clip_x_end
                        || global_y < destination_y
                        || global_y >= clip_y_end
                    {
                        return;
                    }
                    if let Some(destination) = image.get_pixel_mut_checked(global_x, global_y) {
                        destination.blend(&Rgba(pixel_color.as_rgba()));
                    }
                },
            );
        }
    }
}

fn fill_rect(image: &mut RgbaImage, x: u32, y: u32, width: u32, height: u32, color: [u8; 4]) {
    let end_x = x.saturating_add(width).min(image.width());
    let end_y = y.saturating_add(height).min(image.height());
    for py in y.min(end_y)..end_y {
        for px in x.min(end_x)..end_x {
            if let Some(pixel) = image.get_pixel_mut_checked(px, py) {
                *pixel = Rgba(color);
            }
        }
    }
}

fn color(rgba: [u8; 4]) -> Color {
    Color::rgba(rgba[0], rgba[1], rgba[2], rgba[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(markdown: &str, raw_config: &RenderConfig) -> Result<Vec<RenderedImage>> {
        render_in_process_for_test(markdown, raw_config)
    }

    #[test]
    fn renderer_client_and_payloads_satisfy_async_bounds() {
        fn assert_send_static<T: Send + 'static>() {}
        fn assert_renderer<T: Clone + Send + Sync + 'static>() {}
        assert_send_static::<RenderConfig>();
        assert_send_static::<RenderedImage>();
        assert_renderer::<MarkdownImageRenderer>();
    }

    #[test]
    fn renderer_client_is_lazy_and_limits_are_bounded() {
        let renderer = MarkdownImageRenderer::new().unwrap();
        assert!(renderer.worker.try_lock().unwrap().process.is_none());
        assert_eq!(MAX_CACHED_GLYPHS, 2048);
        assert_eq!(WORKER_IDLE_TIMEOUT, Duration::from_secs(60 * 60));
        assert_eq!(RENDER_TIMEOUT, Duration::from_secs(60));
        // debug 二进制 550MB+,光映射自身就撞 512MB;开发构建放宽。
        #[cfg(not(debug_assertions))]
        assert_eq!(WORKER_ADDRESS_SPACE_LIMIT, 512 * 1024 * 1024);
        #[cfg(debug_assertions)]
        assert_eq!(WORKER_ADDRESS_SPACE_LIMIT, 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn renderer_loads_only_the_fonts_needed_by_the_request() {
        let font_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
        let mut renderer = RendererState::from_font_dir(&font_dir).unwrap();
        let config = NormalizedConfig::new(&RenderConfig::default());

        let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
        assert!(fonts.emoji.is_none());
        assert!(!renderer.emoji_loaded);
        let cjk_face_count = {
            let faces = renderer.font_system.db().faces().collect::<Vec<_>>();
            assert!(!faces.is_empty());
            assert!(faces.iter().all(|face| matches!(
                &face.source,
                fontdb::Source::File(path) if path == &font_dir.join(CJK_FONT_FILE)
            )));
            let families = faces
                .iter()
                .flat_map(|face| face.families.iter().map(|(name, _)| name.as_str()))
                .collect::<Vec<_>>();
            assert!(families.contains(&DEFAULT_BODY_FONT));
            assert!(families.contains(&DEFAULT_CODE_FONT));
            assert!(!families.contains(&DEFAULT_EMOJI_FONT));
            faces.len()
        };

        let fonts = renderer.resolve_config_fonts(&config, true).unwrap();
        assert_eq!(fonts.emoji.as_deref(), Some(DEFAULT_EMOJI_FONT));
        assert!(renderer.emoji_loaded);
        let with_emoji = renderer.font_system.db().faces().count();
        assert!(with_emoji > cjk_face_count);
        renderer.ensure_bundled_emoji_font().unwrap();
        assert_eq!(renderer.font_system.db().faces().count(), with_emoji);
    }

    #[test]
    fn missing_emoji_font_does_not_block_text_only_requests() {
        let font_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
        let mut renderer = RendererState::from_font_dir(&font_dir).unwrap();
        renderer.emoji_font_path = font_dir.join("missing-emoji.ttf");
        let config = NormalizedConfig::new(&RenderConfig::default());

        assert!(renderer.resolve_config_fonts(&config, false).is_ok());
        let error = renderer
            .resolve_config_fonts(&config, true)
            .err()
            .expect("missing Emoji font should fail only Emoji requests");
        assert!(error.to_string().contains("missing-emoji.ttf"));
    }

    #[test]
    fn emoji_detection_only_marks_emoji_graphemes() {
        assert!(!markdown_contains_emoji("纯中文 and `code`"));
        assert!(markdown_contains_emoji("完成 ✅"));
        assert!(markdown_contains_emoji("family 👨‍👩‍👧‍👦"));
    }

    #[tokio::test]
    async fn worker_binary_response_round_trips_without_base64() {
        let expected_png = b"\x89PNG\r\n\x1a\nworker".to_vec();
        let (mut worker_side, mut client_side) = tokio::io::duplex(1024);
        let write = write_worker_response(
            &mut worker_side,
            Ok(vec![RenderedImage {
                mime: "image/png".to_string(),
                png: expected_png.clone(),
                width: 960,
                height: MIN_RENDERED_HEIGHT,
            }]),
        );
        let read = read_worker_response(&mut client_side);
        let (write_result, read_result) = tokio::join!(write, read);
        write_result.unwrap();
        let images = read_result.unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].mime, "image/png");
        assert_eq!(images[0].png, expected_png);
        assert_eq!(images[0].width, 960);
        assert_eq!(images[0].height, MIN_RENDERED_HEIGHT);
    }

    #[tokio::test]
    async fn request_frames_enforce_the_input_budget() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        let payload = b"request";
        let (write_result, read_result) = tokio::join!(
            write_frame(&mut writer, payload),
            read_frame(&mut reader, MAX_REQUEST_FRAME_BYTES)
        );
        write_result.unwrap();
        assert_eq!(read_result.unwrap().unwrap(), payload);
        assert!(write_frame(
            &mut tokio::io::sink(),
            &vec![0; MAX_REQUEST_FRAME_BYTES + 1]
        )
        .await
        .is_err());
    }

    #[test]
    fn renders_supported_markdown_and_unicode_to_nonempty_png() {
        let markdown = r#"# Miyu 长回复 🚀

普通中文段落，包含 **粗体**、*斜体*、`inline code` 和 [链接文字](https://example.com)。

> 引用内容支持中文和 Emoji 😀。

- 第一项
- 第二项

1. ordered one
2. ordered two

```rust
fn main() {
    println!("hello");
}
```

| 名称 | 状态 |
| --- | --- |
| renderer | ready |

---

结束。"#;
        let pages = render(markdown, &RenderConfig::default()).unwrap();
        assert_eq!(pages.len(), 1);
        for page in pages {
            assert_eq!(page.mime, "image/png");
            assert!(page.png.starts_with(b"\x89PNG\r\n\x1a\n"));
            assert!((MIN_RENDERED_HEIGHT..=MAX_PAGE_HEIGHT).contains(&page.height));
            assert!(u64::from(page.width) * u64::from(page.height) <= MAX_PAGE_PIXELS);
            let decoded = image::load_from_memory(&page.png).unwrap().to_rgba8();
            assert_eq!(decoded.dimensions(), (page.width, page.height));
            let background = decoded.get_pixel(0, 0);
            assert!(decoded.pixels().any(|pixel| pixel != background));
        }
    }

    #[test]
    fn freshly_shaped_cjk_word_keeps_positive_advances() {
        // cosmic-text 0.15 在冷字体系统上首次整词塑形时,"背景"的首字形
        // advance 为 0,后续字形全部叠画在同一位置(0.19 修复)。锁死该回归:
        // 任何字形 advance 归零都会让文字叠加。
        let mut font_system = FontSystem::new();
        for text in ["背景", "背 景", "背包"] {
            let metrics = Metrics::new(36.0, 52.0);
            let mut buffer = Buffer::new(&mut font_system, metrics);
            buffer.set_size(Some(960.0), None);
            let attrs = Attrs::new().family(Family::SansSerif).metrics(metrics);
            buffer.set_rich_text(
                [(text, attrs.clone())],
                &attrs,
                Shaping::Advanced,
                None,
            );
            buffer.shape_until_scroll(&mut font_system, true);
            for run in buffer.layout_runs() {
                for glyph in run.glyphs {
                    assert!(
                        glyph.w > 0.0,
                        "{text:?} 中 start={} 的字形 advance 为 0",
                        glyph.start
                    );
                }
            }
        }
    }

    #[test]
    fn inline_code_gets_a_background_chip() {
        let count_chip_pixels = |markdown: &str| {
            let pages = render(markdown, &RenderConfig::default()).unwrap();
            let decoded = image::load_from_memory(&pages[0].png).unwrap().to_rgba8();
            let palette = Palette::for_theme(&RenderConfig::default().theme);
            decoded
                .pixels()
                .filter(|pixel| **pixel == Rgba(palette.code_background))
                .count()
        };
        let with_code = count_chip_pixels("行内 `code` 提示,以及 `第二段代码` 也要有底色。");
        let without_code = count_chip_pixels("行内 code 提示,没有任何反引号的对照段落。");
        assert!(
            with_code > 200,
            "行内代码应有底色块,实际命中 {with_code} 像素"
        );
        assert_eq!(without_code, 0, "无行内代码时不应出现底色像素");
    }

    #[test]
    fn input_limit_is_measured_in_unicode_characters() {
        let accepted = "界".repeat(MAX_INPUT_CHARS);
        let rejected = "界".repeat(MAX_INPUT_CHARS + 1);
        assert!(validate_markdown(&accepted).is_ok());
        assert!(validate_markdown(&rejected).is_err());
    }

    #[test]
    fn total_pixel_and_png_writers_enforce_hard_budgets() {
        assert_eq!(
            checked_total_page_pixels(MAX_TOTAL_PAGE_PIXELS - 1, 1).unwrap(),
            MAX_TOTAL_PAGE_PIXELS
        );
        assert!(checked_total_page_pixels(MAX_TOTAL_PAGE_PIXELS, 1).is_err());

        let mut writer = CappedVecWriter::new(3);
        writer.write_all(b"abc").unwrap();
        assert!(writer.write_all(b"d").is_err());
        assert!(writer.exceeded());
        assert_eq!(writer.into_inner(), b"abc");
    }

    #[test]
    fn html_only_output_is_not_rendered_as_a_blank_page() {
        let blocks = collect_blocks("<div>visible</div>");
        assert!(blocks
            .iter()
            .any(|block| { block.spans.iter().any(|span| span.text.contains("visible")) }));
    }

    #[test]
    fn fenced_configuration_keeps_heading_markers_inside_code() {
        let markdown = r#"下面是 Niri 配置：

```kdl
input {
    focus-follows-mouse
    keyboard { mod-key "Mod1" }
}
```

Kitty 透明度：

```conf
# ~/.config/kitty/kitty.conf
background_opacity 0.92
dynamic_background_opacity yes
```
"#;
        let blocks = collect_blocks(markdown);
        let code_blocks = blocks
            .iter()
            .filter(|block| block.kind == BlockKind::Code)
            .collect::<Vec<_>>();

        assert_eq!(code_blocks.len(), 2);
        let code_text = code_blocks
            .iter()
            .flat_map(|block| &block.spans)
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert!(code_text.contains("focus-follows-mouse"));
        assert!(code_text.contains("# ~/.config/kitty/kitty.conf"));
        assert!(!blocks.iter().any(|block| {
            matches!(block.kind, BlockKind::Heading(_))
                && block
                    .spans
                    .iter()
                    .any(|span| span.text.contains("kitty.conf"))
        }));
    }

    #[test]
    fn code_surface_remains_distinct_after_qq_sized_downscale() {
        let markdown = r#"正文内容用于对比代码块。

```kdl
# ~/.config/kitty/kitty.conf
background_opacity 0.92
```

代码块之后的正文。"#;
        let raw_config = RenderConfig::default();
        let config = NormalizedConfig::new(&raw_config);
        let palette = Palette::for_theme("paper");
        let mut renderer = RendererState::new().unwrap();
        let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
        let layouts = layout_blocks(
            &mut renderer.font_system,
            collect_blocks(markdown),
            &config,
            palette,
            &fonts,
        )
        .unwrap();
        let code_index = layouts
            .iter()
            .position(|block| block.kind == BlockKind::Code)
            .expect("fenced block should use the code layout");
        let columns = plan_columns(&layouts, &config).unwrap();
        let placement = columns
            .iter()
            .flat_map(|column| &column.placements)
            .find(|placement| placement.block_index == code_index)
            .expect("code block placement");
        let code_y = config.padding + placement.y;
        let sample_y = code_y + (placement.source_end - placement.source_start) / 2;

        let page = render(markdown, &raw_config).unwrap().remove(0);
        let image = image::load_from_memory(&page.png).unwrap().to_rgba8();
        let outside_x = config.padding / 2;
        let inside_x = config.padding + COLUMN_WIDTH - 12;
        assert_eq!(
            *image.get_pixel(outside_x, sample_y),
            Rgba(palette.background)
        );
        assert_eq!(
            *image.get_pixel(inside_x, sample_y),
            Rgba(palette.code_background)
        );
        assert_eq!(
            *image.get_pixel(config.padding, sample_y),
            Rgba(palette.code_background)
        );

        let scaled_width = 568_u32;
        let scaled_height = (u64::from(page.height) * u64::from(scaled_width)
            / u64::from(page.width))
        .max(1) as u32;
        let scaled = image::imageops::resize(
            &image,
            scaled_width,
            scaled_height,
            image::imageops::FilterType::Triangle,
        );
        let scale_x =
            |x: u32| (u64::from(x) * u64::from(scaled_width) / u64::from(page.width)) as u32;
        let scale_y =
            |y: u32| (u64::from(y) * u64::from(scaled_height) / u64::from(page.height)) as u32;
        let outside = scaled.get_pixel(scale_x(outside_x), scale_y(sample_y));
        let inside = scaled.get_pixel(scale_x(inside_x), scale_y(sample_y));
        let rgb_distance = (0..3)
            .map(|channel| u32::from(outside[channel].abs_diff(inside[channel])))
            .sum::<u32>();
        assert!(
            rgb_distance > 50,
            "downscaled code surface contrast was only {rgb_distance}"
        );
    }

    #[test]
    fn extreme_config_values_are_clamped_and_missing_fonts_fall_back() {
        let config = RenderConfig {
            theme: "unknown".to_string(),
            max_height: 1,
            font_size: 0,
            code_font_size: u32::MAX,
            padding: u32::MAX,
            font: "/definitely/missing/body.ttf".to_string(),
            title_font: "/definitely/missing/title.ttf".to_string(),
            code_font: "/definitely/missing/code.ttf".to_string(),
            emoji_font: "/definitely/missing/emoji.ttf".to_string(),
        };
        let pages = render("fallback 中文 😀", &config).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(
            NormalizedConfig::new(&config).max_height,
            MIN_CONFIGURED_HEIGHT
        );
        assert_eq!(pages[0].height, MIN_RENDERED_HEIGHT);
        assert!(!pages[0].png.is_empty());
    }

    #[test]
    fn empty_markdown_produces_a_valid_blank_page() {
        let pages = render("", &RenderConfig::default()).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].mime, "image/png");
        assert_eq!(pages[0].height, MIN_RENDERED_HEIGHT);
        assert!(image::load_from_memory(&pages[0].png).is_ok());
    }

    #[test]
    fn task_list_markers_are_structured_instead_of_literal_text() {
        let blocks = collect_blocks("- [ ] pending\n- [x] complete\n");
        let tasks = blocks
            .iter()
            .filter_map(|block| block.task.map(|checked| (checked, block)))
            .collect::<Vec<_>>();
        assert_eq!(tasks.len(), 2);
        assert!(!tasks[0].0);
        assert!(tasks[1].0);
        for (_, block) in tasks {
            let text = block
                .spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>();
            assert!(!text.contains('•'));
            assert!(!text.contains("[ ]"));
            assert!(!text.contains("[x]"));
        }
    }

    #[test]
    fn checked_task_box_has_drawn_check_while_empty_box_does_not() {
        let background = [255, 255, 255, 255];
        let foreground = [1, 2, 3, 255];
        let mut unchecked = RgbaImage::from_pixel(40, 40, Rgba(background));
        let mut checked = unchecked.clone();
        draw_checkbox(&mut unchecked, 5, 5, 24, false, foreground);
        draw_checkbox(&mut checked, 5, 5, 24, true, foreground);
        assert_eq!(*unchecked.get_pixel(17, 17), Rgba(background));
        assert!(checked
            .enumerate_pixels()
            .any(|(x, y, pixel)| (9..27).contains(&x)
                && (9..27).contains(&y)
                && *pixel == Rgba(foreground)));
    }

    #[test]
    fn table_parser_preserves_cells_rows_and_alignment() {
        let blocks =
            collect_blocks("| Left | Center | Right |\n| :--- | :---: | ---: |\n| a | b | c |\n");
        let table = blocks
            .iter()
            .find_map(|block| block.table.as_ref())
            .expect("structured table");
        assert_eq!(
            table.alignments,
            vec![Alignment::Left, Alignment::Center, Alignment::Right]
        );
        assert_eq!(table.header.len(), 3);
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0].len(), 3);
        assert!(table.header[0].iter().all(|span| span.style.bold));
    }

    #[test]
    fn code_block_uses_remaining_column_space_before_continuing() {
        let mut markdown = String::from("```text\n");
        for line in 0..8 {
            markdown.push_str(&format!("first {line}\n"));
        }
        markdown.push_str("```\n\n```text\n");
        for line in 0..12 {
            markdown.push_str(&format!("second {line}\n"));
        }
        markdown.push_str("```\n");

        let config = NormalizedConfig::new(&RenderConfig {
            max_height: MIN_CONFIGURED_HEIGHT,
            ..RenderConfig::default()
        });
        let mut renderer = RendererState::new().unwrap();
        let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
        let layouts = layout_blocks(
            &mut renderer.font_system,
            collect_blocks(&markdown),
            &config,
            Palette::for_theme("paper"),
            &fonts,
        )
        .unwrap();
        assert_eq!(layouts.len(), 2);
        let columns = plan_balanced_columns(&layouts, &config).unwrap();
        let placement = columns[0]
            .placements
            .iter()
            .find(|placement| placement.block_index == 1)
            .expect("second code block should begin in the first column");
        assert_eq!(placement.source_start, 0);
        assert!(placement.y > 0);
        assert!(placement.source_end < layouts[1].total_height);
    }

    #[test]
    fn table_continuation_repeats_header_and_never_splits_rows() {
        let mut markdown = String::from("| Name | Value |\n| --- | ---: |\n");
        for row in 0..24 {
            markdown.push_str(&format!("| row {row} | {row} |\n"));
        }
        let config = NormalizedConfig::new(&RenderConfig {
            max_height: MIN_CONFIGURED_HEIGHT,
            ..RenderConfig::default()
        });
        let mut renderer = RendererState::new().unwrap();
        let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
        let layouts = layout_blocks(
            &mut renderer.font_system,
            collect_blocks(&markdown),
            &config,
            Palette::for_theme("paper"),
            &fonts,
        )
        .unwrap();
        let table = layouts[0].table.as_ref().unwrap();
        let columns = plan_balanced_columns(&layouts, &config).unwrap();
        assert!(columns.len() > 1);
        for column in columns.iter().skip(1) {
            let header = column.placements.first().expect("repeated table header");
            assert_eq!(header.source_start, 0);
            assert_eq!(header.source_end, table.header_height);
        }
        for placement in columns.iter().flat_map(|column| &column.placements) {
            assert!(
                placement.source_start == 0
                    || layouts[0].boundaries.contains(&placement.source_start)
            );
            assert!(layouts[0].boundaries.contains(&placement.source_end));
        }
    }

    #[test]
    fn rendered_table_has_grid_header_and_zebra_backgrounds() {
        let markdown = "| A | B |\n| --- | --- |\n| one | two |\n| three | four |\n";
        let raw_config = RenderConfig::default();
        let config = NormalizedConfig::new(&raw_config);
        let palette = Palette::for_theme("paper");
        let mut renderer = RendererState::new().unwrap();
        let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
        let layouts = layout_blocks(
            &mut renderer.font_system,
            collect_blocks(markdown),
            &config,
            palette,
            &fonts,
        )
        .unwrap();
        let table = layouts[0].table.as_ref().unwrap();
        let header = &table.rows[0];
        let first = &table.rows[1];
        let second = &table.rows[2];
        let page = render(markdown, &raw_config).unwrap().remove(0);
        let image = image::load_from_memory(&page.png).unwrap().to_rgba8();
        let x = config.padding + COLUMN_WIDTH - 5;
        assert_eq!(
            *image.get_pixel(x, config.padding + 5),
            Rgba(palette.table_header_background)
        );
        assert_eq!(
            *image.get_pixel(x, config.padding + first.source_start + 5),
            Rgba(palette.table_background)
        );
        assert_eq!(
            *image.get_pixel(x, config.padding + second.source_start + 5),
            Rgba(palette.quote_background)
        );
        let grid_x = config.padding + header.cells[0].width;
        assert_eq!(
            *image.get_pixel(grid_x, config.padding + header.source_end / 2),
            Rgba(palette.border)
        );
    }

    #[test]
    fn long_content_grows_one_image_past_three_columns() {
        let mut markdown = String::from("```text\n");
        for line in 0..70 {
            markdown.push_str(&format!("line {line:02}: rendered column content\n"));
        }
        markdown.push_str("```\n");
        let config = RenderConfig {
            max_height: MIN_CONFIGURED_HEIGHT,
            ..RenderConfig::default()
        };
        let pages = render(&markdown, &config).unwrap();
        assert_eq!(pages.len(), 1);
        let page = &pages[0];
        let old_three_column_width = config.padding * 2 + COLUMN_WIDTH * 3 + COLUMN_GAP * 2;
        assert!(page.width > old_three_column_width);
        assert!((MIN_RENDERED_HEIGHT..=MIN_CONFIGURED_HEIGHT).contains(&page.height));
        // Balancing shares the trailing partial column across all columns, so
        // the finished image no longer stays pinned at the full page height.
        assert!(page.height < NormalizedConfig::new(&config).max_height);
        assert!(u64::from(page.width) * u64::from(page.height) <= MAX_PAGE_PIXELS);
    }

    fn code_layouts_for_balancing(lines: u32) -> (NormalizedConfig, Vec<LayoutBlock>) {
        let mut markdown = String::from("```text\n");
        for line in 0..lines {
            markdown.push_str(&format!("line {line:02}: rendered column content\n"));
        }
        markdown.push_str("```\n");
        let config = NormalizedConfig::new(&RenderConfig {
            max_height: MIN_CONFIGURED_HEIGHT,
            ..RenderConfig::default()
        });
        let mut renderer = RendererState::new().unwrap();
        let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
        let layouts = layout_blocks(
            &mut renderer.font_system,
            collect_blocks(&markdown),
            &config,
            Palette::for_theme("paper"),
            &fonts,
        )
        .unwrap();
        (config, layouts)
    }

    #[test]
    fn balanced_columns_have_similar_used_heights() {
        let (config, layouts) = code_layouts_for_balancing(70);
        let usable_height = config.max_height - config.padding * 2;
        let greedy = plan_columns(&layouts, &config).unwrap();
        let balanced = plan_balanced_columns(&layouts, &config).unwrap();
        assert!(balanced.len() > 1);
        let heights = |columns: &[ColumnPlan]| {
            let min = columns.iter().map(|c| c.used_height).min().unwrap();
            let max = columns.iter().map(|c| c.used_height).max().unwrap();
            (min, max)
        };
        let (greedy_min, greedy_max) = heights(&greedy);
        let (balanced_min, balanced_max) = heights(&balanced);
        assert!(balanced_max - balanced_min < usable_height * 30 / 100);
        assert!(balanced_max - balanced_min < greedy_max - greedy_min);
    }

    #[test]
    fn balancing_removes_trailing_sliver_column_and_shrinks_height() {
        let (config, layouts) = code_layouts_for_balancing(60);
        let usable_height = config.max_height - config.padding * 2;
        let greedy = plan_columns(&layouts, &config).unwrap();
        let sliver = greedy.last().unwrap().used_height;
        assert!(
            sliver < usable_height / 4,
            "test premise: greedy leaves a nearly empty last column, got {sliver}"
        );
        let balanced = plan_balanced_columns(&layouts, &config).unwrap();
        assert!(balanced.len() > 1);
        let min = balanced.iter().map(|c| c.used_height).min().unwrap();
        let max = balanced.iter().map(|c| c.used_height).max().unwrap();
        assert!(min * 2 >= max, "no column holds under half of the tallest");
        assert!(
            max + config.padding * 2 < config.max_height,
            "balanced image should shrink below the full page height"
        );
    }

    #[test]
    fn documents_over_the_pixel_budget_fail_instead_of_truncating() {
        let mut markdown = String::from("```text\n");
        for _ in 0..500 {
            markdown.push_str("x\n");
        }
        markdown.push_str("```\n");
        let config = RenderConfig {
            max_height: MIN_CONFIGURED_HEIGHT,
            font_size: 56,
            code_font_size: 52,
            padding: 160,
            ..RenderConfig::default()
        };
        let error = render(&markdown, &config).unwrap_err();
        assert!(error.to_string().contains("pixel limit"));
    }
}
