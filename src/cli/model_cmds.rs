//! 模型与思考变体的命令。
//!
//! `miyu models` 管的是「这个会话用哪些模型」，`miyu variant` 管的是「思考多
//! 深」。两者都有全局池与会话覆盖两层：会话不设就继承全局，设了就只用自己
//! 那份。菜单渲染也在这里——终端里要在有限宽度内把 provider、模型名、变体三
//! 列排整齐。

use crate::cli::repl::width::*;
use crate::cli::*;

pub(in crate::cli) fn short_model_name(model: &str, provider: &str) -> String {
    model
        .strip_prefix(&format!("{provider}/"))
        .unwrap_or(model)
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_string()
}

pub(in crate::cli) fn print_mixed_model_endpoint(
    show: bool,
    result: &crate::llm::ChatResult,
    variant: Option<&str>,
) {
    if !show {
        return;
    }
    let provider = result.provider_id.as_deref().unwrap_or("-");
    let model = result.model.as_deref().unwrap_or("-");
    println!(
        "\x1b[2m{}\x1b[0m\n",
        mixed_model_endpoint_label(provider, model, variant)
    );
}

pub(in crate::cli) fn mixed_model_endpoint_label(
    provider: &str,
    model: &str,
    variant: Option<&str>,
) -> String {
    let variant = variant
        .filter(|variant| !variant.is_empty())
        .map(|variant| format!(" · {variant}"))
        .unwrap_or_default();
    format!("{provider} / {model}{variant}")
}

pub(in crate::cli) fn show_mixed_model_endpoint(config: &AppConfig, interactive: bool) -> bool {
    config.active_provider_model_choices().len() > 1
        && match config.display.mixed_model_endpoint_display.as_str() {
            "off" => false,
            "all" => true,
            _ => interactive,
        }
}

pub(in crate::cli) fn initialize_models_cache(paths: &MiyuPaths) {
    crate::models_cache::try_load(paths);
    crate::models_cache::spawn_background_refresh(paths.clone());
    if let Ok(config) = AppConfig::load_or_default(paths) {
        crate::models_cache::spawn_provider_api_refresh(config.providers);
    }
}

pub(in crate::cli) async fn run_models(paths: &MiyuPaths, args: ModelsArgs) -> Result<()> {
    run_models_for_session(paths, args, None).await
}

/// REPL 的 `/models` 收的是一整串自由文本,这里把 `--global` / `-g` 从中
/// 摘出来,让两个入口的写法一致(`/models --global`、`/models -g gpt-5`)。
pub(in crate::cli) fn parse_models_argument(argument: &str) -> ModelsArgs {
    let mut global = false;
    let mut rest = argument.trim();
    loop {
        let stripped = rest
            .strip_prefix("--global")
            .or_else(|| rest.strip_prefix("-g"))
            .filter(|remainder| remainder.is_empty() || remainder.starts_with(char::is_whitespace));
        match stripped {
            Some(remainder) => {
                global = true;
                rest = remainder.trim_start();
            }
            None => break,
        }
    }
    ModelsArgs {
        target: (!rest.is_empty()).then(|| rest.to_string()),
        global,
    }
}

