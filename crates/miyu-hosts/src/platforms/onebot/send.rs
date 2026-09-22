//! 出站：分段发送、转发消息、文件上传。
//!
//! `send_segments` 要处理「一条回复变成多个 OneBot 段」的映射，以及发送失败时
//! 的部分成功（见 [`super::outbound`] 的 `partial_send_error`）。
//!
//! 上传走两条路（`upload_file` / `upload_file_source`）：本地文件直接传字节，
//! 远端 URL 交给 OneBot 端自己去拉，省一次往返。

use crate::platforms::onebot::*;

/// 刚发出去那条消息，引用还在不在——回头问对端一句。
///
/// 只记日志，不影响任何投递。问不到（对端不认 `get_msg`、超时、消息还没落库）
/// 就什么都不说：这一行是用来抓"发出去了却没有引用"的，不是用来报错的。
///
/// 先等一下再问：消息刚发完，对端那边往往还没把它写进自己的消息库。
async fn verify_quote_survived(
    connection: ConnectionHandle,
    conversation_id: String,
    reply_id: String,
    message_id: String,
    seq: Option<i64>,
) {
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let Ok(data) = connection
        .call_api_with_timeout(
            "get_msg",
            json!({ "message_id": message_id }),
            std::time::Duration::from_secs(5),
        )
        .await
    else {
        return;
    };
    let Some(segments) = data.get("message").and_then(Value::as_array) else {
        return;
    };
    let survived = segments
        .iter()
        .any(|segment| segment.get("type").and_then(Value::as_str) == Some("reply"));
    if survived {
        // 存活也记一行:只在失败时说话的话,日志里的"安静"是二义的——可能真的
        // 好好的,也可能这次自检压根没跑成(对端不认 get_msg、超时)。
        tracing::info!(
            target: "miyu::qq",
            %conversation_id, %reply_id, %message_id, ?seq,
            "{}",
            t("the reply segment survived on the platform", "引用段在对端存活")
        );
        return;
    }
    // 顺手问一句:被引用的那条,对端自己还找得到吗。找不到 = 它根本没法建引用
    // (这也是它唯一会静默丢弃的理由);找得到却还是丢了,那就是另一回事。
    // 只在失败时问,不给正常路径加任何往返。
    let reachable = |id: String| {
        let connection = connection.clone();
        async move {
            connection
                .call_api_with_timeout(
                    "get_msg",
                    json!({ "message_id": id }),
                    std::time::Duration::from_secs(5),
                )
                .await
                .is_ok()
        }
    };
    // 只给序号那条路没有号可查(reply_id 是 "null"),就别去问了——问出来的
    // "查不到"会把人往错的方向带。
    let has_id = reply_id.trim().parse::<i64>().is_ok();
    let quoted_reachable = match has_id {
        true => Some(reachable(reply_id.clone()).await),
        false => None,
    };
    // 负数号再按**无符号**问一遍:对端的号是 32 位的,它发给我们时是负数、
    // 自己存的时候却可能是无符号——真是这样的话换个写法它就找得到,那就是
    // 现成的修法。只在负数且查不到时多问这一次。
    let unsigned_reachable = match (quoted_reachable, reply_id.parse::<i64>()) {
        (Some(false), Ok(id)) if (-2_147_483_648..0).contains(&id) => {
            Some(reachable((id + 4_294_967_296).to_string()).await)
        }
        _ => None,
    };
    tracing::warn!(
        target: "miyu::qq",
        %conversation_id,
        %reply_id,
        %message_id,
        ?seq,
        // 被引用的那条消息,对端自己查不查得到。None = 没问(这条走的是
        // 只给序号那条路,压根没有号)
        ?quoted_reachable,
        // Some(true) = 同一个号按无符号写就查得到了(修法就在这儿)
        ?unsigned_reachable,
        "{}",
        t(
            "the platform dropped the reply segment; that message went out without a quote",
            "对端把引用段丢掉了,这条消息是没带引用发出去的",
        )
    );
}

impl OneBotAdapter {
    pub(in crate::platforms::onebot) async fn send_message(
        &self,
        message: OutboundMessage,
    ) -> Result<SendReceipt> {
        let response_target = message.response_target;
        match message.body {
            OutboundBody::Segments(segments) => {
                self.send_segments(segments, response_target.as_ref()).await
            }
            OutboundBody::Forward(nodes) => {
                let mut receipt = self.send_forward(nodes).await?;
                if let Some(target) = response_target.filter(ResponseTarget::is_effective) {
                    match self.send_response_marker(&target).await {
                        Ok(message_id) => {
                            receipt.delivered_parts += 1;
                            receipt.response_target_delivered = true;
                            if let Some(message_id) = message_id {
                                receipt.message_ids.push(message_id);
                            }
                        }
                        Err(error) => return Err(partial_send_error(error, receipt)),
                    }
                }
                Ok(receipt)
            }
        }
    }

