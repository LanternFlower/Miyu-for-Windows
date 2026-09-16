//! `rm` 闸:模型不许直接 `rm`,删东西换个能挽回的办法(普通面上是 `trash_path`,
//! 移入回收站、可还原)。
//!
//! 拒绝理由**不点名工具**,所以一句话覆盖全部工具面:dev 面(core_only)没有
//! `trash_path`,点名它就成了指向不存在的东西;说「换个办法」她自己会找到本面
//! 有的那件(用户 09-16 裁定,KISS)。
//!
//! 在此之前 `run_command` 唯一的机械护栏是 `tools.command_deny` 的裸子串名单
//! (默认只有 `rm -rf /`、`rm -rf ~` 两条 rm 相关),`rm -f` 从来没被管过;裸子串
//! 还两头不讨好——`rm -rf /tmp/build` 因为含 `rm -rf /` 被误杀,`rm -fr /`、
//! `rm  -rf /` 却照过。
//!
//! 这里改按**命令位**(argv[0])判:`git rm`、`docker rm`、`rmdir`、`grep rm f`
//! 里的 `rm` 都不在命令位,照常放行;`cd /x && rm -f y`、`sudo rm`、
//! `ls | xargs rm`、`bash -c "rm y"` 一律拒。
//!
//! 开关是 `tools.block_dangerous_commands`(全局设置里的「高危命令拦截」),
//! 默认开。关掉之后 `run_command` 退回只剩 `tools.command_deny` 子串名单把关。
//!
//! 有意够不着的地方:`find -delete`、`python -c os.remove`、`truncate`、`> file`
//! 这些迂回删除不在本闸范围(只禁 `rm` 是明确裁定);双引号里的命令替换
//! (`echo "$(rm x)"`)按字符串处理;中转线 CLI 自带的 Bash 不经过 registry,
//! 本闸看不到它。

use super::ToolGuard;

/// 回给模型的拒绝理由。英文短句(AGENTS.md §1.5):先给理由(不可恢复),再给
/// 方向(换个办法),不点名工具。
const DENIAL: &str = "rm is blocked because it deletes permanently. Remove the path another way.";

/// 壳命令:它们自己不删东西,真正要跑的命令跟在后面。逐个精确解析各自的选项
/// 语法不值当(`sudo -u root rm`、`timeout 5 rm`、`xargs -I{} rm` 各一套),所以
/// 壳段里出现任何一个独立的 `rm` 词就算命中。误伤面只有 `sudo grep rm f` 这类
/// 几乎不存在的写法。
const WRAPPERS: &[&str] = &[
    "busybox", "command", "doas", "env", "exec", "find", "ionice", "nice", "nohup", "setsid",
    "sudo", "time", "timeout", "watch", "xargs",
];

/// `-c` 后面跟的是一整段脚本,递归再扫一遍。
const SHELLS: &[&str] = &["bash", "dash", "ksh", "sh", "zsh"];

/// 复合语句的关键字:它们后面跟的才是命令(`if true; then rm x; fi`)。
const KEYWORDS: &[&str] = &[
    "!", "case", "do", "done", "elif", "else", "esac", "fi", "if", "in", "then", "until", "while",
];

/// `sh -c "sh -c ..."` 这种套娃不值得无限展开。
const MAX_DEPTH: usize = 3;

/// 内置闸,随 `install_builtin_guards` 进每一张工具面。开关
/// (`tools.block_dangerous_commands`)关掉时原样放行——判不判由闸自己管,
/// 挂载点不必分叉。
pub(crate) fn rm_guard(enabled: bool) -> ToolGuard {
    std::sync::Arc::new(move |tool, args, _ctx| {
        if !enabled || tool.name != "run_command" {
            return None;
        }
        let command = args.get("command").and_then(serde_json::Value::as_str)?;
        invokes_rm(command).then(|| DENIAL.to_string())
    })
}