/// `miyu models --global`:直接编辑全局激活模型池。
///
/// 不带 --global 时这条命令改的只是终端集成会话的覆盖,全局池此前只能进
/// `miyu config` 的 TUI 里翻。全局池是所有没有单独覆盖的会话(WebUI、通讯
/// 平台、新开的终端会话)共同的默认来源,值得有一条一行就能改完的路。
///
/// 与会话覆盖的两点不同:池不能清空(至少留一个端点,`set_active_provider_models`
/// 自己会拦),以及 `default` 没有意义——全局池本身就是那个"默认"。
pub(in crate::cli) async fn run_models_global(
    paths: &MiyuPaths,
    target: Option<&str>,
) -> Result<()> {
    let mut config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    if choices.is_empty() {
        bail!(
            "{}",
            t(
                "no configured provider models; configure a model first",
                "没有已配置的 provider 模型；请先配置模型",
            )
        );
    }
    let selected = if let Some(target) = target.map(str::trim) {
        if target.eq_ignore_ascii_case("default") || target.eq_ignore_ascii_case("global") {
            bail!(
                "{}",
                t(
                    "the global pool is the default; pick a concrete model instead",
                    "全局池本身就是默认来源，请直接指定具体模型",
                )
            );
        }
        let choice = crate::config::resolve_provider_model_argument(&choices, target)
            .map_err(anyhow::Error::msg)?;
        vec![ActiveProviderModelConfig {
            provider_id: choice.provider_id.clone(),
            model: choice.model.clone(),
        }]
    } else {
        if !(io::stdout().is_terminal() && io::stdin().is_terminal()) {
            print_model_choices(&config, &choices, None);
            return Ok(());
        }
        let initial = choices
            .iter()
            .map(|choice| config.is_active_provider_model(&choice.provider_id, &choice.model))
            .collect::<Vec<_>>();
        let Some(active) = inline_fuzzy_select(
            &choices
                .iter()
                .map(|choice| choice.label())
                .collect::<Vec<_>>(),
            initial.clone(),
        )?
        else {
            return Ok(());
        };
        if active == initial {
            println!(
                "{}",
                t(
                    "no changes (Enter picks the highlighted model; Tab multi-selects)",
                    "未做修改（回车=选定高亮模型,Tab=多选勾选）"
                )
            );
            return Ok(());
        }
        choices
            .iter()
            .zip(active)
            .filter_map(|(choice, active)| {
                active.then(|| ActiveProviderModelConfig {
                    provider_id: choice.provider_id.clone(),
                    model: choice.model.clone(),
                })
            })
            .collect::<Vec<_>>()
    };
    if selected.is_empty() {
        bail!(
            "{}",
            t(
                "at least one model must stay active in the global pool",
                "全局池至少要保留一个激活模型",
            )
        );
    }
    config.set_active_provider_models(&selected)?;
    config.save(paths)?;
    let labels = selected
        .iter()
        .map(|model| format!("{}/{}", model.provider_id, model.model))
        .collect::<Vec<_>>()
        .join(", ");
    println!("{}: {labels}", t("global model pool", "全局激活模型池"));
    // daemon 在跑就让它立刻吃到新池,否则要等下次重启。已绑定覆盖的会话
    // 不受影响——它们本来就不看全局池。
    if ipc::daemon_info(paths).await.is_some() {
        retry_config_reload(RELOAD_MAX_ATTEMPTS, RELOAD_RETRY_INTERVAL, || {
            request_config_reload(paths)
        })
        .await?;
    }
    Ok(())
}

