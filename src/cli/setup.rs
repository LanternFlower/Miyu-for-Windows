//! 首次运行的初始化与人格挑选。
//!
//! `run_init` 是新用户的第一段体验，所以每一步都打印做了什么
//! （`print_init_step`）——静默创建一堆目录会让人不知道东西去哪了。

use crate::cli::*;

#[derive(Clone, Copy)]
pub(in crate::cli) enum InitKind {
    FirstRun,
    Explicit,
}

pub(in crate::cli) fn run_init(paths: &MiyuPaths, kind: InitKind) -> Result<()> {
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    if interactive {
        println!(
            "{}\n",
            match kind {
                InitKind::FirstRun => t("Miyu first start", "Miyu 首次启动"),
                InitKind::Explicit => t("Miyu initialization", "Miyu 初始化"),
            }
        );
    }
    print_init_step(
        interactive,
        t("Preparing config directory", "正在准备配置目录"),
        &paths.config_dir.display().to_string(),
    )?;
    AppConfig::init_files(paths)?;
    print_init_step(
        interactive,
        t("Writing default config", "正在写入默认配置"),
        &paths.config_file.display().to_string(),
    )?;
    print_init_step(
        interactive,
        t("Creating state files", "正在创建状态文件"),
        &paths.state_dir.display().to_string(),
    )?;
    StateStore::new(paths)?.init_files()?;
    let config = AppConfig::load_or_default(paths)?;
    if crate::default_kb::bundled_available() {
        print_init_step(
            interactive,
            t("Importing default knowledge base", "正在导入默认知识库"),
            &paths.data_dir.join("kb").display().to_string(),
        )?;
        if let Err(err) = crate::default_kb::ensure_initialized(paths, &config) {
            if interactive {
                eprintln!(
                    "{}: {err}",
                    t(
                        "default knowledge base import skipped",
                        "默认知识库导入已跳过"
                    )
                );
            }
        }
    }
    print_init_step(
        interactive,
        t("Preparing data directory", "正在准备数据目录"),
        &paths.data_dir.display().to_string(),
    )?;
    if interactive {
        println!("\n{}\n", t("Initialization complete.", "初始化完成。"));
    } else {
        println!(
            "{} {}",
            t("initialized Miyu at", "Miyu 已初始化于"),
            paths.config_dir.display()
        );
    }
    Ok(())
}

pub(in crate::cli) fn print_init_step(interactive: bool, label: &str, value: &str) -> Result<()> {
    if interactive {
        std::thread::sleep(Duration::from_millis(180));
        println!("  {label:<24} ✓ {value}");
        io::stdout().flush()?;
    }
    Ok(())
}