    pub(in crate::platforms::onebot) async fn send_response_marker(
        &self,
        target: &ResponseTarget,
    ) -> Result<Option<String>> {
        if !matches!(self.target, Target::Group { .. }) || !target.is_effective() {
            return Ok(None);
        }
        let mut segments = vec![text_segment("\u{200b}")];
        prepend_response_target(&mut segments, target);
        let data = self.send_message_segments(segments).await?;
        Ok(data.get("message_id").and_then(value_id_string))
    }

    pub(in crate::platforms::onebot) async fn send_segments(
        &self,
        segments: Vec<OutboundSegment>,
        response_target: Option<&ResponseTarget>,
    ) -> Result<SendReceipt> {
        let mut frames = Vec::new();
        let mut current = Vec::new();
        let mut current_image_digests = Vec::new();
        let mut files = Vec::new();
        for segment in segments {
            match segment {
                OutboundSegment::Markdown(text) => {
                    append_text_chunks(
                        &mut frames,
                        &mut current,
                        &mut current_image_digests,
                        &markdown_to_plain(&text),
                        self.max_reply_chars,
                    );
                }
                OutboundSegment::Text(text) => append_text_chunks(
                    &mut frames,
                    &mut current,
                    &mut current_image_digests,
                    &text,
                    self.max_reply_chars,
                ),
                OutboundSegment::Mention(user_id) => current.push(json!({
                    "type": "at",
                    "data": { "qq": user_id },
                })),
                OutboundSegment::ImageBytes { data, .. } => {
                    if data.len() > MAX_OUTBOUND_IMAGE_BYTES {
                        bail!("outbound image exceeds the 20 MiB limit");
                    }
                    current_image_digests.push(blake3::hash(&data));
                    current.push(image_segment(&data));
                }
                OutboundSegment::ImagePath { path, .. } => {
                    let bytes = read_file_capped(&path, MAX_OUTBOUND_IMAGE_BYTES).await?;
                    // Decode dimensions before giving untrusted/generated bytes
                    // to the adapter, matching WebUI image safety expectations.
                    let bytes = validate_outbound_image(bytes, path).await?;
                    current_image_digests.push(blake3::hash(&bytes));
                    current.push(image_segment(&bytes));
                }
                OutboundSegment::FilePath { path, name } => {
                    push_message_frame(&mut frames, &mut current, &mut current_image_digests);
                    files.push((path, name));
                }
                OutboundSegment::AudioPath { path, .. } => {
                    // QQ 语音消息不能和文字/图片混在一条里:前后各切一帧。
                    push_message_frame(&mut frames, &mut current, &mut current_image_digests);
                    let bytes = read_file_capped(&path, MAX_OUTBOUND_IMAGE_BYTES).await?;
                    current.push(record_segment(&bytes));
                    push_message_frame(&mut frames, &mut current, &mut current_image_digests);
                }
            }
        }
        push_message_frame(&mut frames, &mut current, &mut current_image_digests);

        let has_message_frames = !frames.is_empty();
        // 引用/@ 挂在第一条**非语音**帧上:QQ 语音消息(record 段)必须独占一条,
        // 和 reply/at 段同在一条里时消息能发出去,但别人点不动播放(09-05 用户
        // 报)。全是语音就不带引用。
        let target_frame = if matches!(self.target, Target::Group { .. })
            && response_target.is_some_and(ResponseTarget::is_effective)
        {
            frames
                .iter()
                .position(|frame| !frame_is_voice(&frame.segments))
        } else {
            None
        };
        let mut receipt = SendReceipt::default();
        for (index, frame) in frames.into_iter().enumerate() {
            let MessageFrame {
                mut segments,
                image_digests,
            } = frame;
            let has_image = !image_digests.is_empty();
            let carries_target = target_frame == Some(index);
            if carries_target {
                prepend_response_target(
                    &mut segments,
                    response_target.expect("effective response target exists"),
                );
            }
            let data = match self.send_message_segments(segments).await {
                Ok(data) => data,
                Err(error) => return Err(partial_send_error(error, receipt)),
            };
            receipt.delivered_parts += 1;
            if carries_target {
                receipt.response_target_delivered = true;
            }
            receipt.image_digests.extend(image_digests);
            if let Some(id) = data.get("message_id").and_then(value_id_string) {
                if has_image {
                    receipt.image_message_ids.push(id.clone());
                }
                receipt.message_ids.push(id);
            }
        }
        for (path, name) in files {
            let id = match self.upload_file(&path, name.as_deref()).await {
                Ok(id) => id,
                Err(error) => return Err(partial_send_error(error, receipt)),
            };
            receipt.delivered_parts += 1;
            if let Some(id) = id {
                receipt.message_ids.push(id);
            }
        }
        if !has_message_frames {
            if let Some(target) = response_target.filter(|target| target.is_effective()) {
                let message_id = match self.send_response_marker(target).await {
                    Ok(message_id) => message_id,
                    Err(error) => return Err(partial_send_error(error, receipt)),
                };
                receipt.delivered_parts += 1;
                receipt.response_target_delivered = true;
                if let Some(message_id) = message_id {
                    receipt.message_ids.push(message_id);
                }
            }
        }
        Ok(receipt)
    }

