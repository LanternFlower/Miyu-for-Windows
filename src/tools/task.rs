use super::subagent_runner::{ProgressMode, SubagentProgress, SubagentRunner, SubagentStats};
use super::{ToolRegistry, ToolSpec};
use crate::config::{AppConfig, ModelTier};
use crate::i18n::agent_text as t;
use crate::llm::OpenAiCompatibleClient;
use crate::paths::MiyuPaths;
use anyhow::{bail, Result};
use serde_json::{json, Value};

const EXPLORE_SYSTEM_PROMPT: &str = include_str!("../prompts/subagent-explore.md");
const GENERAL_SYSTEM_PROMPT: &str = include_str!("../prompts/subagent-general.md");

const EXPLORE_ALLOWED: &[&str] = &[
    "read_file",
    "glob",
    "grep",
    "check_os_info",
    "read_clipboard",
    "web_fetch",
    "web_search",
];

const GENERAL_EXCLUDED: &[&str] = &[
    "task",
    "task_agent",
    "deep_research",
    "load_skill",
    "create_skill",
    "update_skill",
    "delete_skill",
    "publish_skill",
    "list_skill_drafts",
    "set_alarm",
    "list_alarms",
    "cancel_alarm",
    "search_meme",
    "show_meme",
    "add_meme",
    "update_meme",
    "delete_meme",
    "generate_image",
    "print_image",
    "search_web_images",
    "xuanxue_pick",
    "xuanxue_divine",
    "draw_zhouyi_hexagram",
    "draw_tarot_card",
    "draw_fortune_lot",
    "roll_dice",
];

const EXPLORE_TOOL_TIMEOUT: u64 = 60;
const GENERAL_TOOL_TIMEOUT: u64 = 120;

#[derive(Clone)]
struct TaskContext {
    config: AppConfig,
    paths: MiyuPaths,
    tools: ToolRegistry,
}

#[derive(Clone, Copy, PartialEq)]
enum SubagentType {
    Explore,
    General,
}

impl SubagentType {
    fn from_str(s: &str) -> Self {
        match s {
            "explore" => Self::Explore,
            _ => Self::General,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Explore => "explore",
            Self::General => "general",
        }
    }

    fn system_prompt(self) -> &'static str {
        match self {
            Self::Explore => EXPLORE_SYSTEM_PROMPT,
            Self::General => GENERAL_SYSTEM_PROMPT,
        }
    }

    fn tool_timeout(self) -> u64 {
        match self {
            Self::Explore => EXPLORE_TOOL_TIMEOUT,
            Self::General => GENERAL_TOOL_TIMEOUT,
        }
    }

    /// Default model tier when the caller doesn't pick one: exploration is
    /// read-only search (cheap), general work gets the balanced tier.
    fn default_tier(self) -> ModelTier {
        match self {
            Self::Explore => ModelTier::Cheap,
            Self::General => ModelTier::Balanced,
        }
    }
}

