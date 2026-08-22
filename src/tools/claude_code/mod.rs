//! `claude_code` 委托工具:把任务交给本机安装的 Claude Code CLI headless
//! 跑(用用户既有订阅登录态,Miyu 不经手任何凭据)。
//!
//! 注册面(§09):只进本机 owner 底座(builtin/dev),平台受限表不注册;
//! host_tools_allowed 的平台管理员会话复用 normal 底座,由 turn 装配层按
//! platform_context 再摘一次。子代理侧走 SUBAGENT_EXCLUDED 排除。

mod runner;

use crate::config::ClaudeCodePluginConfig;
use crate::paths::MiyuPaths;
use crate::tools::{ToolRegistry, ToolSpec};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn register(registry: &mut ToolRegistry, plugin: ClaudeCodePluginConfig, paths: MiyuPaths) {
    registry.register(
        ToolSpec::new(
            "claude_code",
            // 实际契约以 descriptions/claude_code.json 为准(注册时覆盖)。
            "Run the locally installed Claude Code CLI headless with the user's subscription login.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" }
                },
                "required": ["prompt"]
            }),
            move |args| {
                let plugin = plugin.clone();
                let paths = paths.clone();
                async move { run_claude_code(args, plugin, paths).await }
            },
        )
        .writes(),
    );
}

