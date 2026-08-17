//! 重复调用提醒(dsh repeat-tool-reminder 同款语义,advisory-only)。
//!
//! 监视工具调用流,统计「同工具+同规范化参数」的连续次数,达到阈值时注入
//! 一段提醒打断死循环。只提醒、绝不否决或改写调用——是换参数重试、继续
//! 取证还是收尾,仍由模型决定;合理的重复既不延迟也不受阻。
//!
//! 链语义(照 dsh 实现移植):
//! - 链键 = (工具名, 深排序后的规范化参数);仅属性顺序不同视为相同。
//! - 被排除的工具(todo 类)对链**透明**:既不计数也不重置——循环里穿插
//!   记录类调用不能掩盖循环。
//! - 被拒绝/超时的调用也计数:反复撞同一个拒绝正是最需要打断的。
//! - 人类新输入(新回合、排队插话)重置链:语境变了,跨语境的重复不算循环。
//! - 纯内存态,重启清零;提醒是启发式而非日志不变量。

use serde_json::Value;
use std::collections::BTreeMap;

/// 触发提醒的连续次数(升序;首个=温和版,其余=详细版)。
const THRESHOLDS: [u32; 3] = [3, 5, 8];

/// 对链透明的工具:记录类调用不参与也不打断计数。
const TRANSPARENT: [&str; 2] = ["todowrite", "todoupdate"];

/// 详细提醒里引用的参数预览上限(检测始终用全量串,只截提醒文本)。
const ARGUMENTS_PREVIEW_CHARS: usize = 500;

/// 一个会话的连续重复链:上一条受跟踪调用的键与连续次数。
#[derive(Debug, Default, Clone)]
pub struct RepeatChain {
    key: Option<String>,
    count: u32,
}

impl RepeatChain {
    /// 人类新输入到达:语境变化,链重置。
    pub fn reset(&mut self) {
        self.key = None;
        self.count = 0;
    }

    /// 观察一次调用(在结果结算处调用,无论成败)。达到阈值返回应注入的
    /// 提醒文本;透明工具返回 None 且不改变链。
    pub fn observe(&mut self, tool_name: &str, raw_arguments: &str) -> Option<String> {
        if TRANSPARENT.contains(&tool_name) {
            return None;
        }
        let canonical = canonical_arguments(raw_arguments);
        let key = format!("{tool_name}\u{0}{canonical}");
        if self.key.as_deref() == Some(key.as_str()) {
            self.count += 1;
        } else {
            self.key = Some(key);
            self.count = 1;
        }
        if !THRESHOLDS.contains(&self.count) {
            return None;
        }
        Some(if self.count == THRESHOLDS[0] {
            gentle_reminder().to_string()
        } else {
            detailed_reminder(tool_name, self.count, &canonical)
        })
    }
}

/// 深排序键后的规范化参数串:仅属性顺序不同的对象归一;非法 JSON 原样
/// 兜底(dsh 同款)。
fn canonical_arguments(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => sort_json(&value).to_string(),
        Err(_) => raw.to_string(),
    }
}

fn sort_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sort_json).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), sort_json(item)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        other => other.clone(),
    }
}

fn gentle_reminder() -> &'static str {
    crate::i18n::agent_text(
        "You are repeating the exact same tool call with identical arguments. \
         Carefully analyze the previous result before calling again: if the task \
         is not complete, try a different approach or different arguments instead \
         of repeating the call.",
        "你正在用完全相同的参数重复同一个工具调用。再次调用前请仔细分析上一次\
         的结果:如果任务还没完成,换一种思路或换参数,而不是原样重发。",
    )
}

