//! 记忆子系统的配置项。

use crate::config::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub evicted_context_enabled: bool,
    #[serde(default = "default_true")]
    pub association_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_diary_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_fact_enabled: bool,
    #[serde(default = "default_memory_diary_batch_size")]
    pub diary_batch_size: usize,
    #[serde(default = "default_memory_short_diary_retention_days")]
    pub short_diary_retention_days: u64,
    #[serde(default = "default_memory_diary_promotion_recalls")]
    pub diary_promotion_recalls: u64,
    #[serde(default = "default_memory_organizer_timeout_seconds")]
    pub organizer_timeout_seconds: u64,
    #[serde(default)]
    pub auto_skill_enabled: bool,
    #[serde(default = "default_memory_association_facts")]
    pub association_facts: usize,
    #[serde(default = "default_memory_association_episodes")]
    pub association_episodes: usize,
    #[serde(default = "default_memory_association_max_chars")]
    pub association_max_chars: usize,
    /// 单条联想记忆的正文上限（字符）。日记常把当时那条完整回复整段存进
    /// 去，实测一条 400+ 字符；截断后带 id，模型可用 recall_memories(id=)
    /// 取全文。0 = 不截断。
    #[serde(default = "default_memory_association_entry_chars")]
    pub association_entry_chars: usize,
    /// 同一条记忆若已在本会话早前回合注入过（化石仍在可见上下文中逐字回放），
    /// 本回合不再重复注入。内容或日期变化的记忆视为新条目照常注入。
    #[serde(default = "default_true")]
    pub association_dedup: bool,
    #[serde(default = "default_memory_snippet_chars")]
    pub snippet_chars: usize,
    #[serde(default = "default_memory_forget_after_days")]
    pub forget_after_days: u64,
    #[serde(default = "default_true")]
    pub forgetting_enabled: bool,
    #[serde(default = "default_memory_half_life_days")]
    pub forgetting_half_life_days: f64,
    #[serde(default = "default_memory_min_strength")]
    pub forgetting_min_strength: f64,
    #[serde(default = "default_memory_review_boost")]
    pub forgetting_review_boost: f64,
    #[serde(default = "default_memory_min_task_chars")]
    pub learning_min_task_chars: usize,
    #[serde(default = "default_memory_min_method_chars")]
    pub learning_min_method_chars: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            evicted_context_enabled: default_true(),
            association_enabled: default_true(),
            auto_diary_enabled: default_true(),
            auto_fact_enabled: default_true(),
            diary_batch_size: default_memory_diary_batch_size(),
            short_diary_retention_days: default_memory_short_diary_retention_days(),
            diary_promotion_recalls: default_memory_diary_promotion_recalls(),
            organizer_timeout_seconds: default_memory_organizer_timeout_seconds(),
            auto_skill_enabled: false,
            association_facts: default_memory_association_facts(),
            association_episodes: default_memory_association_episodes(),
            association_max_chars: default_memory_association_max_chars(),
            association_entry_chars: default_memory_association_entry_chars(),
            association_dedup: default_true(),
            snippet_chars: default_memory_snippet_chars(),
            forget_after_days: default_memory_forget_after_days(),
            forgetting_enabled: default_true(),
            forgetting_half_life_days: default_memory_half_life_days(),
            forgetting_min_strength: default_memory_min_strength(),
            forgetting_review_boost: default_memory_review_boost(),
            learning_min_task_chars: default_memory_min_task_chars(),
            learning_min_method_chars: default_memory_min_method_chars(),
        }
    }
}