pub fn register(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    tools: ToolRegistry,
) {
    let config_for_status = config.clone();
    let context = TaskContext {
        config,
        paths,
        tools,
    };
    registry.register(ToolSpec::new_with_progress(
        "task",
        t(
            "Launch a subagent to handle a complex task independently. The subagent has its own system prompt, tool set, and LLM loop, and returns its final text to the main agent.",
            "启动子代理独立处理复杂任务。子代理有独立的系统提示、工具集和 LLM 循环，完成后返回最终文本给主 agent。",
        ),
        json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": t("Short task description for progress display.", "简短任务描述，用于进度展示。")
                },
                "prompt": {
                    "type": "string",
                    "description": t("Detailed task prompt. Must include full context, goals, and output requirements since the subagent has no access to the main agent's conversation history.", "详细任务提示。必须包含完整的上下文、目标和输出要求，因为子代理无法访问主 agent 的对话历史。")
                },
                "subagent_type": {
                    "type": "string",
                    "enum": ["explore", "general"],
                    // explore 是一份硬白名单(见 EXPLORE_ALLOWED),不是「所有只读工具」。
                    // 说成「只读搜索」会让模型以为 get_weather 这类只读工具也在里面,
                    // 于是拿 explore 去干需要专用工具的活,再自己用 web_fetch 硬凑。
                    "description": t("Subagent type. explore has exactly these 7 tools: read_file, glob, grep, web_search, web_fetch, check_os_info, read_clipboard — good for reading code and looking things up, nothing else is available. Anything needing another tool (weather, knowledge base, image generation, running commands) must use general, which inherits your full tool set. Defaults to general.", "子代理类型。explore 只有这 7 个工具：read_file、glob、grep、web_search、web_fetch、check_os_info、read_clipboard——适合读代码、查资料，别的一律没有。需要其它任何工具（天气、知识库、生图、运行命令等）就必须用 general，它继承你当前的全部工具。默认 general。"),
                    "default": "general"
                },
                "max_steps": {
                    "type": "integer",
                    "description": t("Optional tool-call budget. Unlimited by default: the subagent ends when the task is done. Set a number only when you want a hard cap.", "可选的工具调用步数预算。默认不限，子代理完成任务即自然结束；仅在需要硬性约束时设置。")
                },
                "background": {
                    "type": "boolean",
                    "description": t("Run the subagent detached in the background: returns a job_id immediately; check with job_status (its log holds live progress) and you are woken automatically on completion. Use for long research/tasks that should not block the conversation.", "后台分离运行子代理：立即返回 job_id，用 job_status 查询（日志即实时进度），完成后自动唤起你跟进。适合不应阻塞对话的长任务。")
                },
                "resume_id": {
                    "type": "string",
                    "description": t("Optional. When a previous task failed with a resume_id in its error, pass it here to continue that subagent from its last completed tool round instead of starting over (process-local; lost on restart).", "可选。当上一次 task 因连接中断失败并在错误中给出 resume_id 时，携带它可让该子代理从最后一个已完成的工具轮继续，而不是从头开始（仅本进程有效，重启后失效）。")
                },
                "tier": {
                    "type": "string",
                    "enum": ["cheap", "balanced", "strong"],
                    "description": t("Optional model tier, picked by task complexity: cheap for simple lookups/mechanical steps, balanced for typical multi-step work, strong for hard reasoning. Defaults: explore→cheap, general→balanced. Unconfigured tiers fall back to the main model.", "可选模型档位，按任务复杂度选择：cheap 适合简单查询/机械步骤，balanced 适合常规多步任务，strong 适合高难度推理。默认 explore→cheap、general→balanced。未配置的档位回退主模型。")
                }
            },
            "required": ["description", "prompt"],
            "additionalProperties": false
        }),
        move |args, progress| {
            let context = context.clone();
            async move { run_task(args, context, progress).await }
        },
    ).writes());
    registry.amend_description("task", &tier_pool_status(&config_for_status));
}

