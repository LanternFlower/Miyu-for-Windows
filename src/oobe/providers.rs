//! 接模型那一屏的数据面：有哪些路可选、模型目录怎么拉、选完怎么写进配置。
//!
//! 三类路：借本机已登录的 CLI（claude / codex / agy，探到才列）、常用供应商
//! 预设（选一条 → 填 key → 拉目录 → 选模型；opencode Zen 免 key）、自定义供应商
//! （名字 / id / 地址 / 协议 / key）。CLI 的模型目录最长要 20 秒（`provider_catalog/cli.rs`
//! 的超时），所以探到 CLI 就**提前**在后台线程拉，等用户走到最后一屏时目录已经在手里。

use miyu_base::config::{ActiveProviderModelConfig, AppConfig, ProviderConfig};
use miyu_base::default_models::OPENCODE_PROVIDER_ID;
use miyu_base::paths::MiyuPaths;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};

/// 协议取值照 `config_tui/providers.rs` 那份下拉。**不能假定 OpenAI 兼容**——
/// Anthropic 的 Messages 协议是平级的一档。
pub(super) const PROTOCOLS: [(&str, &str); 4] = [
    ("auto", ""),
    ("openai-chat", "OpenAI Chat Completions"),
    ("openai-responses", "OpenAI Responses"),
    ("anthropic", "Anthropic Messages"),
];

/// 常用供应商预设：(配置 id, 显示名, 接口地址, 协议, 备注)。地址照各家官方文档
/// （2026-09）；分国内/海外的各列一条，id 后缀 `-global` 是海外站。
/// 配置里已经有同 id 的条目就沿用那条（保留用户改过的东西），只换 key。
pub(super) const PRESETS: &[(&str, &str, &str, &str, &str)] = &[
    (
        OPENCODE_PROVIDER_ID,
        "opencode Zen",
        "",
        "auto",
        "内置公共额度，免 key",
    ),
    ("opencodego", "OpenCode Go", "", "auto", "opencode 付费档"),
    (
        "openai",
        "OpenAI",
        "https://api.openai.com/v1",
        "auto",
        "api.openai.com",
    ),
    (
        "anthropic",
        "Anthropic",
        "https://api.anthropic.com/v1",
        "anthropic",
        "api.anthropic.com",
    ),
    (
        "deepseek",
        "DeepSeek",
        "https://api.deepseek.com",
        "auto",
        "api.deepseek.com",
    ),
    (
        "gemini",
        "Gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        "auto",
        "OpenAI 兼容口",
    ),
    (
        "openrouter",
        "OpenRouter",
        "https://openrouter.ai/api/v1",
        "auto",
        "openrouter.ai",
    ),
    (
        "xiaomi",
        "Xiaomi MiMo",
        "https://token-plan-sgp.xiaomimimo.com/v1",
        "auto",
        "xiaomimimo.com",
    ),
    (
        "zhipu",
        "智谱 GLM（国内）",
        "https://open.bigmodel.cn/api/paas/v4",
        "auto",
        "open.bigmodel.cn",
    ),
    (
        "zai",
        "Z.ai GLM（海外）",
        "https://api.z.ai/api/paas/v4",
        "auto",
        "api.z.ai",
    ),
    (
        "moonshot",
        "Kimi / Moonshot（国内）",
        "https://api.moonshot.cn/v1",
        "auto",
        "api.moonshot.cn",
    ),
    (
        "moonshot-global",
        "Kimi / Moonshot（海外）",
        "https://api.moonshot.ai/v1",
        "auto",
        "api.moonshot.ai",
    ),
    (
        "minimax",
        "MiniMax（国内）",
        "https://api.minimaxi.com/v1",
        "auto",
        "api.minimaxi.com",
    ),
    (
        "minimax-global",
        "MiniMax（海外）",
        "https://api.minimax.io/v1",
        "auto",
        "api.minimax.io",
    ),
    (
        "siliconflow",
        "硅基流动（国内）",
        "https://api.siliconflow.cn/v1",
        "auto",
        "api.siliconflow.cn",
    ),
    (
        "siliconflow-global",
        "SiliconFlow（海外）",
        "https://api.siliconflow.com/v1",
        "auto",
        "api.siliconflow.com",
    ),
];