/// Switches the model pool of one session (the current session when
/// `session_id` is None). The override persists on the session, so reopening
/// it restores the model; the global pool is managed in `miyu config`.
pub(in crate::cli) async fn run_models_for_session(
    paths: &MiyuPaths,
    args: ModelsArgs,
    session_id: Option<&str>,
) -> Result<()> {
    if args.global {
        return run_models_global(paths, args.target.as_deref()).await;
    }
    let config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    if choices.is_empty() {
        bail!(
            "{}",
            t(
                "no configured provider models; configure a model first",
                "没有已配置的 provider 模型；请先配置模型",
            )
        );
    }
    if let Some(target) = args.target.as_deref() {
        let target = target.trim();
        if target.eq_ignore_ascii_case("default") || target.eq_ignore_ascii_case("global") {
            set_session_models(paths, session_id, Vec::new()).await?;
            println!(
                "{}",
                t(
                    "this session now follows the global active pool",
                    "当前会话已恢复跟随全局激活模型池"
                )
            );
            return Ok(());
        }
        let choice = crate::config::resolve_provider_model_argument(&choices, target)
            .map_err(anyhow::Error::msg)?;
        let label = choice.label();
        let models = vec![ActiveProviderModelConfig {
            provider_id: choice.provider_id.clone(),
            model: choice.model.clone(),
        }];
        set_session_models(paths, session_id, models).await?;
        println!("{}: {label}", t("session model", "当前会话模型"));
        return Ok(());
    }
    if io::stdout().is_terminal() && io::stdin().is_terminal() {
        let override_pool = session_model_override_snapshot(paths, session_id)?;
        // 第一项是「继承全局模型池」,与 config TUI 的会话/QQ 模型菜单同款:
        // 会话没有自己的覆盖时它就是当前状态。此前想恢复继承只能记住
        // `miyu models default` 这个隐藏写法,菜单里根本看不到这条路。
        let inherit_label = t("Inherit global model pool", "继承全局模型池").to_string();
        let mut labels = vec![inherit_label];
        labels.extend(choices.iter().map(|choice| choice.label()));
        let mut initial = vec![override_pool.is_none()];
        initial.extend(choices.iter().map(|choice| match override_pool.as_deref() {
            Some(pool) => pool.iter().any(|model| {
                model.provider_id == choice.provider_id && model.model == choice.model
            }),
            None => config.is_active_provider_model(&choice.provider_id, &choice.model),
        }));
        if let Some(active) = inline_fuzzy_select(&labels, initial.clone())? {
            // 勾了「继承」就是继承:继承与覆盖天然互斥,同时勾选时以继承为准
            // (标签本身也这么说)。
            if active.first().copied().unwrap_or(false) && !initial[0] {
                set_session_models(paths, session_id, Vec::new()).await?;
                println!(
                    "{}",
                    t(
                        "this session now follows the global active pool",
                        "当前会话已恢复跟随全局激活模型池"
                    )
                );
                return Ok(());
            }
            let active = active.into_iter().skip(1).collect::<Vec<_>>();
            let initial = initial.into_iter().skip(1).collect::<Vec<_>>();
            if active == initial {
                println!(
                    "{}",
                    t(
                        "no changes (Enter picks the highlighted model; Tab multi-selects)",
                        "未做修改（回车=选定高亮模型,Tab=多选勾选）"
                    )
                );
                return Ok(());
            }
            let models = choices
                .iter()
                .zip(active)
                .filter_map(|(choice, active)| {
                    active.then(|| ActiveProviderModelConfig {
                        provider_id: choice.provider_id.clone(),
                        model: choice.model.clone(),
                    })
                })
                .collect::<Vec<_>>();
            let cleared = models.is_empty();
            set_session_models(paths, session_id, models).await?;
            if cleared {
                println!(
                    "{}",
                    t(
                        "this session now follows the global active pool",
                        "当前会话已恢复跟随全局激活模型池"
                    )
                );
            } else {
                println!("{}", t("session models updated", "已更新当前会话模型"));
            }
        }
        return Ok(());
    }
    print_model_choices(&config, &choices, None);
    Ok(())
}

pub(in crate::cli) fn run_list_models(paths: &MiyuPaths) -> Result<()> {
    let config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    if choices.is_empty() {
        bail!(
            "{}",
            t(
                "no configured provider models; configure a model first",
                "没有已配置的 provider 模型；请先配置模型",
            )
        );
    }
    let override_pool = session_model_override_snapshot(paths, None)?;
    print_model_choices(&config, &choices, override_pool.as_deref());
    println!(
        "{}",
        t(
            "switch with: miyu models <index|provider/model>; 'miyu models default' follows the global pool",
            "切换：miyu models <序号|供应商/模型>；miyu models default 恢复跟随全局模型池"
        )
    );
    Ok(())
}

pub(in crate::cli) fn print_model_choices(
    config: &AppConfig,
    choices: &[crate::config::ProviderModelChoice],
    override_pool: Option<&[ActiveProviderModelConfig]>,
) {
    for (index, choice) in choices.iter().enumerate() {
        let active = match override_pool {
            Some(pool) => pool.iter().any(|model| {
                model.provider_id == choice.provider_id && model.model == choice.model
            }),
            None => config.is_active_provider_model(&choice.provider_id, &choice.model),
        };
        let marker = if active { "[*]" } else { "[ ]" };
        println!("{marker} {}. {}", index + 1, choice.label());
    }
    match override_pool {
        Some(_) => println!(
            "{}",
            t(
                "[*] = models pinned to the current session",
                "[*] = 当前会话固定使用的模型"
            )
        ),
        None => println!(
            "{}",
            t(
                "[*] = global active pool (the current session follows it)",
                "[*] = 全局激活模型池（当前会话跟随全局）"
            )
        ),
    }
}

/// Reads a session's model override straight from the shared state database;
/// works whether or not the daemon is running.
pub(in crate::cli) fn session_model_override_snapshot(
    paths: &MiyuPaths,
    session_id: Option<&str>,
) -> Result<Option<Vec<ActiveProviderModelConfig>>> {
    let store = StateStore::new(paths)?;
    let session_id = match session_id {
        Some(session_id) => session_id.to_string(),
        None => store.session_id().to_string(),
    };
    store.session_model_override(&session_id)
}

