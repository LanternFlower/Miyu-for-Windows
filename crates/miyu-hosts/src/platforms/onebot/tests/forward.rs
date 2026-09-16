//! 合并转发的解析与递归展开(08-26)。
//!
//! 用户反馈:Miyu 看不见别人转发进来的消息,尤其是转发里还套转发的。根因是
//! 入站段分派表里根本没有 `forward` 分支,整条消息落进 `_ => {}`。

use super::shared::*;
use crate::platforms::onebot::forward::{
    expand_forwards, expand_quoted_forwards, MAX_FORWARD_DEPTH,
};
use crate::platforms::onebot::*;

/// 两条解析路径(段数组 / CQ 码)都要认出转发 id,否则后面无从展开。
#[test]
fn both_parsers_capture_forward_ids() {
    let array = json!([
        { "type": "text", "data": { "text": "看这个" } },
        { "type": "forward", "data": { "id": "res-1" } },
    ]);
    let parsed = parse_message(Some(&array), None, 10001);
    assert_eq!(parsed.forward_ids, vec!["res-1"]);
    assert_eq!(parsed.text, "看这个");

    let cq = parse_message(None, Some(&json!("看这个[CQ:forward,id=res-2]")), 10001);
    assert_eq!(cq.forward_ids, vec!["res-2"]);
}

/// 节点里的 `res_id` 写法也要认——实现之间字段名不统一。
#[test]
fn forward_ids_accept_the_res_id_spelling() {
    let array = json!([{ "type": "forward", "data": { "res_id": "res-3" } }]);
    assert_eq!(
        parse_message(Some(&array), None, 10001).forward_ids,
        vec!["res-3"]
    );
}

fn forward_reply(echo: &Value, nodes: Value) -> Value {
    json!({ "status": "ok", "retcode": 0, "data": { "messages": nodes }, "echo": echo })
}

/// 主线:转发展开成"发送者: 内容",嵌套的一层跟着展开并缩进,图片并入现有
/// 入站图片链路。
#[tokio::test]
async fn expands_nested_forwards_and_collects_images() {
    let (handle, mut frames) = test_connection(None);
    let mut parsed = parse_message(
        Some(&json!([{ "type": "forward", "data": { "id": "outer" } }])),
        None,
        10001,
    );
    let task = {
        let handle = handle.clone();
        tokio::spawn(async move {
            let nodes = expand_forwards(&handle, &mut parsed).await.unwrap();
            (nodes, parsed)
        })
    };

    let outer: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(outer["action"], "get_forward_msg");
    assert_eq!(outer["params"]["id"], "outer");
    route_api_response(
        &handle,
        forward_reply(
            &outer["echo"],
            json!([
                {
                    "sender": { "nickname": "阿", "card": "" },
                    "message": [{ "type": "text", "data": { "text": "外层一句" } }]
                },
                {
                    "sender": { "nickname": "布", "card": "布布" },
                    "message": [
                        { "type": "image", "data": { "file": "p.jpg", "url": "https://img.example/p.jpg" } },
                        { "type": "forward", "data": { "id": "inner" } }
                    ]
                },
            ]),
        ),
    );

    let inner: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(inner["params"]["id"], "inner");
    route_api_response(
        &handle,
        forward_reply(
            &inner["echo"],
            json!([{
                "sender": { "nickname": "策" },
                "message": [{ "type": "text", "data": { "text": "里层一句" } }]
            }]),
        ),
    );

    let (nodes, parsed) = task.await.unwrap();
    assert_eq!(nodes, 3);
    assert!(parsed.text.contains("<qq-forward>"));
    // card 优先于 nickname;没有 card 时回落 nickname。
    assert!(parsed.text.contains("阿: 外层一句"));
    assert!(parsed.text.contains("布布: [image][nested forward]"));
    // 内层缩进,读起来能分清层级。
    assert!(parsed.text.contains("  策: 里层一句"));
    // 转发里的图进了正常图片链路,多模态模型能直接看见。
    assert_eq!(parsed.images.len(), 1);
    assert!(matches!(&parsed.images[0], MediaRef::Url(url) if url == "https://img.example/p.jpg"));
    // 展开完 id 要清掉,免得后续环节重复拉取。
    assert!(parsed.forward_ids.is_empty());
    assert_eq!(parsed.text_chars, parsed.text.chars().count());
}