#[derive(Clone)]
pub(super) enum Choice {
    /// 配置里已经有一条能用的：原样保留。
    Keep,
    /// 借本机 CLI 的登录态（claude / codex / agy）。
    Cli(ProviderConfig),
    /// 常用供应商预设。`needs_key` = 要先填 key（opencode Zen 不用）。
    Preset {
        provider: ProviderConfig,
        needs_key: bool,
    },
    /// 自己填一条。
    Custom,
    /// 逃生口：进完整设置界面。
    OpenSettings,
}

#[derive(Clone)]
pub(super) struct ProviderOption {
    pub label: String,
    pub note: String,
    pub choice: Choice,
    /// 分组名：列表里按它插分割线。
    pub section: &'static str,
}

/// 配置里当前这条供应商算不算「已经接好了」。默认配置的 opencode 没 key 也能
/// 用，但那是「还没选过」的状态，不算。
pub(super) fn configured_provider(config: &AppConfig) -> Option<&ProviderConfig> {
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == config.active_provider)?;
    let usable = if provider.is_builtin_cli_provider() {
        provider.enabled
    } else if provider.is_opencode_zen() {
        config
            .active_provider_models
            .as_ref()
            .is_some_and(|pool| pool.iter().any(|entry| entry.provider_id == provider.id))
    } else {
        provider
            .api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
    };
    usable.then_some(provider)
}

fn template_for(
    config: &AppConfig,
    matches: impl Fn(&ProviderConfig) -> bool,
    fallback: ProviderConfig,
) -> ProviderConfig {
    config
        .providers
        .iter()
        .find(|provider| matches(provider))
        .cloned()
        .unwrap_or(fallback)
}

/// 预设对应的供应商条目：配置里有同 id 的沿用，没有就按预设新建。
fn preset_provider(
    config: &AppConfig,
    id: &str,
    name: &str,
    url: &str,
    protocol: &str,
) -> ProviderConfig {
    if let Some(existing) = config.providers.iter().find(|provider| provider.id == id) {
        return existing.clone();
    }
    match id {
        OPENCODE_PROVIDER_ID => ProviderConfig::default_opencodezen(),
        "anthropic" => ProviderConfig::default_anthropic(),
        _ => {
            let mut provider = ProviderConfig::template(id, name, url);
            provider.protocol = protocol.to_string();
            provider
        }
    }
}

/// 按探到的 CLI 列出可选项。顺序：保留现有 → CLI → 预设 → 自定义 → 设置界面。
pub(super) fn options(config: &AppConfig, has_cli: impl Fn(&str) -> bool) -> Vec<ProviderOption> {
    let mut out = Vec::new();
    if let Some(provider) = configured_provider(config) {
        let model = current_model(config, provider);
        out.push(ProviderOption {
            label: format!("保留现在的 {}", provider.display_name),
            note: if model.is_empty() {
                "配置里已经接好".into()
            } else {
                model
            },
            choice: Choice::Keep,
            section: "现有",
        });
    }
    let clis: [(
        &str,
        &str,
        fn(&ProviderConfig) -> bool,
        fn() -> ProviderConfig,
    ); 3] = [
        (
            "claude",
            "借 Claude Code 的订阅",
            ProviderConfig::is_claude_code,
            ProviderConfig::claude_code_template,
        ),
        (
            "codex",
            "借 Codex 的订阅",
            ProviderConfig::is_codex,
            ProviderConfig::codex_template,
        ),
        (
            "agy",
            "借 Antigravity 的订阅",
            ProviderConfig::is_antigravity,
            ProviderConfig::antigravity_template,
        ),
    ];
    for (binary, label, matches, fallback) in clis {
        // 探到装了才列;没装的行摆出来只会让人去点。
        if !has_cli(binary) {
            continue;
        }
        out.push(ProviderOption {
            label: label.into(),
            note: "需已登录".into(),
            choice: Choice::Cli(template_for(config, matches, fallback())),
            section: "本机 CLI",
        });
    }
    for (id, name, url, protocol, note) in PRESETS {
        let provider = preset_provider(config, id, name, url, protocol);
        // opencode Zen 也进填 key 那一屏:那里有「用公共密钥的免费额度」的开关。
        let needs_key = true;
        out.push(ProviderOption {
            label: (*name).into(),
            note: (*note).into(),
            choice: Choice::Preset {
                provider,
                needs_key,
            },
            section: "供应商",
        });
    }
    out.push(ProviderOption {
        label: "自定义供应商".into(),
        note: "地址、协议、key 自己填".into(),
        choice: Choice::Custom,
        section: "其他",
    });
    out.push(ProviderOption {
        label: "进入设置界面".into(),
        note: String::new(),
        choice: Choice::OpenSettings,
        section: "其他",
    });
    out
}

