//! 用户输入的预处理。
//!
//! `prepare_user_input` 把一条原始输入变成能进模型的消息：剥掉粘贴进来的系统
//! 提醒块（用户可能整段复制了上一轮的输出）、把图片占位符换成真实路径、把上传
//! 的附件转成 image part。
//!
//! 剥离系统提醒是安全边界而非清洁工作：不剥的话，用户粘贴一段伪造的
//! `<system-reminder>` 就等于往提示词里注入指令。

use crate::agent::*;

impl Agent {
    pub(in crate::agent) async fn prepare_user_input(
        &self,
        input: &str,
        images: &[Option<PastedImage>],
    ) -> Result<PreparedUserInput> {
        let input = clean_user_visible_text(input);
        let binary_images = images
            .iter()
            .filter_map(|image| match image {
                Some(PastedImage::Binary(image)) => Some(image),
                _ => None,
            })
            .collect::<Vec<_>>();
        let path_images = images
            .iter()
            .filter_map(|image| match image {
                Some(PastedImage::Path(path)) => Some(path.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let absolute_image_paths =
            resolve_pasted_image_paths(images, &self.paths, self.image_platform.as_deref());
        let binary_paths = images
            .iter()
            .zip(&absolute_image_paths)
            .filter_map(|(image, path)| {
                matches!(image, Some(PastedImage::Binary(_)))
                    .then(|| path.clone())
                    .flatten()
            })
            .collect::<Vec<_>>();
        // v7 Phase 1.3-b: register the scoped vision tool whenever the platform
        // path is active, even with no images this turn. A conditional
        // registration made the tools array appear/disappear between turns,
        // invalidating the provider prefix cache from token 0; an empty scope
        // simply rejects analysis requests with a clear message instead.
        //
        // 生图的参考图与看图共用同一份作用域,所以这一段不能只由 vision 插件
        // 开关把门:vision 关、生图开时,平台回合的 generate_image 会留着不受
        // 限的解析器,不可信用户一句话就能让它把宿主上任意文件当参考图上传。
        if self.tools_enabled
            && (self.config.plugins.vision.enabled || self.config.plugins.image_generation.enabled)
            && self.image_platform.is_some()
        {
            let mut tools = self.tools.lock().unwrap();
            if let Some(platform_context) = self.platform_context.clone() {
                vision::register_scoped_platform(
                    &mut tools,
                    self.config.clone(),
                    self.paths.clone(),
                    binary_paths.iter().map(PathBuf::from).collect(),
                    self.context_images.clone(),
                    platform_context,
                );
            } else if !tools.contains("vision_analyze") {
                vision::register_scoped_local(
                    &mut tools,
                    self.config.clone(),
                    self.paths.clone(),
                    binary_paths.iter().map(PathBuf::from).collect(),
                );
            }
        }
        let vision_tool_available =
            self.tools_enabled && self.tools.lock().unwrap().contains("vision_analyze");
        let input = rewrite_image_placeholders_with_paths(&input, &absolute_image_paths);
        let current_model_supports_vision = self.current_model_supports_vision();
        let content = if !binary_images.is_empty() && !current_model_supports_vision {
            self.describe_images_with_vision_provider(&input, &binary_images)
                .await?
        } else {
            input
        };

        let message = if !binary_images.is_empty() && current_model_supports_vision {
            let mut parts = vec![ChatContentPart::Text {
                text: content.clone(),
            }];
            parts.extend(binary_images.iter().map(|image| ChatContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: image.data_url().to_string(),
                },
            }));
            ChatMessage::user_parts(parts)
        } else {
            ChatMessage::plain("user", &content)
        };

        let mut hints = Vec::new();
        if !binary_paths.is_empty() {
            let source = self
                .image_platform_label
                .as_deref()
                .or(self.image_platform.as_deref())
                .map(|platform| format!("通过 {platform} 发送"))
                .unwrap_or_else(|| "粘贴".to_string());
            let tool_hint = if vision_tool_available {
                "\n你可以使用 vision_analyze 工具对此图片进行更详细的分析。"
            } else {
                ""
            };
            let hint = if binary_paths.len() == 1 {
                format!(
                    "用户{source}了 1 张图片，已保存到临时文件：{}{}",
                    binary_paths[0], tool_hint
                )
            } else {
                let list = binary_paths
                    .iter()
                    .enumerate()
                    .map(|(index, path)| format!("  [Image {}] {}", index + 1, path))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "用户{source}了 {} 张图片，已保存到临时文件：\n{}{}",
                    binary_paths.len(),
                    list,
                    if vision_tool_available {
                        "\n你可以使用 vision_analyze 工具对这些图片进行更详细的分析。"
                    } else {
                        ""
                    }
                )
            };
            hints.push(ChatMessage::turn_context(hint));
        }
        if !path_images.is_empty() && vision_tool_available {
            let list = path_images
                .iter()
                .enumerate()
                .map(|(index, path)| format!("  [Image {}] {}", index + 1, path))
                .collect::<Vec<_>>()
                .join("\n");
            hints.push(ChatMessage::turn_context(format!(
                "用户粘贴了 {} 张本地图片路径：\n{}\n你可以使用 vision_analyze 工具读取并分析这些图片。",
                path_images.len(),
                list
            )));
        }
        if !self.context_images.is_empty() && vision_tool_available {
            let ids = self
                .context_images
                .iter()
                .map(|image| image.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            // 用法说明是常量,已随 <qq-context-images> 进 system 提示词
            // (08-17;实测一条 780K token 的群聊请求里这句重复 579 次、
            // 共 139,867 字符)。每轮只留真正会变的 ID 列表。
            hints.push(ChatMessage::turn_context(format!(
                "<context-images>{ids}</context-images>"
            )));
        }

        Ok(PreparedUserInput {
            content,
            message,
            hints,
        })
    }

    pub(in crate::agent) async fn clipboard_image_message(
        &self,
        img: ClipboardImage,
    ) -> Result<Option<ChatMessage>> {
        if self.current_model_supports_vision() {
            return Ok(Some(ChatMessage::user_parts(vec![
                ChatContentPart::ImageUrl {
                    image_url: ImageUrlContent {
                        url: img.data_url().to_string(),
                    },
                },
            ])));
        }

        let images = vec![&img];
        let description = self
            .describe_images_with_vision_provider("", &images)
            .await?;
        if description.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(ChatMessage::plain("user", description)))
    }

    pub(in crate::agent) fn uploaded_attachment_image_parts(
        &self,
        attachments: &[crate::state::UserAttachment],
    ) -> Vec<ChatContentPart> {
        attachments
            .iter()
            .filter(|attachment| attachment.kind == "image")
            .filter_map(|attachment| {
                self.state
                    .load_user_attachment(&attachment.attachment_id)
                    .ok()
                    .flatten()
            })
            .map(|attachment| ChatContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: ClipboardImage::new(attachment.attachment.mime, attachment.bytes)
                        .data_url()
                        .to_string(),
                },
            })
            .collect()
    }

    pub(in crate::agent) fn queued_prompt_images(
        &self,
        prompt: &QueuedPrompt,
    ) -> Result<Vec<Option<PastedImage>>> {
        let mut images = queued_prompt_images(prompt)?;
        for attachment in &prompt.uploaded_attachments {
            if attachment.kind != "image" {
                continue;
            }
            if let Some(data) = self.state.load_user_attachment(&attachment.attachment_id)? {
                images.push(Some(PastedImage::Binary(ClipboardImage::new(
                    data.attachment.mime,
                    data.bytes,
                ))));
            }
        }
        Ok(images)
    }
}
