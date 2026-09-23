//! `render_image`：把 markdown 渲成成品图片，交给她自己发（用户 09-22 拍板）。
//!
//! 平台上早就有「长文自动转图」（`reply_processor` 的 `before_send` 钩子），但
//! 那是**被动**的：只在整条回复超过阈值时触发，她没有任何办法说「这一段请出成
//! 图」。而群里真正需要出图的恰恰是她主动想画的东西——流程图、对照表、结构化
//! 的说明。
//!
//! 这里只做编排：整篇 markdown 交给 `plugins::renderer` 那个渲染子进程
//! （cosmic-text，按 4:3 自动分栏分页）。` ```mermaid ` 围栏由渲染器**内嵌**成
//! 块内位图——第一版让它单独成图，结果一篇文档在图的位置被截断、一张变好几张
//! （用户 09-22 实测打回），所以改成当普通块走正常排版。
//!
//! 出图不投递：路径交回给她，发不发、发哪张由她定（与搜图、生图同一口径）。

use anyhow::{bail, Context, Result};
use miyu_base::paths::MiyuPaths;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use super::plugins::renderer::{MarkdownImageRenderer, RenderConfig};
use crate::platforms::PlatformTurnContext;
use miyu_base::platform_types::PlatformContextFileRef;

/// 整个 daemon 共用一个渲染子进程（`MarkdownImageRenderer` 内部是
/// `Arc<Mutex<WorkerSlot>>`，克隆便宜、进程复用）。
fn renderer() -> Result<MarkdownImageRenderer> {
    static RENDERER: OnceLock<std::result::Result<MarkdownImageRenderer, String>> = OnceLock::new();
    RENDERER
        .get_or_init(|| MarkdownImageRenderer::new().map_err(|error| error.to_string()))
        .clone()
        .map_err(|error| anyhow::anyhow!(error))
}

/// 一次最多读多少源码。与 `read_platform_file` 同一档，再长的文档渲出来也没法看。
const MAX_SOURCE_BYTES: usize = 128 * 1024;

/// 落盘目录：与搜图同一套（成员跑时进成员自己家，别写到管理员的 pictures）。
pub(crate) fn rendered_dir_for(
    paths: &MiyuPaths,
    config: &miyu_base::config::AppConfig,
) -> PathBuf {
    match config.member_home_dir() {
        Some(home) => home.join("pictures"),
        None => paths.pictures_dir.clone(),
    }
    .join("rendered")
}