/// 命令串里有没有在命令位上调用 `rm`。
pub(crate) fn invokes_rm(command: &str) -> bool {
    scan(command, 0)
}

fn scan(command: &str, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    split_segments(command)
        .iter()
        .any(|segment| segment_invokes_rm(segment, depth))
}

fn segment_invokes_rm(words: &[String], depth: usize) -> bool {
    let mut rest = words
        .iter()
        .skip_while(|word| is_assignment(word) || KEYWORDS.contains(&word.as_str()));
    let Some(first) = rest.next() else {
        return false;
    };
    let program = basename(first);
    if program == "rm" {
        return true;
    }
    if SHELLS.contains(&program) {
        return script_argument(words).is_some_and(|script| scan(script, depth + 1));
    }
    if WRAPPERS.contains(&program) {
        return rest.any(|word| basename(word) == "rm");
    }
    false
}

/// 按未被引号包住的分隔符切段,每段是一串已去引号的词。
///
/// 不是完整的 shell 解析器,只认「哪里开始一条新命令」:`; | & 换行 ( ) ` { }`
/// 和 `$(` 都开新段;引号内的同样字符是字面量。
fn split_segments(command: &str) -> Vec<Vec<String>> {
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut has_word = false;
    let mut chars = command.chars().peekable();

    while let Some(current) = chars.next() {
        match current {
            '\'' => {
                has_word = true;
                for quoted in chars.by_ref() {
                    if quoted == '\'' {
                        break;
                    }
                    word.push(quoted);
                }
            }
            '"' => {
                has_word = true;
                while let Some(quoted) = chars.next() {
                    match quoted {
                        '"' => break,
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                word.push(escaped);
                            }
                        }
                        _ => word.push(quoted),
                    }
                }
            }
            '\\' => {
                if let Some(escaped) = chars.next() {
                    has_word = true;
                    word.push(escaped);
                }
            }
            ' ' | '\t' | '\r' => {
                if has_word {
                    words.push(std::mem::take(&mut word));
                    has_word = false;
                }
            }
            ';' | '|' | '&' | '\n' | '(' | ')' | '`' | '{' | '}' => {
                if has_word {
                    words.push(std::mem::take(&mut word));
                    has_word = false;
                }
                if !words.is_empty() {
                    segments.push(std::mem::take(&mut words));
                }
            }
            '$' if chars.peek() == Some(&'(') => {
                chars.next();
                if has_word {
                    words.push(std::mem::take(&mut word));
                    has_word = false;
                }
                if !words.is_empty() {
                    segments.push(std::mem::take(&mut words));
                }
            }
            _ => {
                has_word = true;
                word.push(current);
            }
        }
    }
    if has_word {
        words.push(word);
    }
    if !words.is_empty() {
        segments.push(words);
    }
    segments
}