fn current_model(config: &AppConfig, provider: &ProviderConfig) -> String {
    config
        .active_provider_models
        .as_ref()
        .and_then(|pool| {
            pool.iter()
                .find(|entry| entry.provider_id == provider.id)
                .map(|entry| entry.model.clone())
        })
        .unwrap_or_else(|| provider.default_model.clone())
}

/// 后台拉目录。`recv` 非阻塞，画面每帧问一次。
pub(super) struct CatalogJob {
    receiver: Receiver<Result<Vec<String>, String>>,
}

impl CatalogJob {
    pub fn spawn(config: &AppConfig, provider: ProviderConfig) -> Self {
        let binary = miyu_base::provider_catalog::builtin_cli_binary(config, &provider);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = miyu_base::provider_catalog::fetch_models(&provider, binary.as_deref())
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(result);
        });
        Self { receiver }
    }

    pub fn poll(&self) -> Option<Result<Vec<String>, String>> {
        self.receiver.try_recv().ok()
    }
}

/// 探到 CLI 就提前拉目录；opencode 也顺手拉（它要联网）。
#[derive(Default)]
pub(super) struct Prefetch {
    jobs: HashMap<String, CatalogJob>,
    results: HashMap<String, Result<Vec<String>, String>>,
}

impl Prefetch {
    pub fn start(&mut self, config: &AppConfig, provider: &ProviderConfig) {
        if self.jobs.contains_key(&provider.id) || self.results.contains_key(&provider.id) {
            return;
        }
        self.jobs.insert(
            provider.id.clone(),
            CatalogJob::spawn(config, provider.clone()),
        );
    }

    /// 收一收跑完的。
    pub fn pump(&mut self) {
        let done: Vec<String> = self
            .jobs
            .iter()
            .filter_map(|(id, job)| job.poll().map(|result| (id.clone(), result)))
            .map(|(id, result)| {
                self.results.insert(id.clone(), result);
                id
            })
            .collect();
        for id in done {
            self.jobs.remove(&id);
        }
    }

    pub fn take(&mut self, provider_id: &str) -> Option<Result<Vec<String>, String>> {
        self.results.remove(provider_id)
    }

    pub fn running(&self, provider_id: &str) -> bool {
        self.jobs.contains_key(provider_id)
    }
}

/// 把自己填的端点拼成一条供应商。字段照 `ProviderConfig`，少一样都写不进配置文件。
pub(super) fn custom_provider(
    name: &str,
    id: &str,
    url: &str,
    protocol: &str,
    api_key: &str,
) -> ProviderConfig {
    let mut provider = ProviderConfig::new_custom();
    provider.id = id.trim().to_string();
    provider.display_name = if name.trim().is_empty() {
        id.trim().to_string()
    } else {
        name.trim().to_string()
    };
    provider.base_url = url.trim().trim_end_matches('/').to_string();
    provider.protocol = if protocol.trim().is_empty() {
        "auto".into()
    } else {
        protocol.trim().to_string()
    };
    provider.api_key = (!api_key.trim().is_empty()).then(|| api_key.trim().to_string());
    provider
}

/// 这条供应商有没有「公共密钥」可用(opencode Zen 免 key 的免费额度)。
pub(super) fn public_quota_available(provider: &ProviderConfig) -> bool {
    provider.is_opencode_zen()
}

/// 给预设填上 key。
pub(super) fn with_key(mut provider: ProviderConfig, api_key: &str) -> ProviderConfig {
    provider.api_key = (!api_key.trim().is_empty()).then(|| api_key.trim().to_string());
    provider
}

/// 选定供应商与模型：写进 `providers`（同 id 替换）、激活、模型进「已激活」集合
/// 与池子、按 models.dev 目录补上下文窗口与模态——和设置界面里按 Tab 激活一个
/// 模型是同一套动作，设置界面里看到的勾选才对得上。
pub(super) fn apply(
    config: &mut AppConfig,
    paths: &MiyuPaths,
    mut provider: ProviderConfig,
    model: &str,
) {
    provider.enabled = true;
    let model = model.trim();
    if !model.is_empty() {
        if !provider.models.iter().any(|item| item == model) {
            provider.models.push(model.to_string());
        }
        miyu_base::provider_catalog::auto_configure_model_tags(paths, &mut provider, model);
        provider.default_model = model.to_string();
    }
    match config
        .providers
        .iter()
        .position(|existing| existing.id == provider.id)
    {
        Some(index) => config.providers[index] = provider.clone(),
        None => config.providers.push(provider.clone()),
    }
    config.active_provider = provider.id.clone();
    config.active_provider_models = (!model.is_empty()).then(|| {
        vec![ActiveProviderModelConfig {
            provider_id: provider.id.clone(),
            model: model.to_string(),
        }]
    });
}