async fn run_claude_code(
    args: Value,
    plugin: ClaudeCodePluginConfig,
    paths: MiyuPaths,
) -> Result<String> {
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if prompt.trim().is_empty() {
        bail!("prompt is required");
    }
    let optional = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let request = runner::ClaudeCodeRequest {
        prompt: prompt.to_string(),
        cwd: optional("cwd").map(PathBuf::from),
        model: optional("model"),
        append_system_prompt: optional("append_system_prompt"),
        resume: optional("resume"),
    };
    runner::run(request, &plugin, &paths).await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tools::workspace;
    use std::path::Path;
    use std::sync::Arc;

    /// 假 claude 脚本:落到 cwd 里留证据(args/stdin/pwd/env),再按脚本体
    /// 行事。真实订阅一概不碰。
    fn write_fixture(dir: &Path, body: &str) -> String {
        let path = dir.join("claude");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > args.txt\ncat > stdin.txt\npwd > pwd.txt\nprintf '%s' \"${{ANTHROPIC_API_KEY:-unset}}\" > apikey.txt\n{body}\n"
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    fn plugin_with_binary(binary: String) -> ClaudeCodePluginConfig {
        ClaudeCodePluginConfig {
            binary,
            ..ClaudeCodePluginConfig::default()
        }
    }

    fn success_envelope() -> &'static str {
        r#"printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1234,"num_turns":3,"result":"fixture says hi","session_id":"sess-fixture","total_cost_usd":0.05,"usage":{},"modelUsage":{"claude-test-model":{}}}'"#
    }

    async fn call(
        session: &str,
        args: Value,
        plugin: ClaudeCodePluginConfig,
        paths: MiyuPaths,
    ) -> Result<String> {
        // 每个测试各占一个会话键:并发互斥名单是进程级的,测试并行跑时
        // 共用 "local" 会互相顶掉。
        workspace::with_session(
            Arc::from(session),
            run_claude_code(args, plugin, paths),
        )
        .await
    }

    #[tokio::test]
    async fn happy_path_passes_args_stdin_cwd_env_and_writes_audit() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let workdir = temp.path().join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let binary = write_fixture(temp.path(), success_envelope());
        std::env::set_var("ANTHROPIC_API_KEY", "leak-me-not");
        let output = call(
            "sess-happy",
            json!({
                "prompt": "do the thing",
                "cwd": workdir.display().to_string(),
                "model": "claude-sonnet-x",
                "append_system_prompt": "extra system",
                "resume": "sess-old",
            }),
            plugin_with_binary(binary),
            paths.clone(),
        )
        .await
        .unwrap();

        let args = std::fs::read_to_string(workdir.join("args.txt")).unwrap();
        assert_eq!(
            args,
            "-p\n--output-format\njson\n--permission-mode\nbypassPermissions\n--model\nclaude-sonnet-x\n--append-system-prompt\nextra system\n--resume\nsess-old\n"
        );
        assert_eq!(
            std::fs::read_to_string(workdir.join("stdin.txt")).unwrap(),
            "do the thing"
        );
        assert_eq!(
            std::fs::read_to_string(workdir.join("pwd.txt")).unwrap().trim(),
            workdir.canonicalize().unwrap().display().to_string()
        );
        // prefer_subscription 默认开:API key 不进子进程环境。
        assert_eq!(
            std::fs::read_to_string(workdir.join("apikey.txt")).unwrap(),
            "unset"
        );

        let body: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["result"], json!("fixture says hi"));
        assert_eq!(body["session_id"], json!("sess-fixture"));
        assert_eq!(body["cost_usd"], json!(0.05));
        assert_eq!(body["duration_ms"], json!(1234));
        assert_eq!(body["num_turns"], json!(3));
        assert_eq!(body["truncated"], json!(false));

        let audit =
            std::fs::read_to_string(paths.logs_dir().join("claude-code-usage.jsonl")).unwrap();
        let record: Value = serde_json::from_str(audit.lines().next().unwrap()).unwrap();
        assert_eq!(record["model"], json!("claude-sonnet-x"));
        assert_eq!(record["claude_session_id"], json!("sess-fixture"));
        assert_eq!(record["cost_usd"], json!(0.05));
        assert_eq!(record["duration_ms"], json!(1234));
        assert_eq!(record["prompt_bytes"], json!(12));
        assert_eq!(record["truncated"], json!(false));
        assert_eq!(record["is_error"], json!(false));
        assert!(record["ts"].as_str().is_some());

        // prefer_subscription=false 时环境原样继承。
        let mut plugin = plugin_with_binary(write_fixture(temp.path(), success_envelope()));
        plugin.prefer_subscription = false;
        call(
            "sess-happy-2",
            json!({"prompt": "again", "cwd": workdir.display().to_string()}),
            plugin,
            paths,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(workdir.join("apikey.txt")).unwrap(),
            "leak-me-not"
        );
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[tokio::test]
    async fn default_cwd_is_the_turn_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let workdir = temp.path().join("turn-ws");
        std::fs::create_dir_all(&workdir).unwrap();
        let binary = write_fixture(temp.path(), success_envelope());
        workspace::with_workspace(workdir.clone(), async {
            call(
                "sess-cwd",
                json!({"prompt": "where am i"}),
                plugin_with_binary(binary),
                paths,
            )
            .await
            .unwrap();
        })
        .await;
        assert_eq!(
            std::fs::read_to_string(workdir.join("pwd.txt")).unwrap().trim(),
            workdir.canonicalize().unwrap().display().to_string()
        );
    }

    #[tokio::test]
    async fn oversized_result_is_truncated_and_flagged() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let workdir = temp.path().join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let binary = write_fixture(
            temp.path(),
            r#"big=$(head -c 5000 /dev/zero | tr '\0' 'a')
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1,"num_turns":1,"result":"%s","session_id":"sess-big","total_cost_usd":0.01}' "$big""#,
        );
        let mut plugin = plugin_with_binary(binary);
        plugin.max_output_bytes = 1000;
        let output = call(
            "sess-trunc",
            json!({"prompt": "flood", "cwd": workdir.display().to_string()}),
            plugin,
            paths,
        )
        .await
        .unwrap();
        let body: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(body["result"].as_str().unwrap().len(), 1000);
        assert_eq!(body["session_id"], json!("sess-big"));
    }

    #[tokio::test]
    async fn is_error_envelope_becomes_a_tool_error() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let workdir = temp.path().join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let binary = write_fixture(
            temp.path(),
            r#"printf '{"type":"result","subtype":"error_during_execution","is_error":true,"duration_ms":9,"num_turns":1,"result":"it broke","session_id":"sess-err"}'"#,
        );
        let error = call(
            "sess-iserr",
            json!({"prompt": "fail", "cwd": workdir.display().to_string()}),
            plugin_with_binary(binary),
            paths.clone(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("error_during_execution"), "{error}");
        assert!(error.contains("it broke"), "{error}");
        // 失败也记账:订阅额度已经花出去了。
        let audit =
            std::fs::read_to_string(paths.logs_dir().join("claude-code-usage.jsonl")).unwrap();
        assert!(audit.contains(r#""is_error":true"#), "{audit}");
    }

    #[tokio::test]
    async fn nonzero_exit_reports_stderr_and_stdout_tails() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let workdir = temp.path().join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let binary = write_fixture(
            temp.path(),
            "echo partial-stdout\necho boom-stderr >&2\nexit 3",
        );
        let error = call(
            "sess-exit",
            json!({"prompt": "die", "cwd": workdir.display().to_string()}),
            plugin_with_binary(binary),
            paths,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("exit code 3"), "{error}");
        assert!(error.contains("boom-stderr"), "{error}");
        assert!(error.contains("partial-stdout"), "{error}");
    }

    #[tokio::test]
    async fn unparseable_stdout_is_a_json_parse_error() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let workdir = temp.path().join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let binary = write_fixture(temp.path(), "printf 'this is not json'");
        let error = call(
            "sess-json",
            json!({"prompt": "garble", "cwd": workdir.display().to_string()}),
            plugin_with_binary(binary),
            paths,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("parse Claude Code JSON output"), "{error}");
        assert!(error.contains("this is not json"), "{error}");
    }

    #[tokio::test]
    async fn missing_binary_reports_an_install_hint() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let workdir = temp.path().join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let error = call(
            "sess-missing",
            json!({"prompt": "hi", "cwd": workdir.display().to_string()}),
            plugin_with_binary(temp.path().join("no-such-claude").display().to_string()),
            paths,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("not found"), "{error}");
        assert!(error.contains("plugins.claude_code.binary"), "{error}");
    }

    #[tokio::test]
    async fn timeout_kills_the_whole_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let workdir = temp.path().join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let binary = write_fixture(
            temp.path(),
            "echo $$ > pid.txt\nsleep 300 &\necho $! > sleep_pid.txt\nwait",
        );
        let mut plugin = plugin_with_binary(binary);
        plugin.timeout_seconds = 1;
        let error = call(
            "sess-timeout",
            json!({"prompt": "hang", "cwd": workdir.display().to_string()}),
            plugin,
            paths,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("timed out after 1s"), "{error}");

        let read_pid = |name: &str| -> i32 {
            std::fs::read_to_string(workdir.join(name))
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };
        let shell_pid = read_pid("pid.txt");
        let sleep_pid = read_pid("sleep_pid.txt");
        // 组杀是异步生效的,给内核一点收尸时间再断言。
        for pid in [shell_pid, sleep_pid] {
            let mut alive = true;
            for _ in 0..20 {
                alive = unsafe { libc::kill(pid, 0) } == 0;
                if !alive {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            assert!(!alive, "process {pid} survived the group kill");
        }
    }

    #[tokio::test]
    async fn second_concurrent_call_in_the_same_session_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let workdir = temp.path().join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let binary = write_fixture(temp.path(), &format!("sleep 1\n{}", success_envelope()));
        let plugin = plugin_with_binary(binary);
        let args = json!({"prompt": "slow", "cwd": workdir.display().to_string()});
        let first = call("sess-race", args.clone(), plugin.clone(), paths.clone());
        let second = async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            call("sess-race", args.clone(), plugin.clone(), paths.clone()).await
        };
        let (first, second) = tokio::join!(first, second);
        assert!(first.is_ok(), "{first:?}");
        let error = second.unwrap_err().to_string();
        assert!(error.contains("already in progress"), "{error}");
    }

    /// 注册面(§09 + 08-20 统一总开关):内置 Claude Code 供应商默认禁用,
    /// 工具随之缺席;启用后本机 owner 底座有、平台受限表仍没有;子代理排除
    /// 表点名。
    #[test]
    fn registry_surfaces_follow_the_owner_only_rule() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let mut config = crate::config::AppConfig::default();
        config.normalize_builtin_providers();

        // 默认禁用:开箱不注册,要用户在供应商设置里显式启用订阅接入。
        assert!(!crate::tools::builtin_registry(&config, &paths).contains("claude_code"));
        assert!(!crate::tools::dev_registry(&config, &paths).contains("claude_code"));

        for provider in &mut config.providers {
            if provider.is_claude_code() {
                provider.enabled = true;
            }
        }
        assert!(config.claude_code_enabled());
        assert!(crate::tools::builtin_registry(&config, &paths).contains("claude_code"));
        assert!(crate::tools::dev_registry(&config, &paths).contains("claude_code"));
        assert!(
            !crate::tools::restricted_platform_registry(&config, &paths).contains("claude_code")
        );
        assert!(crate::tools::task::SUBAGENT_EXCLUDED.contains(&"claude_code"));
    }
}
