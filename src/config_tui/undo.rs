//! 删除类操作的撤销栈。
//!
//! 存的是**整份 `AppConfig` 快照**，不是「反向操作」。删掉一个模型会连带清空
//! 它的模态、上下文窗口、`default_model`，以及它在文本池、多模态池、子代理档
//! 位、平台路由里的每一处引用（见 `AppConfig::remove_active_provider_model`）；
//! 删掉一个供应商牵扯更广。把这些逐一还原既啰嗦又容易漏一处，而快照在构造上
//! 就不会漏。
//!
//! 代价是每次删除多拷一份配置。配置是 KB 级的，而删除是低频的人工操作，这个
//! 代价可以忽略；`MAX_DEPTH` 兜住长时间编辑下的累积。
//!
//! 配置只在主菜单统一保存（见 [`super::run_main_menu`]），所以撤销全在内存里
//! 完成，不碰磁盘——按 `q` 不保存退出，删除和撤销一起作废，符合直觉。

use crate::config::AppConfig;
use crate::i18n::text as t;

/// 最多记住多少步。够一次编辑会话里反复试错，又不会让内存无限涨。
const MAX_DEPTH: usize = 20;

#[derive(Default)]
pub(in crate::config_tui) struct ConfigUndo {
    snapshots: Vec<AppConfig>,
}

impl ConfigUndo {
    /// 在**动手之前**调用：把当前状态存下来。
    pub(in crate::config_tui) fn record(&mut self, config: &AppConfig) {
        if self.snapshots.len() == MAX_DEPTH {
            self.snapshots.remove(0);
        }
        self.snapshots.push(config.clone());
    }

    /// 回到上一步。没有可回退的返回 `false`，调用方据此决定要不要提示。
    pub(in crate::config_tui) fn undo(&mut self, config: &mut AppConfig) -> bool {
        match self.snapshots.pop() {
            Some(snapshot) => {
                *config = snapshot;
                true
            }
            None => false,
        }
    }

    pub(in crate::config_tui) fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// 帮助行里的撤销提示。没有可撤销的就不显示——键位提示只列当下真能按的。
    pub(in crate::config_tui) fn hint(&self) -> &'static str {
        if self.is_empty() {
            ""
        } else {
            t(" [u]undo", " [u]撤销")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(models: &[&str]) -> AppConfig {
        let mut config = AppConfig::default();
        let provider = &mut config.providers[0];
        provider.models = models.iter().map(|model| model.to_string()).collect();
        config
    }

    /// 分步：连删三个再连按三次 u，一次退一步，中间每步都对得上。
    #[test]
    fn undo_steps_back_one_deletion_at_a_time() {
        let mut config = config_with(&["a", "b", "c"]);
        let provider_id = config.providers[0].id.clone();
        let mut undo = ConfigUndo::default();

        for model in ["a", "b", "c"] {
            undo.record(&config);
            config
                .remove_active_provider_model(&provider_id, model)
                .unwrap();
        }
        assert!(config.providers[0].models.is_empty());

        assert!(undo.undo(&mut config));
        assert_eq!(config.providers[0].models, ["c"]);
        assert!(undo.undo(&mut config));
        assert_eq!(config.providers[0].models, ["b", "c"]);
        assert!(undo.undo(&mut config));
        assert_eq!(config.providers[0].models, ["a", "b", "c"]);

        // 退完了就退不动了，不能 panic 也不能把配置改坏
        assert!(!undo.undo(&mut config));
        assert_eq!(config.providers[0].models, ["a", "b", "c"]);
    }

    /// 撤销要把删除的**连带影响**一起还原，而不只是模型列表本身。
    #[test]
    fn undo_restores_the_references_the_deletion_cleared() {
        let mut config = config_with(&["keep", "gone"]);
        let provider_id = config.providers[0].id.clone();
        for model in ["keep", "gone"] {
            config.providers[0].model_modalities.insert(
                model.to_string(),
                vec!["text".to_string(), "image".to_string()],
            );
        }
        config
            .toggle_active_multimodal_provider_model(&provider_id, "gone")
            .unwrap();

        let mut undo = ConfigUndo::default();
        undo.record(&config);
        config
            .remove_active_provider_model(&provider_id, "gone")
            .unwrap();
        assert!(!config.is_active_multimodal_provider_model(&provider_id, "gone"));
        assert!(!config.providers[0].model_modalities.contains_key("gone"));

        assert!(undo.undo(&mut config));
        assert!(config.is_active_multimodal_provider_model(&provider_id, "gone"));
        assert!(config.providers[0].model_modalities.contains_key("gone"));
    }

    /// 深度有上限，最老的那步被丢掉，不能无限吃内存。
    #[test]
    fn the_stack_is_bounded() {
        let mut config = config_with(&["a"]);
        let mut undo = ConfigUndo::default();
        for _ in 0..MAX_DEPTH + 5 {
            undo.record(&config);
        }
        assert_eq!(undo.snapshots.len(), MAX_DEPTH);
        for _ in 0..MAX_DEPTH {
            assert!(undo.undo(&mut config));
        }
        assert!(!undo.undo(&mut config));
    }

    /// 没得撤销时不显示提示——键位提示只列当下真能按的。
    #[test]
    fn the_hint_only_shows_when_something_can_be_undone() {
        let config = AppConfig::default();
        let mut undo = ConfigUndo::default();
        assert!(undo.hint().is_empty());
        undo.record(&config);
        assert!(!undo.hint().is_empty());
    }
}