pub(in crate::cli) async fn set_session_models(
    paths: &MiyuPaths,
    session_id: Option<&str>,
    models: Vec<ActiveProviderModelConfig>,
) -> Result<()> {
    if ipc::daemon_info(paths).await.is_some() {
        let target = match session_id {
            Some(id) => ipc::SessionRef::Id { id: id.to_string() },
            None => ipc::SessionRef::Current,
        };
        send_ipc_command(paths, IpcCommand::SetSessionModels { target, models }).await?;
        return Ok(());
    }
    let config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    for model in &models {
        if !choices
            .iter()
            .any(|choice| choice.provider_id == model.provider_id && choice.model == model.model)
        {
            bail!(
                "{}{}/{}",
                t("unknown model: ", "未知模型："),
                model.provider_id,
                model.model
            );
        }
    }
    let store = StateStore::new(paths)?;
    let session_id = match session_id {
        Some(session_id) => session_id.to_string(),
        None => store.session_id().to_string(),
    };
    store.set_session_model_override(
        &session_id,
        (!models.is_empty()).then_some(models.as_slice()),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli) enum VariantOutcome {
    Updated,
    Cancelled,
    Rejected(String),
}

pub(in crate::cli) fn run_variant(paths: &MiyuPaths, args: VariantArgs) -> Result<()> {
    let selected = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if selected.is_none() && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
        bail!(
            "{}",
            t(
                "interactive variant selection requires a terminal; use `miyu variant <name>`",
                "交互 variant 选择需要终端；请使用 `miyu variant <名称>`",
            )
        );
    }
    if !crate::models_cache::is_loaded() {
        crate::models_cache::refresh_blocking(paths).map_err(|error| {
            anyhow::anyhow!(
                "{}: {error:#}",
                t("failed to load model metadata", "无法加载模型元数据")
            )
        })?;
    }

    let config = AppConfig::load_or_default(paths)?;
    let mut client = OpenAiCompatibleClient::from_config(&config, paths)?;
    match execute_variant(paths, &mut client, selected, "miyu variant")? {
        VariantOutcome::Updated => print_variant_updated(),
        VariantOutcome::Cancelled => {}
        VariantOutcome::Rejected(message) => bail!("{message}"),
    }
    Ok(())
}

pub(in crate::cli) fn execute_variant(
    paths: &MiyuPaths,
    client: &mut OpenAiCompatibleClient,
    selected: Option<&str>,
    selector_command: &str,
) -> Result<VariantOutcome> {
    if let Some(selected) = selected {
        if client.thinking_variant_options().len() != 1 {
            let message = if is_zh() {
                format!("当前激活了多个模型；请使用 {selector_command} 在 TUI 中分别设置")
            } else {
                format!(
                    "multiple models are active; use {selector_command} and configure them in the TUI"
                )
            };
            return Ok(VariantOutcome::Rejected(message));
        }
        let available = client.available_thinking_variants();
        let variant = match resolve_variant_name(selected, &available) {
            Ok(variant) => variant,
            Err(message) => return Ok(VariantOutcome::Rejected(message)),
        };
        client.set_thinking_variant(variant)?;
    } else {
        let options = client.thinking_variant_options();
        let Some(selections) = inline_variant_select(&options)? else {
            return Ok(VariantOutcome::Cancelled);
        };
        client.set_thinking_variants(&selections)?;
    }

    client.save_thinking_variants(paths)?;
    Ok(VariantOutcome::Updated)
}

pub(in crate::cli) fn resolve_variant_name(
    selected: &str,
    available: &[String],
) -> std::result::Result<Option<String>, String> {
    let explicit_variant = selected.strip_prefix("variant:");
    if explicit_variant.is_none() && selected.eq_ignore_ascii_case("default") {
        return Ok(None);
    }
    let selected = explicit_variant.unwrap_or(selected);
    available
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(selected))
        .cloned()
        .map(Some)
        .ok_or_else(|| {
            format!(
                "{}: {selected}",
                t("unknown thinking variant", "未知思考档位")
            )
        })
}

pub(in crate::cli) fn print_variant_updated() {
    println!("{}\n", t("thinking variants updated", "已更新思考档位"));
}

