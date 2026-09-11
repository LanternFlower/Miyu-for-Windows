//! 合并转发的展开(08-26)。
//!
//! 合并转发在入站消息里只是一个 `forward` 段,里面除了一个资源 id 什么都没
//! 有。入站解析器原先没有这个分支,整条消息落进 `_ => {}`,Miyu 收到的是一条
//! 空消息——用户看到的现象就是"她看不见别人转发进来的东西"。
//!
//! 内容要拿 id 去调 `get_forward_msg`,返回一组节点(每个节点有发送者名和它
//! 自己的消息段)。节点里可以再套合并转发,所以展开是递归的,靠三道闸收住:
//! 深度、节点总数、正文字节数。三道都是硬上限,套娃再深也炸不了上下文。
//!
//! 展开结果拼成 `<qq-forward>` 包裹的文本块挂在正文后面。图片按现有的入站
//! 图片链路收集(同一套 `MAX_INBOUND_IMAGES` 与总字节上限),多模态模型因此
//! 能直接看见转发里的图。其余媒体只留占位文字,不下载。

use crate::platforms::onebot::inbound::{
    bounded_chars, onebot_id_value, push_inbound_image_source, InboundMessage, MAX_INBOUND_IMAGES,
    MAX_INBOUND_TEXT_CHARS,
};
use crate::platforms::onebot::ConnectionHandle;
use anyhow::Result;
use serde_json::{json, Value};
use std::time::Duration;

/// 转发套转发最多追几层。用户 08-26 拍板:3。
pub(in crate::platforms::onebot) const MAX_FORWARD_DEPTH: usize = 3;
/// 一次展开最多收多少个节点,跨所有层级共享。
pub(in crate::platforms::onebot) const MAX_FORWARD_NODES: usize = 60;
/// 展开文本的字节上限。超了就截断并留一行说明。
pub(in crate::platforms::onebot) const MAX_FORWARD_TEXT_BYTES: usize = 4000;
/// 单个节点正文的字符上限,防止一条超长消息吃光整个预算。
const MAX_FORWARD_NODE_CHARS: usize = 400;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(20);

/// 展开这条消息里的全部合并转发,把结果追加到 `parsed.text`,图片并入
/// `parsed.images`。返回收进来的节点数。
pub(in crate::platforms::onebot) async fn expand_forwards(
    conn: &ConnectionHandle,
    parsed: &mut InboundMessage,
) -> Result<usize> {
    let forward_ids = std::mem::take(&mut parsed.forward_ids);
    let Some((block, nodes)) = collect_forwards(conn, forward_ids, parsed).await else {
        return Ok(0);
    };
    // 走剩余预算,别绕过入站正文上限:直接 push_str 会让一条本已顶格的消息
    // 再多出千把字,而那个上限是下游按 20,000 字设计的。
    let remaining = MAX_INBOUND_TEXT_CHARS.saturating_sub(parsed.text_chars);
    parsed.text.extend(block.chars().take(remaining));
    parsed.text_chars = parsed.text.chars().count();
    Ok(nodes)
}

/// 展开**被引用消息**里的合并转发,返回该展开给被引用消息的完整正文。
///
/// 用户问"这条转发里是什么"最自然的姿势就是引用它再问(08-26 实测)。这条路
/// 与当前消息那条是分开的:引用的内容走 `replied_message`,由 real_context 渲
/// 染成 `quoted earlier message …`,而不是当前消息的正文。
///
/// 图片并入**当前消息**的图片位——被引用消息本身不进模型的图片通道,而那些图
/// 正是问题的一部分。名额与字节上限沿用同一套。
pub(in crate::platforms::onebot) async fn expand_quoted_forwards(
    conn: &ConnectionHandle,
    quoted: &mut InboundMessage,
    image_sink: &mut InboundMessage,
) -> Option<String> {
    let forward_ids = std::mem::take(&mut quoted.forward_ids);
    let (block, _) = collect_forwards(conn, forward_ids, image_sink).await?;
    let mut text = quoted.text.clone();
    text.push_str(&block);
    Some(text)
}

/// 展开一组转发资源。文本作为返回值,图片并入 `image_sink`。
/// 返回 `None` = 没有可展开的内容。
async fn collect_forwards(
    conn: &ConnectionHandle,
    forward_ids: Vec<String>,
    image_sink: &mut InboundMessage,
) -> Option<(String, usize)> {
    if forward_ids.is_empty() {
        return None;
    }
    let mut collector = ForwardCollector::default();
    for id in forward_ids {
        if collector.exhausted() {
            break;
        }
        collector.expand(conn, &id, 1, image_sink).await;
    }
    if collector.nodes == 0 {
        return None;
    }
    let mut block = String::from("\n<qq-forward>\n");
    block.push_str(&collector.text);
    if collector.truncated {
        block.push_str("\n(forwarded content truncated)\n");
    }
    block.push_str("</qq-forward>");
    Some((block, collector.nodes))
}

#[derive(Default)]
struct ForwardCollector {
    text: String,
    nodes: usize,
    truncated: bool,
}

impl ForwardCollector {
    fn exhausted(&self) -> bool {
        self.nodes >= MAX_FORWARD_NODES || self.text.len() >= MAX_FORWARD_TEXT_BYTES
    }

