//! 文本处理：命令前缀与句号。

use crate::platforms::plugins::reply_processor::*;



#[test]
fn command_prefix_requires_a_boundary() {
    assert_eq!(reply_command("/回复处理 状态"), Some("状态"));
    assert_eq!(reply_command("回复处理"), Some(""));
    assert_eq!(reply_command("/回复处理器 状态"), None);
    assert_eq!(reply_command("普通消息"), None);
}

#[test]
fn strips_only_the_last_visible_chinese_period() {
    let mut message = OutboundMessage::segments(
        OutboundOrigin::FinalReply,
        vec![
            OutboundSegment::Text("第一段。".to_string()),
            OutboundSegment::ImageBytes {
                mime: "image/png".to_string(),
                data: Arc::from([1_u8, 2, 3]),
                alt: String::new(),
            },
            OutboundSegment::Markdown("最后一段。  \n".to_string()),
        ],
    );
    strip_trailing_chinese_period(&mut message);
    assert_eq!(message_text(&message), "第一段。\n最后一段  \n");

    let mut english = OutboundMessage::text(OutboundOrigin::FinalReply, "keep.");
    strip_trailing_chinese_period(&mut english);
    assert_eq!(message_text(&english), "keep.");
}

#[test]
fn text_replacement_keeps_non_text_segments_in_order() {
    let message = OutboundMessage::segments(
        OutboundOrigin::Tool,
        vec![
            OutboundSegment::Mention("1".to_string()),
            OutboundSegment::Text("long".to_string()),
            OutboundSegment::ImagePath {
                path: "a.png".into(),
                alt: "a".to_string(),
            },
            OutboundSegment::Markdown("more".to_string()),
            OutboundSegment::FilePath {
                path: "b.txt".into(),
                name: None,
            },
        ],
    );
    let replaced = replace_text_segments(
        message,
        vec![OutboundSegment::ImageBytes {
            mime: "image/png".to_string(),
            data: Arc::from([9_u8]),
            alt: "rendered".to_string(),
        }],
    );
    let OutboundBody::Segments(segments) = replaced.body else {
        panic!("expected segments");
    };
    assert!(matches!(segments[0], OutboundSegment::Mention(_)));
    assert!(matches!(segments[1], OutboundSegment::ImageBytes { .. }));
    assert!(matches!(segments[2], OutboundSegment::ImagePath { .. }));
    assert!(matches!(segments[3], OutboundSegment::FilePath { .. }));
}
