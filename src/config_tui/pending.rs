//! 不在 `config.jsonc` 里、但要跟着「保存并退出」一起落盘的改动。
//!
//! 人格清单（`persona.toml`）与开发模式提示词（`dev-prompt.md`）是各自独立的
//! 文件，2026-09-20 起攒在这里：改完只进内存，按「保存并退出」（或退出时那句
//! 「保存吗」答是）才一起写盘，答否就整份丢掉——和设置界面其余部分同一个口径
//! （在这之前它们是改完即写，用户指出这跟别处不一样）。
//!
//! 读的那一侧也得走这里：功能表二进二出、人格菜单上的计数，都要看得见这一轮
//! 还没写盘的改动，否则退出去再进来会看到旧的。

use crate::config_tui::*;
use miyu_base::config::PersonaManifest;
use std::collections::BTreeMap;

#[derive(Default)]
pub(in crate::config_tui) struct PendingWrites {
    /// 人格 scope → 待写的清单。切过人格就可能攒下不止一份。
    manifests: BTreeMap<String, PersonaManifest>,
    /// 开发模式提示词的正文；空串 = 清空（落盘时删文件，回退内置默认）。
    dev_prompt: Option<String>,
}

impl PendingWrites {
    pub(in crate::config_tui) fn is_empty(&self) -> bool {
        self.manifests.is_empty() && self.dev_prompt.is_none()
    }

    /// 这一层人格此刻的清单：先看这一轮改过没有，没有再读盘。
    pub(in crate::config_tui) fn manifest(
        &self,
        config: &AppConfig,
        paths: &MiyuPaths,
        scope: &str,
    ) -> PersonaManifest {
        self.manifests
            .get(scope)
            .cloned()
            .unwrap_or_else(|| PersonaManifest::load(config, paths, scope))
    }

    pub(in crate::config_tui) fn set_manifest(&mut self, scope: &str, manifest: PersonaManifest) {
        self.manifests.insert(scope.to_string(), manifest);
    }

    /// 开发模式提示词此刻的正文。
    pub(in crate::config_tui) fn dev_prompt(&self, paths: &MiyuPaths) -> String {
        self.dev_prompt.clone().unwrap_or_else(|| {
            std::fs::read_to_string(paths.config_dir.join(miyu_base::config::DEV_PROMPT_FILE))
                .unwrap_or_default()
        })
    }

    pub(in crate::config_tui) fn set_dev_prompt(&mut self, text: String) {
        self.dev_prompt = Some(text);
    }

    /// 全部落盘。配置本身已经存过了，这里只管那几个独立文件。
    pub(in crate::config_tui) fn flush(
        &mut self,
        config: &AppConfig,
        paths: &MiyuPaths,
    ) -> Result<()> {
        for (scope, manifest) in std::mem::take(&mut self.manifests) {
            let path = PersonaManifest::manifest_path(config, paths, &scope);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, manifest.to_toml())?;
        }
        if let Some(text) = self.dev_prompt.take() {
            let path = paths.config_dir.join(miyu_base::config::DEV_PROMPT_FILE);
            let text = text.trim();
            if text.is_empty() {
                if path.exists() {
                    std::fs::remove_file(&path)?;
                }
            } else {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, format!("{text}\n"))?;
            }
        }
        Ok(())
    }
}