/// Direct (daemon-less) sessions read their pinned model pool straight from
/// the state store; daemon-run turns get the same treatment in the turn task.
pub(in crate::cli) fn apply_session_model_override(state: &StateStore, config: &mut AppConfig) {
    match state.session_model_override(&state.session_id()) {
        Ok(Some(models)) => config.active_provider_models = Some(models),
        Ok(None) => {}
        Err(error) => tracing::warn!(
            error = %error,
            "{}",
            t(
                "loading the session model override failed",
                "读取会话模型覆盖失败"
            )
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli) struct VariantMenuItem {
    pub(in crate::cli) provider_id: String,
    pub(in crate::cli) model: String,
    pub(in crate::cli) options: Vec<VariantMenuOption>,
    pub(in crate::cli) selected: usize,
    pub(in crate::cli) cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli) struct VariantMenuOption {
    pub(in crate::cli) label: String,
    pub(in crate::cli) value: Option<String>,
}

impl VariantMenuItem {
    pub(in crate::cli) fn from_options(options: &ThinkingVariantOptions) -> Self {
        let mut variants = vec![VariantMenuOption {
            label: "default".to_string(),
            value: None,
        }];
        variants.extend(options.variants.iter().map(|variant| VariantMenuOption {
            label: if variant == "default" {
                "default (variant)".to_string()
            } else {
                variant.clone()
            },
            value: Some(variant.clone()),
        }));
        let selected = options
            .selected
            .as_ref()
            .and_then(|selected| {
                variants
                    .iter()
                    .position(|variant| variant.value.as_ref() == Some(selected))
            })
            .unwrap_or(0);
        Self {
            provider_id: options.provider_id.clone(),
            model: options.model.clone(),
            options: variants,
            selected,
            cursor: selected,
        }
    }

    pub(in crate::cli) fn selection(&self) -> (String, String, Option<String>) {
        (
            self.provider_id.clone(),
            self.model.clone(),
            self.options[self.selected].value.clone(),
        )
    }

    pub(in crate::cli) fn check_cursor(&mut self) {
        self.selected = self.cursor;
    }
}

pub(in crate::cli) fn inline_variant_select(
    options: &[ThinkingVariantOptions],
) -> Result<Option<Vec<(String, String, Option<String>)>>> {
    let mut items = options
        .iter()
        .map(VariantMenuItem::from_options)
        .collect::<Vec<_>>();
    if items.is_empty() {
        return Ok(None);
    }
    if items.len() == 1 {
        return inline_single_variant_select(items.remove(0));
    }
    let max_options = items
        .iter()
        .map(|item| item.options.len())
        .max()
        .unwrap_or(1);
    let menu_lines = inline_fuzzy_lines(items.len().max(max_options));
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let mut active_column = 0usize;
    let mut model_index = 0usize;
    let mut model_scroll = 0usize;
    let mut variant_scroll = 0usize;
    let (_, cursor_y) = cursor::position().unwrap_or((0, menu_lines.saturating_sub(1)));
    let anchor_y = cursor_y.saturating_sub(menu_lines.saturating_sub(1));
    loop {
        let visible = menu_lines.saturating_sub(2) as usize;
        model_scroll = inline_fuzzy_scroll(model_index, model_scroll, visible.min(items.len()));
        let item = &items[model_index];
        variant_scroll =
            inline_fuzzy_scroll(item.cursor, variant_scroll, visible.min(item.options.len()));
        draw_inline_variant(
            &mut session.stdout,
            anchor_y,
            menu_lines,
            &items,
            active_column,
            model_index,
            model_scroll,
            variant_scroll,
        )?;
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            match code {
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Enter => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(Some(items.iter().map(VariantMenuItem::selection).collect()));
                }
                KeyCode::Left | KeyCode::Char('h') => active_column = 0,
                KeyCode::Right | KeyCode::Char('l') => active_column = 1,
                KeyCode::Up | KeyCode::Char('k') if active_column == 0 => {
                    model_index = model_index.saturating_sub(1);
                    variant_scroll = 0;
                }
                KeyCode::Down | KeyCode::Char('j') if active_column == 0 => {
                    model_index = (model_index + 1).min(items.len() - 1);
                    variant_scroll = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    items[model_index].cursor = items[model_index].cursor.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let last = items[model_index].options.len() - 1;
                    items[model_index].cursor = (items[model_index].cursor + 1).min(last);
                }
                KeyCode::Tab if active_column == 1 => {
                    items[model_index].check_cursor();
                }
                _ => {}
            }
        }
    }
}