/// `FOO=1 rm x` 里的赋值前缀:跳过它之后才是命令位。
fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `sh -c`/`bash -lc` 后面那段脚本。长选项(`--rcfile`)里也有 `c`,要排掉。
fn script_argument(words: &[String]) -> Option<&str> {
    words
        .iter()
        .position(|word| word.starts_with('-') && !word.starts_with("--") && word.contains('c'))
        .and_then(|index| words.get(index + 1))
        .map(String::as_str)
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

#[cfg(test)]
mod tests {
    use super::invokes_rm;

    /// 命令位上的 `rm`,一条都不许过。清单里每一条都是模型真会写的形状。
    #[test]
    fn rm_in_command_position_is_blocked() {
        let blocked = [
            "rm x",
            "rm -f /tmp/x",
            "rm -rf /",
            "  rm   -rf   build  ",
            "cd /tmp && rm -rf miyu-rollback",
            "ls; rm -f a",
            "ls | xargs rm",
            "ls | xargs -I{} rm {}",
            "sudo rm -f /etc/hosts",
            "sudo -u root rm x",
            "env FOO=1 rm x",
            "FOO=1 rm x",
            "nohup rm x &",
            "timeout 5 rm x",
            "nice -n 5 rm x",
            r"find . -name '*.log' -exec rm {} \;",
            "/bin/rm x",
            "/usr/bin/rm -f x",
            r"\rm x",
            "'rm' x",
            "\"rm\" -f x",
            "bash -c \"rm -rf x\"",
            "sh -c 'cd /x && rm -rf y'",
            "echo hi\nrm -f x",
            "$(rm x)",
            "`rm x`",
            "if true; then rm x; fi",
        ];
        for command in blocked {
            assert!(invokes_rm(command), "should be blocked: {command:?}");
        }
    }

    /// `rm` 出现在参数位(`git rm`)、出现在别的名字里(`rmdir`)、或者只是一段
    /// 文本,都不能误伤——裸子串名单就是死在这上面。
    #[test]
    fn rm_outside_command_position_is_allowed() {
        let allowed = [
            "ls -la",
            "git rm --cached a",
            "git -C /x rm y",
            "docker rm container",
            "npm rm package",
            "rmdir empty",
            "echo rm",
            "grep rm file.txt",
            "cat /home/rm.txt",
            "cargo clean",
            "echo \"rm -rf /\"",
            "ls /tmp/rm",
            "firmware-update",
            "",
        ];
        for command in allowed {
            assert!(!invokes_rm(command), "should be allowed: {command:?}");
        }
    }

    /// 闸是内置的:不靠配置、不靠名单,每张工具面都带着它,而且只作用于
    /// `run_command`。
    #[tokio::test]
    async fn builtin_guards_block_rm_in_run_command() {
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register(crate::tools::ToolSpec::new(
            "run_command",
            "runs",
            serde_json::json!({"type":"object","properties":{}}),
            |_| async { Ok("ran".to_string()) },
        ));
        crate::tools::install_builtin_guards(&mut registry, &crate::config::AppConfig::default());

        let denied = registry
            .call("run_command", r#"{"command":"rm -f /tmp/x"}"#)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            denied.contains("rm is blocked"),
            "unexpected denial: {denied}"
        );

        let allowed = registry
            .call("run_command", r#"{"command":"git rm --cached a"}"#)
            .await
            .unwrap();
        assert_eq!(allowed, "ran");
    }

    /// 开关关掉就不挂闸,`run_command` 退回只有子串名单把关的老样子。
    #[tokio::test]
    async fn the_switch_turns_the_guard_off() {
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register(crate::tools::ToolSpec::new(
            "run_command",
            "runs",
            serde_json::json!({"type":"object","properties":{}}),
            |_| async { Ok("ran".to_string()) },
        ));
        let mut config = crate::config::AppConfig::default();
        config.tools.block_dangerous_commands = false;
        crate::tools::install_builtin_guards(&mut registry, &config);

        let allowed = registry
            .call("run_command", r#"{"command":"rm -f /tmp/x"}"#)
            .await
            .unwrap();
        assert_eq!(allowed, "ran");
    }

    /// 面级回归:normal 与 dev 两面都拦。拒绝理由不点名工具,所以 dev 面
    /// (core_only,没有 `trash_path`)也能用同一句话(用户 09-16 裁定)。
    ///
    /// 探针选不存在的路径:万一哪天真放行了,`rm -f` 对不存在的目标是空操作,
    /// 不会删掉任何东西。
    #[tokio::test]
    async fn rm_guard_covers_every_face() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let config = crate::config::AppConfig::default();
        let probe = r#"{"command":"rm -f /nonexistent-miyu-rm-guard-probe"}"#;

        for mode in [
            crate::agent::AgentMode::Normal,
            crate::agent::AgentMode::Dev,
        ] {
            let denial = crate::tools::build_tool_registry(&config, &paths, mode, false)
                .unwrap()
                .call("run_command", probe)
                .await
                .unwrap_err()
                .to_string();
            assert!(denial.contains("rm is blocked"), "{mode:?} face: {denial}");
        }
    }
}