/// Human-readable tier pool status appended to the task tool description,
/// so the calling agent knows which tiers are configured and with which
/// concrete models when choosing a tier.
fn tier_pool_status(config: &AppConfig) -> String {
    // 全部未配置=默认形态:一个字都不追加。三行"未配置(回退主模型池)"
    // 对模型是零信息,还把动态文本焊进 tools 数组(字节稳定性隐患,
    // 验收 08-16 dev 解剖)。配置了档位的用户才见状态。
    let describe = |tier: ModelTier| {
        let pool = config.subagent_tier_choices(tier);
        if pool.is_empty() {
            String::new()
        } else {
            pool.iter()
                .map(|choice| choice.model.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let (cheap, balanced, strong) = (
        describe(ModelTier::Cheap),
        describe(ModelTier::Balanced),
        describe(ModelTier::Strong),
    );
    if cheap.is_empty() && balanced.is_empty() && strong.is_empty() {
        return String::new();
    }
    let fallback = t("main pool", "主池");
    let show = |pool: &str| -> String {
        if pool.is_empty() {
            fallback.to_string()
        } else {
            pool.to_string()
        }
    };
    format!(
        "{}cheap=[{}]; balanced=[{}]; strong=[{}]",
        t(" Current tier pools: ", " 当前档位池状态："),
        show(&cheap),
        show(&balanced),
        show(&strong),
    )
}

fn main_pool_choice(config: &AppConfig) -> Option<(String, String)> {
    config
        .active_provider_model_choices()
        .into_iter()
        .next()
        .map(|choice| (choice.provider_id, choice.model))
}

#[derive(Clone)]
struct TaskParams {
    description: String,
    prompt: String,
    sa_type: SubagentType,
    resume_id: Option<String>,
    max_steps: usize,
    tier: ModelTier,
}

/// Session linkage captured while still inside the turn scope — a detached
/// background subagent loses the task-locals, so the audit anchor must be
/// resolved before spawning.
#[derive(Clone)]
struct AuditAnchor {
    parent: Option<String>,
    persona: String,
}

fn parse_task_params(args: &Value) -> Result<TaskParams> {
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if description.is_empty() {
        bail!("description is required");
    }
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if prompt.is_empty() {
        bail!("prompt is required");
    }
    let sa_type = SubagentType::from_str(
        args.get("subagent_type")
            .and_then(Value::as_str)
            .unwrap_or("general"),
    );
    let resume_id = args
        .get("resume_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    // 0 = 不限步数(runner 语义):默认让子代理自然结束,预算仅在调用方
    // 显式给出 max_steps 时生效。
    let max_steps = args
        .get("max_steps")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(0);
    let tier = args
        .get("tier")
        .and_then(Value::as_str)
        .and_then(ModelTier::from_str)
        .unwrap_or_else(|| sa_type.default_tier());
    Ok(TaskParams {
        description,
        prompt,
        sa_type,
        resume_id,
        max_steps,
        tier,
    })
}

async fn run_task(
    args: Value,
    context: TaskContext,
    progress: crate::tools::ToolProgress,
) -> Result<String> {
    let params = parse_task_params(&args)?;
    let anchor = AuditAnchor {
        parent: crate::tools::workspace::try_session().map(|session| session.to_string()),
        persona: context.config.active_persona_scope(),
    };
    if args
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return spawn_background_task(context, params, anchor, progress).await;
    }
    run_task_core(context, progress, params, anchor).await
}

/// Detach the subagent run behind the shared background-job registry: its
/// progress streams into the job log, and completion goes through the same
/// wake path as background commands.
async fn spawn_background_task(
    context: TaskContext,
    params: TaskParams,
    anchor: AuditAnchor,
    progress: crate::tools::ToolProgress,
) -> Result<String> {
    let description = params.description.clone();
    crate::tools::jobs::spawn_background_subagent(None, &description, &progress, move |job_id, log_path| {
        async move {
            let bridge = spawn_subagent_log_bridge(log_path.clone());
            let output = run_task_core(context, bridge, params, anchor).await;
            let state_label = match &output {
                Ok(json) => serde_json::from_str::<Value>(json)
                    .ok()
                    .and_then(|value| value.get("state").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_else(|| "completed".to_string()),
                Err(_) => "error".to_string(),
            };
            let tail = match &output {
                Ok(json) => format!(
                    "\n{}\n{json}\n",
                    crate::tools::jobs::SUBAGENT_RESULT_MARKER
                ),
                Err(error) => format!(
                    "\n{}\n{error}\n",
                    crate::tools::jobs::SUBAGENT_ERROR_MARKER
                ),
            };
            let _ = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .and_then(|mut file| {
                    use std::io::Write as _;
                    file.write_all(tail.as_bytes())
                });
            tracing::debug!(job_id = %job_id, state = %state_label, "background subagent finished");
            match state_label.as_str() {
                "completed" | "budget_reached" => {
                    crate::tools::jobs::JobState::Exited { code: Some(0) }
                }
                "timeout" => crate::tools::jobs::JobState::TimedOut,
                _ => crate::tools::jobs::JobState::Exited { code: None },
            }
        }
    })
    .await
}

/// Bridge a detached subagent's progress stream into its job log so
/// `job_status` reads live progress the same way it reads command output.
fn spawn_subagent_log_bridge(log_path: std::path::PathBuf) -> crate::tools::ToolProgress {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            let crate::tools::ToolProgressEvent::Message(message) = event else {
                continue;
            };
            let line = readable_subagent_log_line(&message);
            if line.is_empty() {
                continue;
            }
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .and_then(|mut file| {
                    use std::io::Write as _;
                    writeln!(file, "{line}")
                });
        }
    });
    crate::tools::ToolProgress::new(sender)
}

fn readable_subagent_log_line(message: &str) -> String {
    if let Some(text) = message.strip_prefix("__subagent_reasoning__") {
        let text = text.trim();
        if text.is_empty() {
            return String::new();
        }
        return format!("[思考] {text}");
    }
    if let Some(text) = message.strip_prefix("__subtool_call__") {
        return format!("[工具] {}", text.trim());
    }
    if let Some(text) = message.strip_prefix("__subtool_result__") {
        return format!("[结果] {}", text.trim());
    }
    if let Some(text) = message.strip_prefix("__subagent_stats__") {
        return format!("[统计] {}", text.trim());
    }
    message.trim().to_string()
}

async fn run_task_core(
    context: TaskContext,
    progress: crate::tools::ToolProgress,
    params: TaskParams,
    anchor: AuditAnchor,
) -> Result<String> {
    let TaskParams {
        description,
        prompt,
        sa_type,
        resume_id,
        max_steps,
        tier,
    } = params;
    let tool_timeout = sa_type.tool_timeout();

    let mode = ProgressMode::from_config(&context.config);
    let enabled = context.config.plugins.deep_research.show_progress;
    let sa_progress = SubagentProgress::new(progress, mode, enabled);

    // Tier routing: the tier's pool gets its own load-balanced client;
    // an unconfigured pool silently uses the main model pool, and a
    // configured-but-unusable pool falls back with a notice returned to
    // the calling agent (not printed to the user).
    let pool = context.config.subagent_tier_choices(tier);
    let mut tier_notice: Option<String> = None;
    let (client, model_choice) = if pool.is_empty() {
        if !context.config.subagent_tiers.pool(tier).is_empty() {
            tier_notice = Some(format!(
                "tier '{}' pool has no usable model (models were removed from the text models); fell back to the main model pool",
                tier.label()
            ));
        }
        (
            OpenAiCompatibleClient::from_config(&context.config, &context.paths)?
                .with_request_scope("subagent"),
            main_pool_choice(&context.config),
        )
    } else {
        match OpenAiCompatibleClient::from_choices(&context.config, &context.paths, &pool) {
            Ok(client) => {
                let first = &pool[0];
                (
                    client.with_request_scope("subagent"),
                    Some((first.provider_id.clone(), first.model.clone())),
                )
            }
            Err(err) => {
                tier_notice = Some(format!(
                    "tier '{}' pool is unavailable ({err}); fell back to the main model pool",
                    tier.label()
                ));
                (
                    OpenAiCompatibleClient::from_config(&context.config, &context.paths)?
                .with_request_scope("subagent"),
                    main_pool_choice(&context.config),
                )
            }
        }
    };
    let client = client.for_subagent_output(mode == ProgressMode::Full);
    let tools = match sa_type {
        SubagentType::Explore => context.tools.clone_filtered(EXPLORE_ALLOWED),
        SubagentType::General => context.tools.clone(),
    };

    let runner = SubagentRunner::new(client, sa_type.system_prompt(), tools, sa_progress)
        .max_steps(max_steps)
        .timeout_seconds(tool_timeout)
        .excluded_tools(if sa_type == SubagentType::General {
            GENERAL_EXCLUDED
        } else {
            &[]
        });

    // 子代理不设总时长上限:它自然结束于任务完成或步数预算;逐工具超时
    // (tool_timeout)仍然兜底单步挂死。
    let (result, stats) = match runner.run_with_resume(&prompt, resume_id.as_deref()).await {
            Ok((result, stats)) => (result, stats),
            Err(err) => {
                let output = serde_json::to_string_pretty(&json!({
                    "ok": false,
                    "kind": "task",
                    "subagent_type": sa_type.label(),
                    "tier": tier.label(),
                    "tier_notice": tier_notice,
                    "description": description,
                    "state": "error",
                    "error": err.to_string(),
                    "stats": SubagentStats::default().public(),
                }))?;
                record_subagent_audit(
                    &context,
                    &anchor,
                    &description,
                    &prompt,
                    &output,
                    None,
                    &model_choice,
                );
                return Ok(output);
            }
        };

    let state = if stats.budget_reached {
        "budget_reached"
    } else {
        "completed"
    };

    let final_text = result.content.trim().to_string();

    let output = serde_json::to_string_pretty(&json!({
        "ok": true,
        "kind": "task",
        "subagent_type": sa_type.label(),
        "tier": tier.label(),
        "tier_notice": tier_notice,
        "description": description,
        "state": state,
        "result": final_text,
        "stats": stats.public(),
    }))?;
    // Prefer the endpoint that actually produced the final reply (pools
    // load-balance, so the representative pool entry may differ).
    let model_choice = match (&result.provider_id, &result.model) {
        (Some(provider_id), Some(model)) => Some((provider_id.clone(), model.clone())),
        _ => model_choice,
    };
    record_subagent_audit(
        &context,
        &anchor,
        &description,
        &prompt,
        &output,
        Some(&stats),
        &model_choice,
    );
    Ok(output)
}

/// Persists an audit session for a subagent run: a hidden `kind='subagent'`
/// session linked to the parent turn's session, holding one turn (prompt →
/// result JSON) plus the model identity and token usage on the session row.
/// Best-effort: audit failures never fail the task itself.
fn record_subagent_audit(
    context: &TaskContext,
    anchor: &AuditAnchor,
    description: &str,
    prompt: &str,
    output: &str,
    stats: Option<&SubagentStats>,
    model_choice: &Option<(String, String)>,
) {
    let outcome = (|| -> Result<()> {
        let store = crate::state::StateStore::new(&context.paths)?;
        let parent = anchor.parent.clone();
        let persona = anchor.persona.clone();
        let name: String = description.chars().take(40).collect();
        let record = store.create_session(&persona, &name, "subagent", parent.as_deref())?;
        let pinned = store.pinned(&record.session_id);
        let turn_id = format!(
            "sat_{}_{:08x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0),
            rand::random::<u32>()
        );
        pinned.start_turn(&turn_id, prompt, std::process::id())?;
        pinned.complete_turn(&turn_id, output, None)?;
        let (provider_id, model) = match model_choice.as_ref() {
            Some((provider_id, model)) => (Some(provider_id.as_str()), Some(model.as_str())),
            None => (None, None),
        };
        let context_window = match (provider_id, model) {
            (Some(provider), Some(model)) => context
                .config
                .context_window_for_provider_model(provider, model)
                .ok()
                .flatten()
                .map(|window| window as i64),
            _ => None,
        };
        let (prompt_tokens, completion_tokens, total_tokens, cache_read_tokens) = match stats {
            Some(stats) => (
                stats.prompt_tokens as i64,
                stats.completion_tokens as i64,
                stats.total_tokens.max(stats.token_estimate) as i64,
                stats.cache_read_tokens as i64,
            ),
            None => (0, 0, 0, 0),
        };
        store.record_subagent_usage(
            &record.session_id,
            provider_id,
            model,
            context_window,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cache_read_tokens,
        )
    })();
    if let Err(error) = outcome {
        tracing::warn!(error = %error, "{}", crate::i18n::text("failed to record subagent audit session", "记录子代理审计会话失败"));
    }
}