pub(in crate::cli) fn inline_single_variant_select(
    mut item: VariantMenuItem,
) -> Result<Option<Vec<(String, String, Option<String>)>>> {
    let menu_lines = inline_fuzzy_lines(item.options.len());
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let mut scroll = 0usize;
    let (_, cursor_y) = cursor::position().unwrap_or((0, menu_lines.saturating_sub(1)));
    let anchor_y = cursor_y.saturating_sub(menu_lines.saturating_sub(1));
    loop {
        let visible = menu_lines.saturating_sub(2) as usize;
        scroll = inline_fuzzy_scroll(item.cursor, scroll, visible.min(item.options.len()));
        draw_inline_single_variant(&mut session.stdout, anchor_y, menu_lines, &item, scroll)?;
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            match code {
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Enter => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(Some(vec![item.selection()]));
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    item.cursor = item.cursor.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    item.cursor = (item.cursor + 1).min(item.options.len() - 1);
                }
                KeyCode::Tab => item.check_cursor(),
                _ => {}
            }
        }
    }
}

pub(in crate::cli) fn draw_inline_single_variant(
    stdout: &mut io::Stdout,
    anchor_y: u16,
    menu_lines: u16,
    item: &VariantMenuItem,
    scroll: usize,
) -> Result<()> {
    let (cols, _) = terminal::size().unwrap_or((80, 24));
    let bar = inline_fuzzy_bar();
    let available = (cols as usize).saturating_sub(visible_width(&bar)).max(1);
    let width = single_variant_content_width(item).min(available);
    let visible = menu_lines.saturating_sub(2) as usize;
    queue!(stdout, Hide)?;
    for row in 0..menu_lines {
        queue!(
            stdout,
            MoveTo(0, anchor_y + row),
            Clear(ClearType::CurrentLine)
        )?;
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y),
        Print(&bar),
        Print(variant_menu_header(
            t("Thinking variant", "思考档位"),
            true,
            width,
        )),
    )?;
    for row in 0..visible {
        let index = scroll + row;
        let line = item.options.get(index).map_or_else(
            || " ".repeat(width),
            |variant| {
                variant_menu_cell(
                    &variant.label,
                    index == item.cursor,
                    index == item.cursor,
                    Some(index == item.selected),
                    width,
                )
            },
        );
        queue!(
            stdout,
            MoveTo(0, anchor_y + row as u16 + 1),
            Print(&bar),
            Print(line),
        )?;
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y + menu_lines.saturating_sub(1)),
        Print(&bar),
        Print(format!(
            "\x1b[2m{}\x1b[0m",
            truncate_visible_width(
                t(
                    "j/k move · Tab select · Enter confirm · Esc/q cancel",
                    "j/k 移动 · Tab 勾选 · Enter 确认 · Esc/q 取消"
                ),
                available,
            )
        ))
    )?;
    stdout.flush()?;
    Ok(())
}

pub(in crate::cli) fn single_variant_content_width(item: &VariantMenuItem) -> usize {
    item.options
        .iter()
        .map(|option| visible_width(&option.label).saturating_add(6))
        .chain(std::iter::once(visible_width(t(
            "Thinking variant",
            "思考档位",
        ))))
        .max()
        .unwrap_or(1)
}