/// 读一个 markdown 文件。
///
/// **只认 markdown 后缀**（用户 09-22 拍板：只能传 md，不限路径）。这是这件工具
/// 唯一的准入判据——它读到什么就会渲成图发到群里，不收口的话群里一句「渲染一下
/// xxx」就能把宿主上任意纯文本变成一张图。收到后缀之后，`~/.ssh/id_rsa`、
/// `/etc/passwd`、`config.jsonc` 这些都进不来。
///
/// 沙箱守卫照旧走一遍：`/sandbox` 会话锁着的目录仍然锁着，这条与准入无关。
fn read_markdown_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read as _;

    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(extension.as_deref(), Some("md") | Some("markdown")) {
        bail!(
            "only .md / .markdown files can be drawn: {}",
            path.display()
        );
    }
    miyu_base::sandbox::guard_read(path)?;
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take((MAX_SOURCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() > MAX_SOURCE_BYTES {
        bytes.truncate(MAX_SOURCE_BYTES);
        if let Err(error) = std::str::from_utf8(&bytes) {
            bytes.truncate(error.valid_up_to());
        }
    }
    if bytes.contains(&0) {
        bail!("`{}` looks binary", path.display());
    }
    String::from_utf8(bytes).with_context(|| format!("`{}` is not UTF-8 text", path.display()))
}

fn write_png(dir: &std::path::Path, bytes: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    // 按内容哈希命名：同一张图重复渲染不会堆文件，也便于去重闸认出同一张。
    let name = format!("render-{}.png", blake3::hash(bytes).to_hex());
    let path = dir.join(name);
    if !path.exists() {
        std::fs::write(&path, bytes)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(path)
}

pub(crate) fn register(
    registry: &mut miyu_engine::tools::ToolRegistry,
    context: Arc<PlatformTurnContext>,
    files: Vec<PlatformContextFileRef>,
) {
    let files = Arc::new(files);
    registry.register(
        miyu_engine::tools::ToolSpec::new(
            "render_image",
            "Render Markdown into a finished picture on this host: tables, structured notes, and ```mermaid fences, which are drawn as real diagrams inside the page. Very long input becomes several pages. Returns local paths and delivers nothing, so send the ones worth showing in one send_message_to_user call.",
            json!({
                "type": "object",
                "properties": {
                    "markdown": {
                        "type": "string",
                        "description": "The Markdown to draw. A ```mermaid fence becomes a diagram in place."
                    },
                    "file": {
                        "type": "string",
                        "description": "Draw a Markdown file instead: a path to a .md file, or a file id from chat history. Its text never passes through you."
                    }
                },
                "additionalProperties": false
            }),
            move |arguments| {
                let context = context.clone();
                let files = files.clone();
                async move { render(arguments, context, files).await }
            },
        )
        .with_display_name(miyu_base::i18n::text("Render image", "渲染图片")),
    );
}

async fn render(
    arguments: Value,
    context: Arc<PlatformTurnContext>,
    files: Arc<Vec<PlatformContextFileRef>>,
) -> Result<String> {
    let inline = arguments
        .get("markdown")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let reference = arguments
        .get("file")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    // 二选一。两个都给会让「到底画的哪份」说不清,不如当场问清楚。
    let markdown = match (inline, reference) {
        (Some(_), Some(_)) => bail!("give either markdown or file, not both"),
        (Some(text), None) => text.to_string(),
        (None, Some(reference)) => {
            // 聊天记录里的文件 id 要先下载；别的一律当路径。
            let path = if files.iter().any(|file| file.id == reference) {
                super::file_reader::resolve_platform_file(&context, &files, reference)
                    .await?
                    .0
            } else {
                super::file_reader::expand_home(std::path::Path::new(reference))
            };
            read_markdown_file(&path)?
        }
        (None, None) => bail!("markdown or file is required"),
    };
    let markdown = markdown.trim();
    if markdown.is_empty() {
        bail!("nothing to draw");
    }
    let pages = renderer()?
        .render(markdown, &RenderConfig::default())
        .await?;
    if pages.is_empty() {
        bail!("nothing was rendered");
    }
    let dir = rendered_dir_for(&context.paths, &context.config);
    let paths = pages
        .iter()
        .map(|page| write_png(&dir, &page.png))
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "ok": true,
        "count": paths.len(),
        "images": paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        "assistant_instruction": "The images are on this host, not delivered. Send them in one send_message_to_user call: images takes the whole list, and the caption belongs in that same call's text. The reader cannot open a local path, so keep paths out of message text.",
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本回合没有附件时的文件清单。
    fn no_files() -> Arc<Vec<PlatformContextFileRef>> {
        Arc::new(Vec::new())
    }

    /// 用户 09-22 实测打回的那条：一篇「正文 + 流程图 + 正文」必须是**一张**图，
    /// 不能在图的位置被截断。
    ///
    /// 判据用页数：内容很短，正常排版就一页；第一版把 mermaid 切出去单独渲，
    /// 同样的输入会出三张。
    #[tokio::test]
    async fn a_diagram_does_not_split_the_document() {
        let source =
            "# 标题\n\n开头一句话。\n\n```mermaid\ngraph TD; A-->B; B-->C;\n```\n\n结尾一句话。\n";
        let pages = renderer()
            .expect("渲染器该起得来")
            .render(source, &RenderConfig::default())
            .await
            .expect("该渲得出来");
        assert_eq!(pages.len(), 1, "被截断成了 {} 张", pages.len());
        let decoded = image::load_from_memory(&pages[0].png).expect("该是能解码的 PNG");
        assert!(decoded.width() > 0 && decoded.height() > 0);
    }

    /// 图真的画进去了：同一篇内容，带图那版必然比不带图那版高一截。
    ///
    /// 光看「渲出来了」证明不了图画进去没有——mermaid 围栏被当成代码块时也照样
    /// 出一张图，只是里面是源码。高度差才分得出来。
    #[tokio::test]
    async fn the_diagram_is_actually_drawn_into_the_page() {
        let with_diagram = "开头\n\n```mermaid\ngraph TD; A-->B; B-->C; C-->D;\n```\n\n结尾\n";
        let without = "开头\n\n结尾\n";
        let renderer = renderer().expect("渲染器该起得来");
        let tall = renderer
            .render(with_diagram, &RenderConfig::default())
            .await
            .expect("该渲得出来");
        let short = renderer
            .render(without, &RenderConfig::default())
            .await
            .expect("该渲得出来");
        assert_eq!(tall.len(), 1);
        assert_eq!(short.len(), 1);
        assert!(
            tall[0].height > short[0].height + 100,
            "带图那版只高了 {} 像素，图多半没画进去",
            tall[0].height.saturating_sub(short[0].height)
        );
    }

    /// 图的底色要跟纸面一个色（用户 09-22：正文米色、图纯白，一眼看出是贴上去的）。
    ///
    /// 判据是整张成图里**一个纯白像素都不该有**：paper 主题的纸面是
    /// `[244,239,229]`，正文里本来就没有纯白；写死 `fill(WHITE)` 的那一版会在
    /// 图那一块留下一大片 255。
    #[tokio::test]
    async fn the_diagram_background_matches_the_page() {
        let pages = renderer()
            .expect("渲染器该起得来")
            .render(
                "开头\n\n```mermaid\ngraph TD; A-->B; B-->C;\n```\n\n结尾\n",
                &RenderConfig::default(),
            )
            .await
            .expect("该渲得出来");
        assert_eq!(pages.len(), 1);
        let decoded = image::load_from_memory(&pages[0].png)
            .expect("该是能解码的 PNG")
            .to_rgba8();
        let white = decoded
            .pixels()
            .filter(|pixel| pixel.0[0] == 255 && pixel.0[1] == 255 && pixel.0[2] == 255)
            .count();
        assert_eq!(white, 0, "图里还留着 {white} 个纯白像素");

        // 纸面色本身得占大头——别把「没有白」做成「整张都是别的颜色」。
        let paper = decoded
            .pixels()
            .filter(|pixel| pixel.0 == [244, 239, 229, 255])
            .count();
        assert!(
            paper > decoded.pixels().count() / 4,
            "纸面色只占 {paper}/{}，底色多半没铺对",
            decoded.pixels().count()
        );
    }

    /// 画不出来的图退回成代码块，别把整篇带崩——她至少还能看见自己写了什么。
    #[tokio::test]
    async fn a_broken_diagram_falls_back_to_a_code_block() {
        let source = "```mermaid\n这不是 mermaid {{{\n```\n";
        let pages = renderer()
            .expect("渲染器该起得来")
            .render(source, &RenderConfig::default())
            .await
            .expect("坏图不该让整篇失败");
        assert_eq!(pages.len(), 1);
        assert!(pages[0].height > 0);
    }

    /// 给 .md 文件就直接画它，内容不必先读进上下文再传回来（用户 09-22 拍板 A）。
    ///
    /// 准入只看后缀（用户 09-22：只能传 md，不限路径）——工具读到什么就会渲成图
    /// 发到群里，不收口的话群里一句「渲染一下 xxx」就能把宿主上任意纯文本变成
    /// 一张图。所以这条同时钉住三件事：任意路径能读、非 .md 被拒、抬头不混进图里。
    #[tokio::test]
    async fn any_markdown_path_is_drawn_but_other_files_are_not() {
        let (temp, context) = crate::platforms::tests::shared::built_in_test_context(
            crate::platforms::ConversationKind::Private,
        );
        // 故意放在 platform_files 之外:路径不该再有限制。
        let doc = temp.path().join("note.md");
        std::fs::write(
            &doc,
            "# 从文件来的\n\n正文一句话。\n\n```mermaid\ngraph TD; A-->B;\n```\n",
        )
        .unwrap();
        let raw = render(
            json!({ "file": doc.display().to_string() }),
            context.clone(),
            no_files(),
        )
        .await
        .expect("任意路径下的 .md 都该画得出来");
        let result: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(result["count"], json!(1), "带图也该是一张:{result}");

        // 非 markdown 一律拒:这是唯一的准入判据。
        for name in ["secret", "id_rsa", "config.jsonc", "notes.txt"] {
            let other = temp.path().join(name);
            std::fs::write(&other, "不该被渲出来").unwrap();
            assert!(
                render(
                    json!({ "file": other.display().to_string() }),
                    context.clone(),
                    no_files(),
                )
                .await
                .is_err(),
                "{name} 不该读得到"
            );
        }

        // 两个都给说不清画的哪份,当场拒。
        assert!(
            render(
                json!({ "markdown": "# 甲", "file": doc.display().to_string() }),
                context.clone(),
                no_files(),
            )
            .await
            .is_err(),
            "markdown 与 file 该二选一"
        );
    }

    /// 整件工具跑一遍：出图、落盘到 rendered 目录、路径交回去；空输入直接拒。
    ///
    /// 上面几条测的是渲染器，这条测工具自己的契约——尤其是「不投递、只给路径」
    /// 这一口径（与搜图、生图同源）。
    #[tokio::test]
    async fn the_tool_writes_files_and_returns_their_paths() {
        let (_temp, context) = crate::platforms::tests::shared::built_in_test_context(
            crate::platforms::ConversationKind::Private,
        );

        assert!(
            render(json!({ "markdown": "   \n " }), context.clone(), no_files())
                .await
                .is_err(),
            "空输入该被拒"
        );
        assert!(
            render(json!({}), context.clone(), no_files())
                .await
                .is_err(),
            "缺参数该被拒"
        );

        let raw = render(
            json!({ "markdown": "# 标题\n\n正文一句话。\n" }),
            context.clone(),
            no_files(),
        )
        .await
        .expect("该渲得出来");
        let result: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(result["ok"], json!(true));
        let images = result["images"].as_array().expect("该给路径清单");
        assert!(!images.is_empty());
        let dir = rendered_dir_for(&context.paths, &context.config);
        for entry in images {
            let path = std::path::Path::new(entry.as_str().unwrap());
            assert!(path.is_file(), "没真写出来：{}", path.display());
            assert!(
                path.starts_with(&dir),
                "写到 rendered 之外去了：{}",
                path.display()
            );
            assert!(
                image::load_from_memory(&std::fs::read(path).unwrap()).is_ok(),
                "不是能解码的 PNG"
            );
        }
        // 只给路径,不投递:结果里不该出现任何「已发送」的字样。
        assert!(result.get("message_ids").is_none(), "{result}");
    }
}
