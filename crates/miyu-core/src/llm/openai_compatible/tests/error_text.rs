//! 报错写给**人**看的那一面（BUG-16）。
//!
//! 分类器把失败切成 7 档、算出冷却时长，可这些以前全喂给了调度器，一个字都没
//! 到过用户眼前：`Display` 只打 `upstream returned HTTP 429`，限流和别的状态码
//! 长得一模一样。这里钉住「打出来的那句话里有什么」。

use crate::llm::openai_compatible::chat::{all_endpoints_failed_message, clip_reason};
use crate::llm::openai_compatible::*;

/// 限流要说成限流，而不是一个光秃秃的状态码。
#[test]
fn a_rate_limit_says_it_is_a_rate_limit() {
    let failure = HttpStatusFailure::classify(429, "");
    let text = failure.to_string();
    assert!(text.contains("限流") || text.contains("额度"), "{text}");
    assert!(text.contains("429"), "真的 HTTP 失败要留状态码备查: {text}");
}

/// 中转线（codex / claude-code / agy）的状态码是按子进程输出的措辞编的，
/// 打给用户看的时候一个数字都不许提——不然人会照着网络问题去查 base_url。
#[test]
fn a_relay_failure_never_claims_an_http_status() {
    let failure = HttpStatusFailure::relay(429, HttpFailureKind::RateLimit);
    let text = failure.to_string();
    assert!(text.contains("额度"), "{text}");
    assert!(!text.contains("429"), "中转线没发过 HTTP 请求: {text}");
    assert!(!text.to_lowercase().contains("http"), "{text}");
}

/// 每一档都得有一句人话，别漏。
#[test]
fn every_failure_kind_has_a_human_label() {
    for kind in [
        HttpFailureKind::Status,
        HttpFailureKind::Authentication,
        HttpFailureKind::RateLimit,
        HttpFailureKind::EndpointUnavailable,
        HttpFailureKind::EndpointIncompatible,
        HttpFailureKind::InvalidRequest,
        HttpFailureKind::ContentPolicy,
    ] {
        for relay in [false, true] {
            assert!(!kind.label(relay).is_empty(), "{kind} 少一句人话");
        }
    }
}

/// 全池失败那一段：一句结论 + 每个端点一行。不再多一句「该怎么办」——
/// 端点行里已经说了「被限流」「暂停 10 分钟」，那句是同义反复（用户 09-20）。
#[test]
fn the_all_failed_message_lists_every_endpoint_and_nothing_else() {
    let lines = vec![
        format!(
            "opencodego / deepseek-v4.1-flash（key#1）：{}；该端点暂停 10 分钟",
            HttpFailureKind::RateLimit.label(false)
        ),
        format!(
            "ririxin / glm-5.2（key#1）：{}；该端点暂停 10 分钟",
            HttpFailureKind::RateLimit.label(false)
        ),
    ];
    let message = all_endpoints_failed_message(&lines, "llm_1789_42");
    assert!(message.starts_with("所有模型端点都没跑通"), "{message}");
    assert!(
        message.contains("llm_1789_42"),
        "请求 id 要留着备查: {message}"
    );
    assert!(
        message.contains("opencodego") && message.contains("ririxin"),
        "{message}"
    );
    assert!(message.contains("暂停 10 分钟"), "冷却时长要说: {message}");
    assert!(
        !message.contains("→"),
        "不再给「该怎么办」那一行: {message}"
    );
    assert_eq!(message.lines().count(), 3, "结论 + 两条端点，没有第四行");
}

/// 任何组合都不再有建议行。原来只在「所有端点栽在同一件事上」时才给，
/// 09-20 起一概不给。
#[test]
fn no_combination_of_failures_adds_an_advice_line() {
    let lines = vec![
        format!(
            "a / m（key#1）：{}",
            HttpFailureKind::RateLimit.label(false)
        ),
        format!(
            "b / m（key#1）：{}",
            HttpFailureKind::Authentication.label(false)
        ),
    ];
    let message = all_endpoints_failed_message(&lines, "llm_1");
    assert!(!message.contains("→"), "{message}");
}

/// 单端点池不说「所有端点」。
#[test]
fn a_single_endpoint_pool_does_not_say_every_endpoint() {
    let lines = vec![format!(
        "a / m（key#1）：{}",
        HttpFailureKind::RateLimit.label(false)
    )];
    let message = all_endpoints_failed_message(&lines, "llm_1");
    assert!(message.starts_with("模型端点没跑通"), "{message}");
}

/// 每条端点的理由按条裁：整段在 daemon 那头会被砍到 1000 字，砍掉的正好是排在
/// 后面的端点明细。
#[test]
fn a_long_reason_is_clipped_per_endpoint() {
    let clipped = clip_reason(&"很长的报文".repeat(100));
    assert!(
        clipped.chars().count() <= 161,
        "{}",
        clipped.chars().count()
    );
    assert!(clipped.ends_with('…'));
    assert_eq!(clip_reason("短的  报文\n还有一行"), "短的 报文 还有一行");
}