    pub(in crate::platforms::onebot) async fn send_forward(
        &self,
        nodes: Vec<ForwardNode>,
    ) -> Result<SendReceipt> {
        if nodes.is_empty() {
            bail!("a forward message needs at least one node");
        }
        let mut messages = Vec::with_capacity(nodes.len());
        let mut image_digests = Vec::new();
        for node in nodes {
            let mut content = Vec::new();
            for segment in node.segments {
                match segment {
                    OutboundSegment::Markdown(text) => {
                        content.push(text_segment(&markdown_to_plain(&text)));
                    }
                    OutboundSegment::Text(text) => content.push(text_segment(&text)),
                    OutboundSegment::Mention(user_id) => content.push(json!({
                        "type": "at",
                        "data": { "qq": user_id },
                    })),
                    OutboundSegment::ImageBytes { data, .. } => {
                        if data.len() > MAX_OUTBOUND_IMAGE_BYTES {
                            bail!("outbound image exceeds the 20 MiB limit");
                        }
                        image_digests.push(blake3::hash(&data));
                        content.push(image_segment(&data));
                    }
                    OutboundSegment::ImagePath { path, .. } => {
                        let bytes = read_file_capped(&path, MAX_OUTBOUND_IMAGE_BYTES).await?;
                        let bytes = validate_outbound_image(bytes, path).await?;
                        image_digests.push(blake3::hash(&bytes));
                        content.push(image_segment(&bytes));
                    }
                    OutboundSegment::FilePath { .. } => {
                        bail!("files cannot be embedded in a OneBot forward node")
                    }
                    OutboundSegment::AudioPath { .. } => {
                        bail!("voice messages cannot be embedded in a OneBot forward node")
                    }
                }
            }
            messages.push(json!({
                "type": "node",
                "data": {
                    "uin": node.user_id,
                    "name": node.display_name,
                    "content": content,
                }
            }));
        }
        let (action, params) = match self.target {
            Target::Private { user_id } => (
                "send_private_forward_msg",
                json!({ "user_id": user_id, "messages": messages }),
            ),
            Target::Group { group_id } => (
                "send_group_forward_msg",
                json!({ "group_id": group_id, "messages": messages }),
            ),
        };
        let data = self.connection().call_api(action, params).await?;
        Ok(SendReceipt {
            message_ids: data
                .get("message_id")
                .and_then(value_id_string)
                .into_iter()
                .collect(),
            image_message_ids: Vec::new(),
            delivered_parts: 1,
            image_digests,
            response_target_delivered: false,
        })
    }

