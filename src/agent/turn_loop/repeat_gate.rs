//! 回合内同参工具调用复读闸(08-23,08-24 二版)。
//!
//! 端点故障窗口会返回逐字节相同的补全(温度 0.6 下不可能是诚实采样;
//! 08-24 排除法:请求组装/模型能力/上下文规模全无罪),模型侧表现为每轮
//! 发出相同工具调用(线上实录 32×vision_analyze / 27×web_search)。执行侧
//! 收口:连续第三个相同轮起不再真执行,**回灌上一轮的真实结果**(字节一
//! 致,不注入任何指令文本——一版的英文错误提示实测无用,故障态模型看不
//! 见输入增量);到保险丝阈值后收走工具,逼模型用已有结果正常成文,全
//! 程不产警告文本。
//!
//! 判定必须是「连续且整轮相同」:同一回合里 edit→build→edit→build 的重复
//! build 是正当流程,中间隔了别的调用就会重置计数,不会被误伤。

use crate::llm::ToolCall;

/// 连续相同轮达到该次数(即第 3 个相同轮)起,跳过执行回灌缓存结果。
pub(in crate::agent) const REPEAT_SKIP_THRESHOLD: u32 = 2;
/// 连续相同轮达到该次数(即第 6 个相同轮)起,强制收束工具循环。
pub(in crate::agent) const REPEAT_FUSE_THRESHOLD: u32 = 5;

#[derive(Default)]
pub(in crate::agent) struct ToolRepeatGate {
    previous: Option<Vec<(String, String)>>,
    consecutive: u32,
    /// 上一轮各调用的真实输出,键=(工具名,参数)。跳过执行时按键回灌。
    last_outputs: std::collections::HashMap<(String, String), String>,
}

impl ToolRepeatGate {
    pub(in crate::agent) fn new() -> Self {
        Self::default()
    }

    /// 观察一轮的调用集合,返回它与紧前一轮完全相同(同名同参同顺序)的
    /// 连续次数;0=与上一轮不同。空轮不参与计数。
    pub(in crate::agent) fn observe(&mut self, calls: &[ToolCall]) -> u32 {
        if calls.is_empty() {
            return self.consecutive;
        }
        let signature: Vec<(String, String)> = calls
            .iter()
            .map(|call| (call.function.name.clone(), call.function.arguments.clone()))
            .collect();
        if self.previous.as_ref() == Some(&signature) {
            self.consecutive += 1;
        } else {
            self.consecutive = 0;
            self.previous = Some(signature);
        }
        self.consecutive
    }

    /// 记录一轮的真实输出(执行后调用);下一轮同参跳过时按键回灌。
    pub(in crate::agent) fn record_output(&mut self, name: &str, arguments: &str, output: &str) {
        self.last_outputs.insert(
            (name.to_string(), arguments.to_string()),
            output.to_string(),
        );
    }

    /// 跳过执行时回灌的内容:上一轮同参调用的真实结果。签名相同保证在
    /// 场;极端缺失时退回空成功壳,绝不注入指令文本。
    pub(in crate::agent) fn cached_output(&self, name: &str, arguments: &str) -> String {
        self.last_outputs
            .get(&(name.to_string(), arguments.to_string()))
            .cloned()
            .unwrap_or_else(|| "{\"ok\": true}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolCallFunction;

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: "id".to_string(),
            kind: "function".to_string(),
            function: ToolCallFunction {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        }
    }

    /// 相同轮连续计数递增;不同轮(哪怕只差参数)立即清零;穿插别的调用
    /// 重置计数——edit→build→edit→build 不误伤。
    #[test]
    fn consecutive_identical_rounds_count_and_reset() {
        let mut gate = ToolRepeatGate::new();
        let search = [call("web_search", "{\"query\":\"a\"}")];
        assert_eq!(gate.observe(&search), 0);
        assert_eq!(gate.observe(&search), 1);
        assert_eq!(gate.observe(&search), 2);
        // 参数变了 → 清零重计。
        let other = [call("web_search", "{\"query\":\"b\"}")];
        assert_eq!(gate.observe(&other), 0);
        assert_eq!(gate.observe(&other), 1);
        // 穿插不同调用 → 重复 build 不累计。
        let build = [call("run_command", "{\"command\":\"cargo build\"}")];
        let edit = [call("edit", "{\"path\":\"a.rs\"}")];
        assert_eq!(gate.observe(&build), 0);
        assert_eq!(gate.observe(&edit), 0);
        assert_eq!(gate.observe(&build), 0);
    }

    /// 多调用轮按整轮比较:同一对调用重复才计数,顺序不同不算相同。
    #[test]
    fn multi_call_rounds_compare_whole_round() {
        let mut gate = ToolRepeatGate::new();
        let pair = [
            call("read", "{\"path\":\"a\"}"),
            call("read", "{\"path\":\"b\"}"),
        ];
        let swapped = [
            call("read", "{\"path\":\"b\"}"),
            call("read", "{\"path\":\"a\"}"),
        ];
        assert_eq!(gate.observe(&pair), 0);
        assert_eq!(gate.observe(&pair), 1);
        assert_eq!(gate.observe(&swapped), 0);
    }

    /// 空轮不动计数(空 tool_calls 意味着循环即将自然结束)。
    #[test]
    fn empty_round_keeps_count() {
        let mut gate = ToolRepeatGate::new();
        let search = [call("web_search", "{}")];
        assert_eq!(gate.observe(&search), 0);
        assert_eq!(gate.observe(&search), 1);
        assert_eq!(gate.observe(&[]), 1);
    }

    /// 跳过轮回灌上一轮真实结果的字节;未记录的键退回空成功壳。
    #[test]
    fn cached_output_replays_previous_result_bytes() {
        let mut gate = ToolRepeatGate::new();
        gate.record_output(
            "web_search",
            "{\"q\":\"a\"}",
            "{\"ok\": true, \"results\": \"x\"}",
        );
        assert_eq!(
            gate.cached_output("web_search", "{\"q\":\"a\"}"),
            "{\"ok\": true, \"results\": \"x\"}"
        );
        assert_eq!(
            gate.cached_output("web_search", "{\"q\":\"b\"}"),
            "{\"ok\": true}"
        );
    }
}