pub(in crate::cli) fn terminal_bell_fallback() {
    for _ in 0..5 {
        let _ = std::io::stderr().write_all(b"\x07");
        let _ = std::io::stderr().flush();
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub(in crate::cli) const DEFAULT_PERSONA_LABEL_ZH: &str = "Miyu（内置默认）";

pub(in crate::cli) const DEFAULT_PERSONA_LABEL_EN: &str = "Miyu (built-in default)";

pub(in crate::cli) fn list_persona_files(paths: &MiyuPaths, config: &AppConfig) -> Result<Vec<String>> {
    let dir = config.prompts_dir_path(paths);
    let mut names = Vec::new();
    if dir.exists() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".md") && !name.eq_ignore_ascii_case("system-prompt.md") {
                    names.push(name);
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Interactive persona picker (single-select). Returns true when the active
/// persona changed and the config was saved.
pub(in crate::cli) fn run_persona_picker(paths: &MiyuPaths, argument: &str) -> Result<bool> {
    let mut config = AppConfig::load(paths)?;
    let personas = list_persona_files(paths, &config)?;
    let current = config.prompt.active_persona.trim().to_string();
    let argument = argument.trim();
    let chosen: Option<String> = if !argument.is_empty() {
        if argument.eq_ignore_ascii_case("default")
            || argument.eq_ignore_ascii_case("miyu")
            || argument == "内置"
        {
            Some(String::new())
        } else {
            let needle = argument.to_ascii_lowercase();
            let matched = personas.iter().find(|name| {
                name.eq_ignore_ascii_case(argument)
                    || name
                        .to_ascii_lowercase()
                        .trim_end_matches(".md")
                        .contains(needle.trim_end_matches(".md"))
            });
            match matched {
                Some(name) => Some(name.clone()),
                None => bail!(
                    "{}: {argument}",
                    t("no persona file matches", "没有匹配的人格文件")
                ),
            }
        }
    } else if io::stdout().is_terminal() && io::stdin().is_terminal() {
        let default_label = t(DEFAULT_PERSONA_LABEL_EN, DEFAULT_PERSONA_LABEL_ZH).to_string();
        let mut items = vec![default_label];
        items.extend(personas.iter().cloned());
        let initial = if current.is_empty() {
            0
        } else {
            personas
                .iter()
                .position(|name| *name == current)
                .map(|index| index + 1)
                .unwrap_or(0)
        };
        match inline_fuzzy_select_single(&items, initial)? {
            Some(0) => Some(String::new()),
            Some(index) => personas.get(index - 1).cloned(),
            None => None,
        }
    } else {
        println!(
            "{}: {}",
            t("current persona", "当前人格"),
            if current.is_empty() {
                t(DEFAULT_PERSONA_LABEL_EN, DEFAULT_PERSONA_LABEL_ZH).to_string()
            } else {
                current.clone()
            }
        );
        for name in &personas {
            println!("  {name}");
        }
        println!("{}", t("switch with: /persona <name>", "切换：/persona <名称>"));
        return Ok(false);
    };
    let Some(target) = chosen else {
        return Ok(false);
    };
    if target == current {
        println!("{}", t("no changes", "未做修改"));
        return Ok(false);
    }
    config.prompt.active_persona = target.clone();
    config.save(paths)?;
    println!(
        "{}: {}",
        t("active persona", "当前人格"),
        if target.is_empty() {
            t(DEFAULT_PERSONA_LABEL_EN, DEFAULT_PERSONA_LABEL_ZH).to_string()
        } else {
            target
        }
    );
    Ok(true)
}

pub(in crate::cli) async fn run_config(paths: &MiyuPaths, args: ConfigArgs) -> Result<bool> {
    match args.command {
        Some(ConfigCommand::Validate) => {
            AppConfig::load(paths)?;
            println!(
                "{}: {}",
                t("config is valid", "配置有效"),
                paths.config_file.display()
            );
            Ok(false)
        }
        Some(ConfigCommand::Paths) => {
            paths.print();
            Ok(false)
        }
        Some(ConfigCommand::PromptSource) => {
            let config = AppConfig::load(paths)?;
            let persona = config.prompt.active_persona.trim();
            let identity = config.prompt.active_identity.trim();
            let persona_path = (!persona.is_empty()).then(|| config.persona_path(paths, persona));
            let legacy_prompt = config.custom_system_prompt(paths)?;
            let legacy_prompt_path = config.system_prompt_path(paths);
            let base_prompt_source =
                if let Some(path) = persona_path.as_ref().filter(|path| path.exists()) {
                    format!("persona ({})", path.display())
                } else if !legacy_prompt.trim().is_empty() {
                    format!("legacy_custom ({})", legacy_prompt_path.display())
                } else {
                    "built-in".to_string()
                };
            println!("base_prompt_source: {}", base_prompt_source);
            println!(
                "active_persona: {}",
                if persona.is_empty() {
                    "(none)"
                } else {
                    persona
                }
            );
            if let Some(path) = persona_path {
                println!("active_persona_file: {}", path.display());
            }
            println!(
                "active_identity: {}",
                if identity.is_empty() {
                    "(none)"
                } else {
                    identity
                }
            );
            println!("prompts_dir: {}", config.prompts_dir_path(paths).display());
            println!(
                "identities_dir: {}",
                config.identities_dir_path(paths).display()
            );
            let system_prompt = config.system_prompt(paths)?;
            println!(
                "system_prompt_first_line: {}",
                system_prompt.lines().next().unwrap_or("")
            );
            println!("system_prompt_chars: {}", system_prompt.chars().count());
            Ok(false)
        }
        None => crate::config_tui::run(paths),
    }
}