    /// 递归展开一个转发资源。取不到就静默跳过——转发过期、被撤回、实现不支持
    /// 这个 API 都会失败,为此丢掉整条消息不划算。
    async fn expand(
        &mut self,
        conn: &ConnectionHandle,
        forward_id: &str,
        depth: usize,
        parsed: &mut InboundMessage,
    ) {
        if depth > MAX_FORWARD_DEPTH {
            self.truncated = true;
            return;
        }
        let Ok(data) = fetch_forward(conn, forward_id).await else {
            return;
        };
        let Some(messages) = forward_nodes(&data) else {
            return;
        };
        for node in messages {
            if self.exhausted() {
                self.truncated = true;
                return;
            }
            self.push_node(conn, node, depth, parsed).await;
        }
    }

    async fn push_node(
        &mut self,
        conn: &ConnectionHandle,
        node: &Value,
        depth: usize,
        parsed: &mut InboundMessage,
    ) {
        let sender = node_sender_name(node);
        let segments = node_segments(node);
        let mut body = String::new();
        let mut nested = Vec::new();
        for segment in segments {
            let kind = segment.get("type").and_then(Value::as_str).unwrap_or("");
            let data = segment.get("data").unwrap_or(&Value::Null);
            match kind {
                "text" => {
                    if let Some(text) = data.get("text").and_then(Value::as_str) {
                        body.push_str(text);
                    }
                }
                "image" => {
                    body.push_str("[image]");
                    if parsed.images.len() < MAX_INBOUND_IMAGES {
                        let file = data.get("file").and_then(Value::as_str).unwrap_or("");
                        push_inbound_image_source(
                            parsed,
                            file,
                            data.get("url").and_then(Value::as_str),
                        );
                    }
                }
                "at" => body.push_str("[@]"),
                "face" => body.push_str("[emoji]"),
                "record" => body.push_str("[voice]"),
                "video" => body.push_str("[video]"),
                "file" => body.push_str("[file]"),
                "forward" => {
                    // 内层转发的 id 在同一个字段位置上。先记下,等这个节点的
                    // 文字落定再展开,读起来才是"某人转发了 X,X 的内容如下"。
                    if let Some(id) = data
                        .get("id")
                        .or_else(|| data.get("res_id"))
                        .and_then(|id| match id {
                            Value::String(id) => Some(id.clone()),
                            Value::Number(id) => Some(id.to_string()),
                            _ => None,
                        })
                    {
                        body.push_str("[nested forward]");
                        nested.push(id);
                    }
                }
                _ => {}
            }
        }
        let body = bounded_chars(body.trim(), MAX_FORWARD_NODE_CHARS);
        if body.is_empty() && nested.is_empty() {
            return;
        }
        self.nodes += 1;
        let indent = "  ".repeat(depth.saturating_sub(1));
        self.text.push_str(&format!("{indent}{sender}: {body}\n"));
        if self.text.len() > MAX_FORWARD_TEXT_BYTES {
            self.truncated = true;
            self.text
                .truncate(floor_char_boundary(&self.text, MAX_FORWARD_TEXT_BYTES));
            return;
        }
        for id in nested {
            if self.exhausted() {
                self.truncated = true;
                return;
            }
            Box::pin(self.expand(conn, &id, depth + 1, parsed)).await;
        }
    }
}

async fn fetch_forward(conn: &ConnectionHandle, forward_id: &str) -> Result<Value> {
    conn.call_api_with_timeout(
        "get_forward_msg",
        // 实现之间参数名不统一:OneBot 11 是 id,部分实现认 message_id。
        // id 走 onebot_id_value:纯数字的 id 要发成数字,与全仓其余 OneBot
        // 调用一致——字符串化的数字 id 正是引用段那类"发出去了却不生效"的
        // 温床(08-26)。资源 id 形态的字符串原样透传。
        json!({
            "id": onebot_id_value(forward_id),
            "message_id": onebot_id_value(forward_id),
        }),
        FORWARD_TIMEOUT,
    )
    .await
}

/// 节点数组在不同实现里挂的位置不一样,几处都试一遍。
fn forward_nodes(data: &Value) -> Option<&Vec<Value>> {
    data.get("messages")
        .or_else(|| data.get("message"))
        .or_else(|| data.get("nodes"))
        .and_then(Value::as_array)
}

fn node_sender_name(node: &Value) -> String {
    let raw = node
        .get("sender")
        .and_then(|sender| {
            sender
                .get("card")
                .and_then(Value::as_str)
                .filter(|card| !card.trim().is_empty())
                .or_else(|| sender.get("nickname").and_then(Value::as_str))
        })
        .or_else(|| node.get("nickname").and_then(Value::as_str))
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        "unknown".to_string()
    } else {
        bounded_chars(raw, 64)
    }
}

/// 节点自身的消息段。`message` 可能直接是数组,也可能裹在 `data.message` 里。
fn node_segments(node: &Value) -> &[Value] {
    node.get("message")
        .or_else(|| node.get("data").and_then(|data| data.get("message")))
        .or_else(|| node.get("content"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// `str::floor_char_boundary` 还没稳定,自己找一个不劈开字符的位置。
fn floor_char_boundary(text: &str, limit: usize) -> usize {
    if limit >= text.len() {
        return text.len();
    }
    let mut index = limit;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}
