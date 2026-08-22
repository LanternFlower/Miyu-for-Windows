//! 图片输入的解析与视觉模型的选路。
//!
//! 用户发的图片可能来自剪贴板、平台消息、或历史里的占位符。三条路都要落到同一
//! 组路径上，再决定交给谁：`should_use_active_text_pool_for_images` 判断当前文
//! 本模型池能不能直接吃图，不能就走独立的视觉工具。
//!
//! 判定要求池里**每个**模型都支持视觉（`active_text_pool_supports_vision`），
//! 因为池内是随机选的——只要有一个不支持，就会随机地失败。

use crate::agent::*;

pub(in crate::agent) fn queued_prompt_images(
    prompt: &QueuedPrompt,
) -> Result<Vec<Option<PastedImage>>> {
    prompt
        .attachments
        .iter()
        .map(|attachment| match attachment {
            QueuedPromptAttachment::Binary { mime, data_base64 } => {
                let data = base64::engine::general_purpose::STANDARD
                    .decode(data_base64)
                    .map_err(|error| anyhow::anyhow!("invalid queued image data: {error}"))?;
                Ok(Some(PastedImage::Binary(ClipboardImage::new(
                    mime.clone(),
                    data,
                ))))
            }
            QueuedPromptAttachment::Path { path } => Ok(Some(PastedImage::Path(path.clone()))),
        })
        .collect()
}

pub(in crate::agent) fn clipboard_binary_image_from_tool_result(
    tool_name: &str,
    output: &str,
) -> Option<ClipboardImage> {
    if tool_name != "read_clipboard" {
        return None;
    }
    let value = serde_json::from_str::<Value>(output).ok()?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    if value.get("kind").and_then(Value::as_str) != Some("clipboard") {
        return None;
    }
    if value.get("content_type").and_then(Value::as_str) != Some("image") {
        return None;
    }
    if value.get("source").and_then(Value::as_str) != Some("clipboard_binary") {
        return None;
    }
    let path = value.get("path").and_then(Value::as_str)?;
    let mime = value
        .get("mime")
        .and_then(Value::as_str)
        .unwrap_or("image/png")
        .to_string();
    let data = std::fs::read(path).ok()?;
    Some(ClipboardImage::new(mime, data))
}

pub(in crate::agent) fn resolve_pasted_image_paths(
    images: &[Option<PastedImage>],
    paths: &MiyuPaths,
    image_platform: Option<&str>,
) -> Vec<Option<String>> {
    images
        .iter()
        .enumerate()
        .map(|(i, image)| match image {
            Some(PastedImage::Binary(img)) => image_platform
                .map(|platform| {
                    img.write_cache_file(
                        &paths.cache_dir,
                        &PathBuf::from("platform_images").join(platform),
                    )
                })
                .unwrap_or_else(|| img.write_temp_file(&paths.cache_dir, i + 1))
                .ok()
                .map(|path| path.display().to_string()),
            Some(PastedImage::Path(path)) => Some(path.clone()),
            None => None,
        })
        .collect()
}

pub(in crate::agent) fn rewrite_image_placeholders_with_paths(
    input: &str,
    paths: &[Option<String>],
) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("[Image ") {
        output.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let Some(end) = after_start.find(']') else {
            output.push_str(after_start);
            return output;
        };
        let placeholder = &after_start[..=end];
        if let Some(index) = image_placeholder_index(placeholder) {
            if let Some(Some(path)) = paths.get(index - 1) {
                output.push_str(&format!("[Image {index}: {path}]"));
            } else {
                output.push_str(placeholder);
            }
        } else {
            output.push_str(placeholder);
        }
        rest = &after_start[end + 1..];
    }
    output.push_str(rest);
    output
}

pub(in crate::agent) fn image_placeholder_index(placeholder: &str) -> Option<usize> {
    let inner = placeholder
        .strip_prefix("[Image ")?
        .strip_suffix(']')?
        .trim_start();
    let num: String = inner.chars().take_while(|c| c.is_ascii_digit()).collect();
    let index = num.parse::<usize>().ok()?;
    (index > 0).then_some(index)
}

pub(in crate::agent) fn vision_analysis_progress(tick: usize) -> String {
    let dots = match tick % 3 {
        1 => ".",
        2 => "..",
        _ => "...",
    };
    if crate::i18n::is_zh() {
        format!("视觉分析{dots}")
    } else {
        format!("Vision analysis{dots}")
    }
}

pub(in crate::agent) fn active_text_pool_supports_vision(config: &AppConfig) -> bool {
    let choices = config.active_provider_model_choices();
    !choices.is_empty()
        && choices.iter().all(|choice| {
            config.model_supports_any_input(&choice.provider_id, &choice.model, &["image"])
        })
}

pub(in crate::agent) fn should_use_active_text_pool_for_images(config: &AppConfig) -> bool {
    config.plugins.vision.prefer_current_multimodal_model
        && active_text_pool_supports_vision(config)
}