/// 校验自定义供应商三个字段；返回第一条错误。
pub(super) fn validate_custom(id: &str, url: &str) -> Option<&'static str> {
    let id = id.trim();
    if id.is_empty()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Some("配置 ID 只能是小写字母、数字、连字符");
    }
    if PRESETS.iter().any(|(preset, ..)| *preset == id) {
        return Some("这个 ID 被内置预设占用了，换一个");
    }
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Some("接口地址要以 http:// 或 https:// 开头");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_default_config_lists_presets_and_no_keep() {
        let config = AppConfig::default();
        assert!(configured_provider(&config).is_none());
        let options = options(&config, |_| false);
        let Choice::Preset { provider, .. } = &options[0].choice else {
            panic!("expected preset")
        };
        assert!(public_quota_available(provider));
        assert!(options
            .iter()
            .any(|option| option.label.contains("DeepSeek")));
        assert!(options
            .iter()
            .any(|option| matches!(option.choice, Choice::Custom)));
        assert!(!options
            .iter()
            .any(|option| option.label.contains("Claude Code")));
    }

    #[test]
    fn detected_cli_becomes_option_and_apply_activates() {
        let mut config = AppConfig::default();
        let paths = miyu_base::paths::MiyuPaths::new().unwrap();
        let options = options(&config, |bin| bin == "claude");
        assert!(options[0].label.contains("Claude Code"));
        assert_eq!(options[0].note, "需已登录");
        let Choice::Cli(provider) = options[0].choice.clone() else {
            panic!("expected cli choice");
        };
        apply(&mut config, &paths, provider, "sonnet");
        assert_eq!(config.active_provider, "claude-code");
        let active = config
            .providers
            .iter()
            .find(|provider| provider.id == "claude-code")
            .unwrap();
        assert!(active.enabled);
        assert_eq!(active.default_model, "sonnet");
        assert!(active.models.iter().any(|model| model == "sonnet"));
        assert_eq!(
            config.active_provider_models.as_ref().unwrap()[0].model,
            "sonnet"
        );
        assert!(configured_provider(&config).is_some());
        assert!(matches!(
            super::options(&config, |_| false)[0].choice,
            Choice::Keep
        ));
    }

    #[test]
    fn preset_reuses_existing_entry_and_marks_model_active() {
        let mut config = AppConfig::default();
        let paths = miyu_base::paths::MiyuPaths::new().unwrap();
        let deepseek = options(&config, |_| false)
            .into_iter()
            .find(|option| option.label == "DeepSeek")
            .unwrap();
        let Choice::Preset {
            provider,
            needs_key,
        } = deepseek.choice
        else {
            panic!("expected preset");
        };
        assert!(needs_key);
        apply(
            &mut config,
            &paths,
            with_key(provider, "sk-x"),
            "deepseek-chat",
        );
        let saved = config
            .providers
            .iter()
            .find(|provider| provider.id == "deepseek")
            .unwrap();
        assert_eq!(saved.api_key.as_deref(), Some("sk-x"));
        assert!(saved.models.contains(&"deepseek-chat".to_string()));
        assert_eq!(
            config
                .providers
                .iter()
                .filter(|p| p.id == "deepseek")
                .count(),
            1
        );
    }

    #[test]
    fn custom_provider_validation() {
        assert!(validate_custom("my-relay", "https://x.example/v1").is_none());
        assert!(validate_custom("My Relay", "https://x").is_some());
        assert!(validate_custom("ok", "x.example").is_some());
        assert!(validate_custom("deepseek", "https://x").is_some());
        let provider = custom_provider("", "relay", "https://x/", "anthropic", " key ");
        assert_eq!(provider.display_name, "relay");
        assert_eq!(provider.base_url, "https://x");
        assert_eq!(provider.api_key.as_deref(), Some("key"));
        assert_eq!(provider.protocol, "anthropic");
    }
}