pub(in crate::cli) fn draw_inline_variant(
    stdout: &mut io::Stdout,
    anchor_y: u16,
    menu_lines: u16,
    items: &[VariantMenuItem],
    active_column: usize,
    model_index: usize,
    model_scroll: usize,
    variant_scroll: usize,
) -> Result<()> {
    let (cols, _) = terminal::size().unwrap_or((80, 24));
    let bar = inline_fuzzy_bar();
    let width = (cols as usize).saturating_sub(visible_width(&bar)).max(1);
    let separator = if width >= 3 { " │ " } else { "" };
    let available = width.saturating_sub(visible_width(separator));
    let (left_width, right_width) = variant_menu_column_widths(items, available);
    let visible = menu_lines.saturating_sub(2) as usize;
    queue!(stdout, Hide)?;
    for row in 0..menu_lines {
        queue!(
            stdout,
            MoveTo(0, anchor_y + row),
            Clear(ClearType::CurrentLine)
        )?;
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y),
        Print(&bar),
        Print(variant_menu_header(
            t("Provider / Model", "Provider / 模型"),
            active_column == 0,
            left_width,
        )),
        Print(format!("\x1b[2m{separator}\x1b[0m")),
        Print(variant_menu_header(
            t("Thinking variant", "思考档位"),
            active_column == 1,
            right_width,
        )),
    )?;
    let variants = &items[model_index];
    for row in 0..visible {
        let left_index = model_scroll + row;
        let right_index = variant_scroll + row;
        let left = items.get(left_index).map_or_else(
            || " ".repeat(left_width),
            |item| {
                variant_menu_cell(
                    &format!("{} / {}", item.provider_id, item.model),
                    active_column == 0 && left_index == model_index,
                    left_index == model_index,
                    None,
                    left_width,
                )
            },
        );
        let right = variants.options.get(right_index).map_or_else(
            || " ".repeat(right_width),
            |variant| {
                variant_menu_cell(
                    &variant.label,
                    active_column == 1 && right_index == variants.cursor,
                    right_index == variants.cursor,
                    Some(right_index == variants.selected),
                    right_width,
                )
            },
        );
        queue!(
            stdout,
            MoveTo(0, anchor_y + row as u16 + 1),
            Print(&bar),
            Print(left),
            Print(format!("\x1b[2m{separator}\x1b[0m")),
            Print(right),
        )?;
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y + menu_lines.saturating_sub(1)),
        Print(&bar),
        Print(format!(
            "\x1b[2m{}\x1b[0m",
            truncate_visible_width(
                t(
                    "h/l switch · j/k move · Tab select · Enter confirm · Esc/q cancel",
                    "h/l 切栏 · j/k 移动 · Tab 勾选 · Enter 确认 · Esc/q 取消"
                ),
                width,
            )
        ))
    )?;
    stdout.flush()?;
    Ok(())
}

pub(in crate::cli) fn variant_menu_column_widths(
    items: &[VariantMenuItem],
    available: usize,
) -> (usize, usize) {
    if available == 0 {
        return (0, 0);
    }
    if available == 1 {
        return (1, 0);
    }
    let left_needed = items
        .iter()
        .map(|item| {
            visible_width(&format!("{} / {}", item.provider_id, item.model)).saturating_add(2)
        })
        .chain(std::iter::once(visible_width(t(
            "Provider / Model",
            "Provider / 模型",
        ))))
        .max()
        .unwrap_or(1);
    let right_needed = items
        .iter()
        .flat_map(|item| item.options.iter())
        .map(|option| visible_width(&option.label).saturating_add(6))
        .chain(std::iter::once(visible_width(t(
            "Thinking variant",
            "思考档位",
        ))))
        .max()
        .unwrap_or(1);
    if left_needed.saturating_add(right_needed) <= available {
        return (left_needed, right_needed);
    }
    let total_needed = left_needed.saturating_add(right_needed).max(1);
    let left = available
        .saturating_mul(left_needed)
        .saturating_div(total_needed)
        .clamp(1, available - 1);
    (left, available - left)
}

pub(in crate::cli) fn variant_menu_header(label: &str, active: bool, width: usize) -> String {
    let label = pad_visible_width(&truncate_visible_width(label, width), width);
    if active {
        format!("\x1b[1m\x1b[35m{label}\x1b[0m")
    } else {
        format!("\x1b[1m{label}\x1b[0m")
    }
}

pub(in crate::cli) fn variant_menu_cell(
    label: &str,
    focused: bool,
    highlighted: bool,
    checked: Option<bool>,
    width: usize,
) -> String {
    let marker = if highlighted { "›" } else { " " };
    let check = match checked {
        Some(true) => "[*] ",
        Some(false) => "[ ] ",
        None => "",
    };
    let line = pad_visible_width(
        &truncate_visible_width(&format!("{marker} {check}{label}"), width),
        width,
    );
    if focused {
        format!("\x1b[1m\x1b[35m{line}\x1b[0m")
    } else if checked == Some(true) {
        format!("\x1b[1m\x1b[32m{line}\x1b[0m")
    } else if highlighted {
        format!("\x1b[1m{line}\x1b[0m")
    } else {
        format!("\x1b[2m{line}\x1b[0m")
    }
}
