//! 终端/WebUI 显示相关配置。
//!
//! `DisplayConfig` 的反序列化是手写的（`RawDisplayConfig` 先收旧键名再折算），
//! 所以定义、原始形态与 `Default` 三件放在一起。

use crate::config::*;

#[derive(Debug, Clone, Serialize)]
pub struct DisplayConfig {
    #[serde(default = "default_display_language")]
    pub language: String,
    #[serde(default = "default_reasoning_display")]
    pub reasoning: String,
    #[serde(default = "default_tool_call_display")]
    pub tool_calls: String,
    #[serde(default = "default_true")]
    pub readable_tool_names: bool,
    #[serde(default)]
    pub show_token_usage: bool,
    #[serde(default = "default_mixed_model_endpoint_display")]
    pub mixed_model_endpoint_display: String,
    /// 命令那一步抬头底下露几行**命令**。09-17 之前露的是命令输出,现在输出
    /// 退到点开里;键名不改,改了用户设过的值会掉回默认。
    #[serde(default = "default_command_output_lines")]
    pub command_output_lines: usize,
    /// How many finished turns a reopened REPL redraws; 0 disables replay.
    #[serde(default = "default_repl_replay_turns")]
    pub repl_replay_turns: usize,
    /// 空会话时在输入框上方画 MIYU banner（渐变艺术字 + 星空 + 模式行）。
    /// 关掉就只剩输入框。艺术字可用 `config/banner.txt` 替换。
    #[serde(default = "default_true")]
    pub banner: bool,
    /// 这个版本不认识的显示项，原样留着写回。见 [`AppConfig::extra`]。
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawDisplayConfig {
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<String>,
    #[serde(default)]
    show_reasoning: Option<bool>,
    #[serde(default)]
    reasoning_mode: Option<String>,
    #[serde(default)]
    show_tool_details: Option<bool>,
    #[serde(default)]
    readable_tool_names: Option<bool>,
    #[serde(default)]
    show_token_usage: Option<bool>,
    #[serde(default)]
    show_mixed_model_endpoint: Option<bool>,
    #[serde(default)]
    mixed_model_endpoint_display: Option<String>,
    #[serde(default)]
    command_output_lines: Option<usize>,
    #[serde(default)]
    repl_replay_turns: Option<usize>,
    #[serde(default)]
    banner: Option<bool>,
    #[serde(flatten, default)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for DisplayConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawDisplayConfig::deserialize(deserializer)?;
        let reasoning = raw.reasoning.unwrap_or_else(|| {
            if raw.show_reasoning == Some(false) {
                "hidden".to_string()
            } else {
                raw.reasoning_mode.unwrap_or_else(default_reasoning_display)
            }
        });
        let tool_calls = raw.tool_calls.unwrap_or_else(|| {
            if raw.show_tool_details == Some(true) {
                "full".to_string()
            } else {
                default_tool_call_display()
            }
        });
        Ok(Self {
            language: raw.language.unwrap_or_else(default_display_language),
            reasoning,
            tool_calls,
            readable_tool_names: raw.readable_tool_names.unwrap_or_else(default_true),
            show_token_usage: raw.show_token_usage.unwrap_or(false),
            mixed_model_endpoint_display: raw.mixed_model_endpoint_display.unwrap_or_else(|| {
                match raw.show_mixed_model_endpoint {
                    Some(true) => "all".to_string(),
                    Some(false) => "off".to_string(),
                    None => default_mixed_model_endpoint_display(),
                }
            }),
            command_output_lines: raw
                .command_output_lines
                .unwrap_or_else(default_command_output_lines),
            repl_replay_turns: raw
                .repl_replay_turns
                .unwrap_or_else(default_repl_replay_turns),
            banner: raw.banner.unwrap_or(true),
            extra: raw.extra,
        })
    }
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            language: default_display_language(),
            reasoning: default_reasoning_display(),
            tool_calls: default_tool_call_display(),
            readable_tool_names: default_true(),
            show_token_usage: false,
            mixed_model_endpoint_display: default_mixed_model_endpoint_display(),
            command_output_lines: default_command_output_lines(),
            repl_replay_turns: default_repl_replay_turns(),
            banner: true,
            extra: BTreeMap::new(),
        }
    }
}