fn detailed_reminder(tool_name: &str, count: u32, canonical: &str) -> String {
    let preview: String = if canonical.chars().count() <= ARGUMENTS_PREVIEW_CHARS {
        canonical.to_string()
    } else {
        let head: String = canonical.chars().take(ARGUMENTS_PREVIEW_CHARS).collect();
        let omitted = canonical.chars().count() - ARGUMENTS_PREVIEW_CHARS;
        format!("{head}… (+{omitted} more chars)")
    };
    if crate::i18n::agent_is_zh() {
        format!(
            "检测到重复的工具调用:\n- 工具: {tool_name}\n- 连续次数: {count}\n- 参数: {preview}\n\
             这些重复调用没有取得进展。不要再用这组参数调用这个工具了;检查最新结果,\
             换一个动作、换一组参数,或者证据已够就直接收尾。"
        )
    } else {
        format!(
            "Repeated tool call detected:\n- tool: {tool_name}\n- consecutive_calls: {count}\n- arguments: {preview}\n\
             The repeated calls are not making progress. Do not call this tool with these \
             exact arguments again. Inspect the latest result and choose a different action, \
             different arguments, or finish the task if enough evidence has been gathered."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_fire_gentle_then_detailed_and_go_silent() {
        let mut chain = RepeatChain::default();
        let mut fired = Vec::new();
        for round in 1..=9u32 {
            if let Some(text) = chain.observe("grep", r#"{"pattern":"x"}"#) {
                fired.push((round, text));
            }
        }
        let rounds: Vec<u32> = fired.iter().map(|(round, _)| *round).collect();
        assert_eq!(rounds, vec![3, 5, 8]);
        assert!(!fired[0].1.contains("grep"), "温和版不点名工具");
        assert!(fired[1].1.contains("grep") && fired[1].1.contains('5'));
        // 超过最高阈值后静默(第 9 次没有触发)。
    }

    #[test]
    fn different_arguments_reset_the_run_and_key_order_is_canonical() {
        let mut chain = RepeatChain::default();
        assert!(chain.observe("grep", r#"{"a":1,"b":2}"#).is_none());
        // 仅属性顺序不同 → 视为相同,计数 2。
        assert!(chain.observe("grep", r#"{"b":2,"a":1}"#).is_none());
        assert!(chain.observe("grep", r#"{"a":1,"b":2}"#).is_some(), "第 3 次触发");
        // 换参数 → 链重开。
        assert!(chain.observe("grep", r#"{"a":9}"#).is_none());
        assert!(chain.observe("grep", r#"{"a":9}"#).is_none());
        assert!(chain.observe("grep", r#"{"a":9}"#).is_some());
    }

    #[test]
    fn transparent_tools_neither_count_nor_reset() {
        let mut chain = RepeatChain::default();
        assert!(chain.observe("grep", "{}").is_none());
        assert!(chain.observe("todowrite", r#"{"todos":[]}"#).is_none());
        assert!(chain.observe("grep", "{}").is_none());
        // 穿插 todowrite 不断链:这已是连续第 3 次 grep。
        assert!(chain.observe("grep", "{}").is_some());
    }

    #[test]
    fn human_reset_clears_the_run() {
        let mut chain = RepeatChain::default();
        chain.observe("grep", "{}");
        chain.observe("grep", "{}");
        chain.reset();
        assert!(chain.observe("grep", "{}").is_none());
        assert!(chain.observe("grep", "{}").is_none());
        assert!(chain.observe("grep", "{}").is_some());
    }

    #[test]
    fn detailed_preview_is_head_truncated_with_omission_marker() {
        let mut chain = RepeatChain::default();
        let long = format!(r#"{{"content":"{}"}}"#, "x".repeat(2000));
        chain.observe("write_file", &long);
        chain.observe("write_file", &long);
        let text = chain.observe("write_file", &long);
        // 阈值 3 是温和版,不带参数;推到 5 拿详细版。
        assert!(text.is_some());
        chain.observe("write_file", &long);
        let detailed = chain.observe("write_file", &long).unwrap();
        assert!(detailed.contains("more chars") || detailed.contains("… (+"));
    }
}