    pub(in crate::platforms::onebot) async fn send_message_segments(
        &self,
        segments: Vec<Value>,
    ) -> Result<Value> {
        let timeout = send_timeout_for(&segments);
        // 带引用的出站留痕(08-26)。历史库能证明 Miyu 决定了引用,但发到对端的
        // 到底长什么样、对端认不认,原先整条链路没有任何 payload 级记录,查
        // "引用没渲染"时无从下手。只在含 reply 段时记一行,不记正文。
        // 触发条件是**有没有引用段**,不是"有没有 id":负数号那条路只给 seq 不
        // 给 id,按 id 判的话那一半恰好全从日志里消失——而那一半正是要盯的。
        let reply_data = segments
            .iter()
            .find(|segment| segment.get("type").and_then(Value::as_str) == Some("reply"))
            .and_then(|segment| segment.get("data").cloned());
        let quoted = reply_data.as_ref().and_then(|data| data.get("id").cloned());
        let seq = reply_data
            .as_ref()
            .and_then(|data| data.get("seq"))
            .and_then(Value::as_i64);
        // 段类型要在 segments 被移进 params 之前取好。
        let kinds = quoted.is_some().then(|| {
            segments
                .iter()
                .map(|segment| {
                    segment
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("+")
        });
        let (action, params) = match self.target {
            Target::Private { user_id } => (
                "send_private_msg",
                json!({ "user_id": user_id, "message": segments }),
            ),
            Target::Group { group_id } => (
                "send_group_msg",
                json!({ "group_id": group_id, "message": segments }),
            ),
        };
        let result = self
            .connection()
            .call_api_with_timeout(action, params, timeout)
            .await;
        if reply_data.is_some() {
            let quoted = quoted.unwrap_or(Value::Null);
            let sent_message_id = result
                .as_ref()
                .ok()
                .and_then(|data| data.get("message_id").and_then(value_id_string));
            tracing::info!(
                target: "miyu::qq",
                conversation_id = self.target.conversation_id(),
                reply_id = %quoted,
                reply_seq = ?seq,
                segments = kinds.unwrap_or_default(),
                sent_message_id = ?sent_message_id,
                error = ?result.as_ref().err().map(ToString::to_string),
                "{}",
                t(
                    "OneBot outbound carried a reply segment",
                    "OneBot 出站消息携带引用段"
                )
            );
            // 对端收下了不等于引用还在:NapCat 解析不出引用段时是**静默丢掉、
            // 消息照发**的(短号反查不到就 return),我们这头拿到的照样是"发送
            // 成功"。所以回头问它一句这条消息现在长什么样——这是"她为什么没
            // 引用"唯一能自证的地方(用户 09-19)。
            //
            // 丢进后台:这只是观测,绝不能挡住后面几帧的投递(拆成多条发时,
            // 等在这儿就是每条之间多几百毫秒)。
            if let Some(message_id) = sent_message_id {
                let connection = self.connection();
                let conversation_id = self.target.conversation_id().to_string();
                // 取**字符串内容**,不是 JSON 写法:`Value::to_string` 会把
                // 字符串连引号一起给出来("-46382822"),拿去 parse 成数字必然
                // 失败,探针就永远不会跑(09-19 自己踩过一次)。
                let reply_id = match &quoted {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                tokio::spawn(async move {
                    verify_quote_survived(connection, conversation_id, reply_id, message_id, seq)
                        .await;
                });
            }
        }
        result
    }

    pub(in crate::platforms::onebot) async fn upload_file(
        &self,
        path: &std::path::Path,
        name: Option<&str>,
    ) -> Result<Option<String>> {
        let metadata = tokio::fs::metadata(path)
            .await
            .with_context(|| format!("reading outbound file metadata: {}", path.display()))?;
        if !metadata.is_file() {
            bail!(
                "outbound attachment is not a regular file: {}",
                path.display()
            );
        }
        if metadata.len() > MAX_OUTBOUND_FILE_BYTES as u64 {
            bail!("outbound attachment exceeds the 50 MiB limit");
        }
        let name = name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .or_else(|| path.file_name().and_then(|name| name.to_str()))
            .unwrap_or("file");
        let name = sanitize_file_name(name);
        let conn = self.connection();
        if let Some(base_url) = conn.asset_base_url.as_deref() {
            let lease = conn.assets.create(base_url, path, &name).await?;
            match self.upload_file_source(&lease.url, &name).await {
                Ok(id) => return Ok(id),
                Err(error) => tracing::warn!(
                    error = %error,
                    "{}",
                    t("NapCat could not fetch streamed file; considering base64 fallback", "NapCat 无法获取流式文件，尝试使用 base64 回退")
                ),
            }
        }
        if metadata.len() > MAX_BASE64_FILE_BYTES as u64 {
            bail!(
                "NapCat could not fetch the temporary file URL and the file exceeds the 16 MiB base64 fallback limit"
            );
        }
        let bytes = read_file_capped(path, MAX_BASE64_FILE_BYTES).await?;
        self.upload_file_source(&format!("base64://{}", BASE64.encode(bytes)), &name)
            .await
    }

    pub(in crate::platforms::onebot) async fn upload_file_source(
        &self,
        source: &str,
        name: &str,
    ) -> Result<Option<String>> {
        let (action, params) = match self.target {
            Target::Private { user_id } => (
                "upload_private_file",
                json!({ "user_id": user_id, "file": source, "name": name }),
            ),
            Target::Group { group_id } => (
                "upload_group_file",
                json!({ "group_id": group_id, "file": source, "name": name }),
            ),
        };
        // connection() 取 registry 里的现任连接:NapCat 重连换代后,构造期
        // 快照 self.conn 的 writer 已关闭,直接用它上传必报 writer closed。
        let data = self
            .connection()
            .call_api_with_timeout(action, params, FILE_DOWNLOAD_TIMEOUT)
            .await?;
        Ok(data.get("file_id").and_then(value_id_string))
    }
}

/// 这一帧是不是语音消息(record 段)。语音必须独占一条 QQ 消息。
fn frame_is_voice(segments: &[Value]) -> bool {
    segments
        .iter()
        .any(|segment| segment.get("type").and_then(Value::as_str) == Some("record"))
}
