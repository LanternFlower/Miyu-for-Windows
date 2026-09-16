use super::*;

/// 中途的量报只刷标题和状态行，**不进流水账**。
///
/// 它一秒能来好几次（每调完一个工具报一次）。落进去的话，面板里那条时间线
/// 会被「统计」节点撑满，真正在干什么反而看不见了。
#[test]
fn running_metric_never_lands_in_the_job_log() {
    assert_eq!(
        readable_subagent_log_line_timed("__subagent_metric__1.2K\t工具调用 3 次", None),
        ""
    );
    // 跑完那一次照旧留底。
    assert_eq!(
        readable_subagent_log_line_timed("__subagent_stats__工具调用 3 次", None),
        "[统计] 工具调用 3 次"
    );
}

/// 结果那一行要把工具真吐出来的东西带上。
///
/// 只写一句「运行命令 ok · ls」的话，面板里那一步点开看到的还是同一句话
/// ——等于点开是空的（用户实测：浮层里这些工具展开都没内容）。
#[test]
fn tool_result_carries_its_output_into_the_log() {
    let json = serde_json::json!({
        "name": "run_command",
        "args": r#"{"command":"ls"}"#,
        "ok": true,
        "output": "total 12\n\ndrwxr-xr-x 2 shorin\n",
    })
    .to_string();
    let line = readable_subagent_log_line_timed(&format!("__subtool_result__{json}"), None);
    let mut lines = line.lines();
    assert!(
        lines
            .next()
            .unwrap_or_default()
            .starts_with("[结果] run_command\t"),
        "{line}"
    );
    assert_eq!(lines.next(), Some("[输出] total 12"), "{line}");
    // 空行不占一条记录。
    assert_eq!(lines.next(), Some("[输出] drwxr-xr-x 2 shorin"), "{line}");
    assert_eq!(lines.next(), None, "{line}");
}

/// 正文也是**逐 delta** 来的，得攒成段落再落盘。
///
/// 一条一行的话日志会变成每行一个词的字符梯（用户实测截图：整屏
/// `[正文] the` / `[正文] and`）。
#[test]
fn streamed_speech_is_batched_into_paragraphs() {
    let mut buffer = String::new();
    let mut lines = Vec::new();
    for chunk in ["Now ", "let ", "me ", "enumerate."] {
        accumulate_stream(&mut buffer, chunk, "[正文]", &mut lines);
    }
    assert!(lines.is_empty(), "还没到段落就落盘了: {lines:?}");
    flush_stream_buffer(&mut buffer, "[正文]", &mut lines);
    assert_eq!(lines, vec!["[正文] Now let me enumerate.".to_string()]);
    // 空行就是段落分隔，到了就落一条。
    let mut lines = Vec::new();
    accumulate_stream(&mut buffer, "第一段\n\n第二段", "[正文]", &mut lines);
    assert_eq!(lines, vec!["[正文] 第一段".to_string()]);
    flush_stream_buffer(&mut buffer, "[正文]", &mut lines);
    assert_eq!(lines[1], "[正文] 第二段");
}

/// 工具吐的原始输出要洗干净再进流水账。
///
/// 转义序列、回车、制表符原样写进去的话，面板按纯文本算宽度，算出来的和
/// 真实占宽对不上，右边那根竖线跟着参差不齐。
#[test]
fn tool_output_is_plain_text_in_the_log() {
    let json = serde_json::json!({
        "name": "run_command",
        "args": "{}",
        "ok": true,
        "output": "\u{1b}[31m红的\u{1b}[0m\ta\u{7}b\r\n干净一行\n",
    })
    .to_string();
    let line = readable_subagent_log_line_timed(&format!("__subtool_result__{json}"), None);
    let outputs = line
        .lines()
        .filter_map(|line| line.strip_prefix("[输出] "))
        .collect::<Vec<_>>();
    assert_eq!(outputs, vec!["红的 ab", "干净一行"], "{line}");
    // `[结果]` 那一行自己带一个制表符（工具 id 的分隔），只看输出那几行。
    assert!(
        outputs
            .iter()
            .all(|line| !line.contains(|ch: char| ch.is_control())),
        "{line}"
    );
}

