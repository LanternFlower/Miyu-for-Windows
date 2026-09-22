//! 首次运行的初始化与人格挑选。
//!
//! `run_init` 是新用户的第一段体验，所以每一步都打印做了什么
//! （`print_init_step`）——静默创建一堆目录会让人不知道东西去哪了。

use crate::cli::*;

#[derive(Clone, Copy)]
pub(in crate::cli) enum InitKind {
    FirstRun,
    Explicit,
    /// 紧接着要进引导/全屏画面:一个字都不打,免得留在屏上。
    Quiet,
}

pub(in crate::cli) fn run_init(paths: &MiyuPaths, kind: InitKind) -> Result<()> {
    let quiet = matches!(kind, InitKind::Quiet);
    let interactive = !quiet && io::stdin().is_terminal() && io::stdout().is_terminal();
    if interactive {
        println!(
            "{}\n",
            match kind {
                InitKind::FirstRun | InitKind::Quiet => t("Miyu first start", "Miyu 首次启动"),
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
    if miyu_engine::default_kb::bundled_available() {
        print_init_step(
            interactive,
            t("Importing default knowledge base", "正在导入默认知识库"),
            &paths.data_dir.join("kb").display().to_string(),
        )?;
        if let Err(err) = miyu_engine::default_kb::ensure_initialized(paths, &config) {
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
    } else if !quiet {
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

pub(in crate::cli) fn list_persona_files(
    paths: &MiyuPaths,
    config: &AppConfig,
) -> Result<Vec<String>> {
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

/// /persona 的数据：人格文件清单、当前人格、配置（改完要存回去）。
///
/// 行内（`inline_fuzzy_select_single`）与全屏面板（`pick_single`）共用：菜单项、
/// 名字解析、落盘都在这里，两条路一个规矩。
pub(in crate::cli) struct PersonaChoices {
    config: AppConfig,
    pub(in crate::cli) personas: Vec<String>,
    pub(in crate::cli) current: String,
}

impl PersonaChoices {
    pub(in crate::cli) fn load(paths: &MiyuPaths) -> Result<Self> {
        let config = AppConfig::load(paths)?;
        let personas = list_persona_files(paths, &config)?;
        let current = config.prompt.active_persona.trim().to_string();
        Ok(Self {
            config,
            personas,
            current,
        })
    }

    /// 按名字找：`default` / `miyu` / `内置` 是内置默认（空串）；否则按文件名匹配
    /// （不分大小写、可省 `.md`、可只写一截）。
    pub(in crate::cli) fn resolve(&self, argument: &str) -> Result<String> {
        let argument = argument.trim();
        if argument.eq_ignore_ascii_case("default")
            || argument.eq_ignore_ascii_case("miyu")
            || argument == "内置"
        {
            return Ok(String::new());
        }
        let needle = argument.to_ascii_lowercase();
        self.personas
            .iter()
            .find(|name| {
                name.eq_ignore_ascii_case(argument)
                    || name
                        .to_ascii_lowercase()
                        .trim_end_matches(".md")
                        .contains(needle.trim_end_matches(".md"))
            })
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{}: {argument}",
                    t("no persona file matches", "没有匹配的人格文件")
                )
            })
    }

    /// 菜单项（第一项是内置默认）与光标初始位置（当前人格）。
    pub(in crate::cli) fn menu(&self) -> (Vec<String>, usize) {
        let mut items = vec![t(DEFAULT_PERSONA_LABEL_EN, DEFAULT_PERSONA_LABEL_ZH).to_string()];
        items.extend(self.personas.iter().cloned());
        let initial = if self.current.is_empty() {
            0
        } else {
            self.personas
                .iter()
                .position(|name| *name == self.current)
                .map(|index| index + 1)
                .unwrap_or(0)
        };
        (items, initial)
    }

    /// 菜单第 `index` 项对应的人格；0 是内置默认（空串）。
    pub(in crate::cli) fn at_menu_index(&self, index: usize) -> Option<String> {
        if index == 0 {
            Some(String::new())
        } else {
            self.personas.get(index - 1).cloned()
        }
    }

    /// 不带参数、又不在终端里时的清单文本。
    pub(in crate::cli) fn listing(&self) -> String {
        let mut out = format!(
            "{}: {}
",
            t("current persona", "当前人格"),
            self.label_of(&self.current)
        );
        for name in &self.personas {
            out.push_str(&format!(
                "  {name}
"
            ));
        }
        out.push_str(&format!(
            "{}
",
            t("switch with: /persona <name>", "切换：/persona <名称>")
        ));
        out
    }

    fn label_of(&self, persona: &str) -> String {
        if persona.is_empty() {
            t(DEFAULT_PERSONA_LABEL_EN, DEFAULT_PERSONA_LABEL_ZH).to_string()
        } else {
            persona.to_string()
        }
    }

    /// 落盘。返回（改了没, 给用户的一句话）。
    pub(in crate::cli) fn apply(
        mut self,
        paths: &MiyuPaths,
        target: String,
    ) -> Result<(bool, String)> {
        if target == self.current {
            return Ok((false, t("no changes", "未做修改").to_string()));
        }
        self.config.prompt.active_persona = target.clone();
        self.config.save(paths)?;
        Ok((
            true,
            format!(
                "{}: {}",
                t("active persona", "当前人格"),
                self.label_of(&target)
            ),
        ))
    }
}

/// Interactive persona picker (single-select). Returns true when the active
/// persona changed and the config was saved.
pub(in crate::cli) fn run_persona_picker(paths: &MiyuPaths, argument: &str) -> Result<bool> {
    let choices = PersonaChoices::load(paths)?;
    let argument = argument.trim();
    let target = if !argument.is_empty() {
        choices.resolve(argument)?
    } else if io::stdout().is_terminal() && io::stdin().is_terminal() {
        let (items, initial) = choices.menu();
        let Some(target) = inline_fuzzy_select_single(&items, initial)?
            .and_then(|index| choices.at_menu_index(index))
        else {
            return Ok(false);
        };
        target
    } else {
        print!("{}", choices.listing());
        return Ok(false);
    };
    let (changed, message) = choices.apply(paths, target)?;
    println!("{message}");
    Ok(changed)
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