/// 深度闸:套到第 4 层就不再请求了。用户 08-26 拍板追 3 层。
#[tokio::test]
async fn nesting_stops_at_the_configured_depth() {
    assert_eq!(MAX_FORWARD_DEPTH, 3);
    let (handle, mut frames) = test_connection(None);
    let mut parsed = parse_message(
        Some(&json!([{ "type": "forward", "data": { "id": "d1" } }])),
        None,
        10001,
    );
    let task = {
        let handle = handle.clone();
        tokio::spawn(async move {
            let nodes = expand_forwards(&handle, &mut parsed).await.unwrap();
            (nodes, parsed)
        })
    };

    // 每一层都回一个"再套一层"的节点;第 4 层的请求不该出现。
    let mut requested = Vec::new();
    for depth in 1..=MAX_FORWARD_DEPTH {
        let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
        requested.push(
            frame["params"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
        route_api_response(
            &handle,
            forward_reply(
                &frame["echo"],
                json!([{
                    "sender": { "nickname": format!("第{depth}层") },
                    "message": [{ "type": "forward", "data": { "id": format!("d{}", depth + 1) } }]
                }]),
            ),
        );
    }
    let (nodes, parsed) = task.await.unwrap();
    assert_eq!(requested, vec!["d1", "d2", "d3"]);
    assert_eq!(nodes, MAX_FORWARD_DEPTH);
    assert!(parsed.text.contains("(forwarded content truncated)"));
    assert!(frames.try_recv().is_err(), "第 4 层不该再发请求");
}

/// 取不到内容(转发过期、实现不支持这个 API)只是少了这一块,不能让整条消息
/// 连带失败。
#[tokio::test]
async fn a_failed_lookup_leaves_the_rest_of_the_message_intact() {
    let (handle, mut frames) = test_connection(None);
    let mut parsed = parse_message(
        Some(&json!([
            { "type": "text", "data": { "text": "原话还在" } },
            { "type": "forward", "data": { "id": "gone" } },
        ])),
        None,
        10001,
    );
    let task = {
        let handle = handle.clone();
        tokio::spawn(async move {
            let nodes = expand_forwards(&handle, &mut parsed).await.unwrap();
            (nodes, parsed)
        })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    route_api_response(
        &handle,
        json!({ "status": "failed", "retcode": 200, "echo": frame["echo"] }),
    );
    let (nodes, parsed) = task.await.unwrap();
    assert_eq!(nodes, 0);
    assert_eq!(parsed.text, "原话还在");
    assert!(!parsed.text.contains("<qq-forward>"));
}

/// 被引用消息里的转发也要展开(08-26 实测:用户引用一条转发问"里面是什么",
/// Miyu 答"那条在我这儿是空的")。图片并入**当前消息**的图片位。
#[tokio::test]
async fn quoted_forwards_expand_into_the_quoted_text() {
    let (handle, mut frames) = test_connection(None);
    // 当前消息是纯提问,转发在被引用的那条里。
    let mut current = parse_message(
        Some(&json!([{ "type": "text", "data": { "text": "这条里面是什么" } }])),
        None,
        10001,
    );
    let mut quoted = parse_message(
        Some(&json!([{ "type": "forward", "data": { "id": "quoted-res" } }])),
        None,
        10001,
    );
    assert_eq!(quoted.forward_ids, vec!["quoted-res"]);
    let task = {
        let handle = handle.clone();
        tokio::spawn(async move {
            let text = expand_quoted_forwards(&handle, &mut quoted, &mut current).await;
            (text, current)
        })
    };

    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "get_forward_msg");
    assert_eq!(frame["params"]["id"], "quoted-res");
    route_api_response(
        &handle,
        forward_reply(
            &frame["echo"],
            json!([
                {
                    "sender": { "nickname": "甲" },
                    "message": [{ "type": "text", "data": { "text": "转发里的一句" } }]
                },
                {
                    "sender": { "nickname": "乙" },
                    "message": [{ "type": "image", "data": { "file": "q.jpg", "url": "https://img.example/q.jpg" } }]
                },
            ]),
        ),
    );

    let (text, current) = task.await.unwrap();
    let text = text.expect("被引用的转发应当展开");
    assert!(text.contains("甲: 转发里的一句"));
    assert!(text.contains("<qq-forward>"));
    // 当前消息的正文不受影响,图片却并了进来。
    assert_eq!(current.text, "这条里面是什么");
    assert_eq!(current.images.len(), 1);
    assert!(matches!(&current.images[0], MediaRef::Url(url) if url == "https://img.example/q.jpg"));
}

/// 被引用消息里没有转发时不发请求、不改正文。
#[tokio::test]
async fn quoted_messages_without_forwards_are_left_alone() {
    let (handle, mut frames) = test_connection(None);
    let mut current = parse_message(Some(&json!([])), None, 10001);
    let mut quoted = parse_message(
        Some(&json!([{ "type": "text", "data": { "text": "普通引用" } }])),
        None,
        10001,
    );
    assert!(expand_quoted_forwards(&handle, &mut quoted, &mut current)
        .await
        .is_none());
    assert!(frames.try_recv().is_err(), "没有转发就不该发请求");
}