/// 差事写在流水账开头，换行折成 `\u{1}`（面板那边再拆回来）。
#[test]
fn prompt_header_folds_newlines() {
    let dir = std::env::temp_dir().join(format!("miyu-prompt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建目录");
    let path = dir.join("job.log");
    write_subagent_prompt_header(&path, "  第一行\n第二行  ");
    let text = std::fs::read_to_string(&path).expect("读日志");
    assert_eq!(text, "[提示] 第一行\u{1}第二行\n");
    // 空差事不写。
    let empty = dir.join("empty.log");
    write_subagent_prompt_header(&empty, "   ");
    assert!(!empty.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

fn test_paths(root: &std::path::Path) -> MiyuPaths {
    crate::tools::tests::test_paths(root)
}

#[test]
fn dev_flag_defaults_to_off_and_parses() {
    let base = json!({"description": "d", "prompt": "p"});
    assert!(!parse_params(&base).unwrap().dev);
    let mut with_dev = base.clone();
    with_dev["dev"] = json!(true);
    assert!(parse_params(&with_dev).unwrap().dev);
}

/// dev 子代理的系统提示词是三段拼起来的,少任何一段它都得先浪费一轮
/// 去问「我在哪、说给谁听」。
#[test]
fn dev_system_prompt_carries_the_dev_prompt_host_block_and_contract() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let prompt = build_dev_system_prompt(&config, &paths).unwrap();
    assert!(
        prompt.starts_with(miyu_base::config::DEFAULT_DEV_SYSTEM_PROMPT),
        "{prompt}"
    );
    assert!(prompt.contains("<host-environment"), "{prompt}");
    assert!(prompt.contains("<runtime cwd="), "{prompt}");
    assert!(prompt.ends_with(SUBAGENT_DEV_CONTRACT), "{prompt}");
}

/// 同一个会话里连开两个 dev 子代理,系统提示词必须逐字节相同——不然
/// 每一个都是一次冷前缀。
#[test]
fn dev_system_prompt_is_byte_stable_within_a_session() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    assert_eq!(
        build_dev_system_prompt(&config, &paths).unwrap(),
        build_dev_system_prompt(&config, &paths).unwrap()
    );
}

/// 递归防护:dev 子代理拿的是 dev 会话那张面,而那张面里也注册着
/// `subagent`——排除表必须认得新名,否则子代理能自己再开子代理。
#[test]
fn subagent_excludes_itself_by_its_current_name() {
    assert!(SUBAGENT_EXCLUDED.contains(&"subagent"));
}

/// 后台子代理起在 `tokio::spawn` 的新任务上,回合的 task-local 到那儿
/// 全空了:成员的后台子代理会因此跑在 Landlock 之外,工具的工作目录
/// 也退回 daemon 的 cwd。这条钉住「抓下来再套回去」。
#[tokio::test]
async fn background_scope_is_carried_across_the_spawn() {
    let workspace = std::path::PathBuf::from("/tmp/miyu-subagent-scope");
    let session: std::sync::Arc<str> = "sess_probe".into();
    let (bare, restored) = miyu_base::workspace::with_workspace(
        workspace.clone(),
        miyu_base::workspace::with_session(session.clone(), async {
            let carried_workspace = miyu_base::workspace::try_workspace();
            let carried_session = miyu_base::workspace::try_session();
            tokio::spawn(async move {
                let bare = (
                    miyu_base::workspace::try_workspace(),
                    miyu_base::workspace::try_session(),
                );
                let restored = with_turn_scope(None, carried_workspace, carried_session, async {
                    (
                        miyu_base::workspace::try_workspace(),
                        miyu_base::workspace::try_session(),
                    )
                })
                .await;
                (bare, restored)
            })
            .await
            .unwrap()
        }),
    )
    .await;
    assert_eq!(bare, (None, None), "裸 spawn 本就看不见回合作用域");
    assert_eq!(restored, (Some(workspace), Some(session)));
}
