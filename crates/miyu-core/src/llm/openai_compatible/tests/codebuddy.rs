//! codebuddy 中转协议:假 `codebuddy` 脚本回放 stream-json,验证参数拼装、
//! 载荷翻译、流事件→chunk 与会话续传。不碰真实登录态。
//!
//! 事件解析与 claude-code 共用一份(`claude_code::stream`),那边的
//! `tests/claude_code.rs` 已经把解析盯死了;这里只守 codebuddy 自己的那几处:
//! 二进制、**不发 `--effort`**、**不发 `--no-session-persistence`**、
//! 工具作用域与桥、以及续传只发增量。

use crate::llm::openai_compatible::tests::shared::*;
use crate::llm::openai_compatible::*;
use crate::llm::{ChatMessage, ChatStreamKind};

fn fake_codebuddy_script(dir: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("codebuddy");
    std::fs::write(
        &script,
        r#"#!/usr/bin/env bash
dir="$(cd "$(dirname "$0")" && pwd)"
sid="cb-$(basename "$dir")"
printf '%s\n' "$@" > "$dir/args.txt"
cat > "$dir/stdin.txt"
echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$sid\"}"
# 真 codebuddy 会多吐这一帧;解析器不认识它,必须当没看见而不是报错。
echo '{"type":"file-history-snapshot","id":"snap-1","snapshot":{"trackedFileBackups":{}}}'
echo '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}}'
echo '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"想一下"}}}'
echo '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}'
echo '{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}}'
echo '{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"你好"}}}'
echo '{"type":"stream_event","event":{"type":"content_block_stop","index":1}}'
echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$sid\",\"result\":\"你好\",\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":30,\"output_tokens\":4}}"
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn codebuddy_client(
    dir: &std::path::Path,
    provider_id: &str,
    native: &str,
    miyu: &str,
) -> OpenAiCompatibleClient {
    let mut provider = test_provider(provider_id, "");
    provider.protocol = "codebuddy".to_string();
    provider.default_model = "glm-5.3".to_string();
    let mut client = test_client(provider);
    client.codebuddy = Some(Arc::new(CodeBuddyRuntime {
        binary: fake_codebuddy_script(dir),
        native_tools: native.to_string(),
        miyu_tools: miyu.to_string(),
        permission_mode: "bypassPermissions".to_string(),
        idle_timeout: Duration::from_secs(30),
    }));
    client
}

fn read(dir: &std::path::Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

/// 首轮:该发的发了,claude 特有的两个 flag 一个都不许出现。
#[tokio::test]
async fn first_turn_sends_the_codebuddy_flag_set() {
    let temp = tempfile::tempdir().unwrap();
    let client = codebuddy_client(temp.path(), "codebuddy", "off", "off");
    let mut chunks = Vec::new();
    let result = client
        .chat_codebuddy_stream(
            vec![
                ChatMessage::system("你是未由。"),
                ChatMessage::plain("user", "在吗"),
            ],
            Vec::new(),
            "llm_cb_1",
            &mut |chunk| {
                chunks.push((chunk.kind, chunk.text));
                Ok(())
            },
        )
        .await
        .unwrap();

    let args = read(temp.path(), "args.txt");
    for flag in [
        "-p",
        "--output-format",
        "stream-json",
        "--input-format",
        "--include-partial-messages",
        "--model",
        "glm-5.3",
        "--system-prompt",
        "--strict-mcp-config",
    ] {
        assert!(args.contains(flag), "少了 {flag}：{args}");
    }
    // CodeBuddy 的 CLI 没有这两个参数(09-20 核过 `-h`),发过去会被它当成
    // 未知选项。
    assert!(
        !args.contains("--effort"),
        "codebuddy 不认 --effort：{args}"
    );
    assert!(
        !args.contains("--no-session-persistence"),
        "codebuddy 不认 --no-session-persistence：{args}"
    );
    // 原生工具关着时用 `--tools ""` 全禁,而不是给权限模式。
    assert!(args.contains("--tools"), "{args}");
    assert!(
        !args.contains("--permission-mode"),
        "工具关着就不该谈权限模式：{args}"
    );

    assert!(result.content.contains("你好"), "{:?}", result.content);
    assert!(
        chunks
            .iter()
            .any(|(kind, text)| *kind == ChatStreamKind::Reasoning && text.contains("想一下")),
        "思考没流出来：{chunks:?}"
    );
    // 不认识的 `file-history-snapshot` 帧必须被跳过而不是把整轮带崩。
    assert!(result.usage.is_some(), "用量该有:{result:?}");
}

/// 原生工具开着时给权限模式;Miyu 工具开着时挂 MCP 桥。
#[tokio::test]
async fn tool_scopes_shape_the_cli_args() {
    let temp = tempfile::tempdir().unwrap();
    let client = codebuddy_client(temp.path(), "codebuddy-tools", "all", "all");
    client
        .chat_codebuddy_stream(
            vec![ChatMessage::plain("user", "跑个命令")],
            Vec::new(),
            "llm_cb_2",
            &mut |_| Ok(()),
        )
        .await
        .unwrap();
    let args = read(temp.path(), "args.txt");
    assert!(args.contains("--permission-mode"), "{args}");
    assert!(args.contains("bypassPermissions"), "{args}");
    assert!(!args.contains("--tools\n\n"), "开着就不该全禁：{args}");
}

/// 续传:第二轮带 `--resume`,stdin 只发增量那一条。
#[tokio::test]
async fn appended_turn_resumes_and_sends_only_the_delta() {
    let temp = tempfile::tempdir().unwrap();
    let client = codebuddy_client(temp.path(), "codebuddy-resume", "off", "off");
    let first = vec![
        ChatMessage::system("你是未由。"),
        ChatMessage::plain("user", "第一句"),
    ];
    client
        .chat_codebuddy_stream(first.clone(), Vec::new(), "llm_cb_3", &mut |_| Ok(()))
        .await
        .unwrap();
    assert!(
        !read(temp.path(), "args.txt").contains("--resume"),
        "首轮不该续传"
    );

    let mut second = first;
    second.push(ChatMessage::assistant("你好", None));
    second.push(ChatMessage::plain("user", "第二句"));
    client
        .chat_codebuddy_stream(second, Vec::new(), "llm_cb_4", &mut |_| Ok(()))
        .await
        .unwrap();
    let args = read(temp.path(), "args.txt");
    assert!(args.contains("--resume"), "第二轮该续传：{args}");
    let stdin = read(temp.path(), "stdin.txt");
    assert!(stdin.contains("第二句"), "增量要在：{stdin}");
    assert!(
        !stdin.contains("第一句"),
        "续传只发增量，不该重发第一句：{stdin}"
    );
}

/// 二进制不在时给的是「装它 / 配 plugins.codebuddy.binary」，不是裸的 ENOENT。
#[tokio::test]
async fn missing_binary_reports_actionable_error() {
    let temp = tempfile::tempdir().unwrap();
    let mut client = codebuddy_client(temp.path(), "codebuddy-missing", "off", "off");
    let runtime = CodeBuddyRuntime {
        binary: temp.path().join("nope-not-here"),
        native_tools: "off".to_string(),
        miyu_tools: "off".to_string(),
        permission_mode: "bypassPermissions".to_string(),
        idle_timeout: Duration::from_secs(30),
    };
    client.codebuddy = Some(Arc::new(runtime));
    let error = client
        .chat_codebuddy_stream(
            vec![ChatMessage::plain("user", "在吗")],
            Vec::new(),
            "llm_cb_5",
            &mut |_| Ok(()),
        )
        .await
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("codebuddy") || text.contains("CodeBuddy"),
        "{text}"
    );
}
