//! 语音相关配置：唤醒/听写（`VoiceConfig`）与回复播报（`VoiceTtsConfig`
//! 及两家供应商 `MimoTtsConfig` / `MiniMaxTtsConfig`）。

use crate::config::*;

/// 语音功能。整套只在 `voice.enabled` 时由 daemon 拉起独立的 `miyu-voice`
/// 进程,关着时 daemon 零占用;主程序不含任何识别模型代码。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceConfig {
    /// 语音唤醒开关:麦克风常开、唤醒词、听写。与 `tts.enabled` 独立。
    #[serde(default)]
    pub enabled: bool,
    /// 中文唤醒词,任意汉字,内部转拼音送 KWS 模型,不用重训。
    /// 唤醒词,可多个,任一命中即唤醒。配置里写数组或逗号分隔的字符串都行,
    /// 旧键名 `wake_keyword` 照样读。
    #[serde(
        default = "default_wake_keywords",
        alias = "wake_keyword",
        deserialize_with = "deserialize_wake_keywords"
    )]
    pub wake_keywords: Vec<String>,
    /// 唤醒判定阈值(0~1,越低越灵敏;sherpa 默认 0.25)。
    #[serde(default = "default_wake_threshold")]
    pub wake_threshold: f32,
    /// 唤醒词路径加分(越大越灵敏;sherpa 默认 1.0)。
    #[serde(default = "default_wake_boost")]
    pub wake_boost: f32,
    /// 麦克风设备名(`miyu-voice devices` 可列),null = 系统默认。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub microphone: Option<String>,
    /// "local"(SenseVoice,本地)| "cloud"(OpenAI 兼容 transcriptions)。
    /// 本地识别线程数。
    #[serde(default = "default_stt_threads")]
    pub stt_threads: usize,
    /// 本地识别语言:auto | zh | en | ja | ko | yue。固定 zh 可避免噪声被
    /// 认成日文碎片。
    #[serde(default = "default_stt_language")]
    pub stt_language: String,
    /// 本地识别模型闲置多少秒后卸载(0 = 常驻)。
    #[serde(default = "default_stt_unload_seconds")]
    pub stt_unload_seconds: u64,
    /// 免唤醒追问窗口(秒):从她回复完(播报播完)起算,这段时间内说话不用
    /// 再喊唤醒词;每次回复都重新起算。0 = 每句都要唤醒词。
    #[serde(default = "default_follow_up_seconds")]
    pub follow_up_seconds: u64,
    /// 识别文本少于这么多有效字视为噪声丢弃。
    #[serde(default = "default_min_utterance_chars")]
    pub min_utterance_chars: usize,
    /// 提示音总开关。
    #[serde(default = "default_true")]
    pub sounds: bool,
    #[serde(default = "default_sound_volume")]
    pub sound_volume: f32,
    /// 回合完成通知里带的回复摘要字数。
    #[serde(default = "default_notify_reply_chars")]
    pub notify_reply_chars: usize,
    /// REPL 听写:识别一句就直接提交(true)还是先填进编辑框等回车(false)。
    #[serde(default)]
    pub dictation_auto_submit: bool,
    /// 回复播报(语音合成)。
    #[serde(default)]
    pub tts: VoiceTtsConfig,
}

/// 回复播报(语音合成)。供应商各自独立配置(不共用 providers 里的 LLM
/// 供应商,免得混),`active` 指向激活的那一个;空 = 不播报,只弹通知和提示音。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceTtsConfig {
    /// 文本转语音开关:开了才播报回复、才注册 `speak` 工具。与语音唤醒独立,
    /// 任一开启都会拉起 miyu-voice(唤醒关闭时它只管播放,不开麦克风)。
    #[serde(default)]
    pub enabled: bool,
    /// 播报供应商:`minimax` | `mimo`(小米 MiMo);None / 空 = 默认 MiniMax。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// 播报文本上限(字),超出截断。
    #[serde(default = "default_tts_max_chars")]
    pub max_chars: usize,
    /// 试听用的句子。
    #[serde(default = "default_tts_preview_text")]
    pub preview_text: String,
    #[serde(default)]
    pub minimax: MiniMaxTtsConfig,
    #[serde(default)]
    pub mimo: MimoTtsConfig,
}

/// 小米 MiMo 语音合成(`mimo-v2.5-tts` 系列,OpenAI 兼容的 `chat/completions`:
/// 待合成文本放 assistant 消息,风格描述放 user 消息,音频以 base64 回来)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MimoTtsConfig {
    /// API key(platform.xiaomimimo.com),支持 `$env:VAR` 引用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// `https://api.xiaomimimo.com/v1`。
    #[serde(default = "default_mimo_base_url")]
    pub base_url: String,
    /// `mimo-v2.5-tts`(预置音色)| `mimo-v2.5-tts-voicedesign`(按描述造音色)|
    /// `mimo-v2.5-tts-voiceclone`(按样本克隆)。
    #[serde(default = "default_mimo_model")]
    pub model: String,
    /// 预置音色:mimo_default / 冰糖 / 茉莉 / 苏打 / 白桦 / Mia / Chloe / Milo / Dean。
    /// voicedesign / voiceclone 模型不用它。
    #[serde(default = "default_mimo_voice")]
    pub voice: String,
    /// 风格标签(写在文本开头的 `(温柔)` 那种):空 = 不加。多个用空格隔开,
    /// 如 `温柔 慵懒`。
    #[serde(default)]
    pub style: String,
    /// 提示词(user 消息):语速/语气/角色用自然语言写,如「语速稍快,像在跟朋友
    /// 聊天」(MiMo 没有数值语速,只认这种说法);voicedesign 模型下是音色描述
    /// (必填)。空 = 不发 user 消息。旧键名 `instruction` 照样读。
    #[serde(default, alias = "instruction")]
    pub prompt: String,
    /// voiceclone 模型的参考音频路径(wav / mp3,base64 后 ≤ 10MB)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_audio: Option<String>,
}

fn default_mimo_base_url() -> String {
    "https://api.xiaomimimo.com/v1".to_string()
}
fn default_mimo_model() -> String {
    "mimo-v2.5-tts".to_string()
}
fn default_mimo_voice() -> String {
    "mimo_default".to_string()
}

impl Default for MimoTtsConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: default_mimo_base_url(),
            model: default_mimo_model(),
            voice: default_mimo_voice(),
            style: String::new(),
            prompt: String::new(),
            sample_audio: None,
        }
    }
}

impl MimoTtsConfig {
    pub fn has_key(&self) -> bool {
        self.api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
    }
}

/// 播报供应商 id 与显示名(TUI/WebUI 列表顺序)。
pub const TTS_PROVIDERS: &[(&str, &str)] = &[("minimax", "MiniMax"), ("mimo", "Xiaomi MiMo")];

/// MiniMax `t2a_v2` 播报配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MiniMaxTtsConfig {
    /// API key,支持 `$env:VAR` 引用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// 国内 `https://api.minimaxi.com/v1`,国际 `https://api.minimax.io/v1`。
    #[serde(default = "default_minimax_base_url")]
    pub base_url: String,
    #[serde(default = "default_tts_model")]
    pub model: String,
    /// 音色 id(系统音色名或克隆音色 id)。
    #[serde(default = "default_tts_voice")]
    pub voice_id: String,
    /// 语速 0.5~2.0。
    #[serde(default = "default_unit")]
    pub speed: f32,
    /// 音量 0.1~10。
    #[serde(default = "default_unit")]
    pub vol: f32,
    /// 音调:半音偏移 -12~12,0 原声。
    #[serde(default)]
    pub pitch: i32,
    /// 情绪:空=模型自定;happy | sad | angry | fearful | disgusted | surprised | calm | fluent | whisper
    #[serde(default)]
    pub emotion: String,
    /// 语种增强:auto 或语种名(Chinese / English / Japanese …)。
    #[serde(default = "default_language_boost")]
    pub language_boost: String,
}

fn default_minimax_base_url() -> String {
    "https://api.minimaxi.com/v1".to_string()
}
fn default_tts_model() -> String {
    "speech-2.6-turbo".to_string()
}
fn default_tts_voice() -> String {
    "Chinese_sweet_girl_nv1".to_string()
}
fn default_unit() -> f32 {
    1.0
}
fn default_language_boost() -> String {
    "auto".to_string()
}
fn default_tts_max_chars() -> usize {
    300
}
fn default_tts_preview_text() -> String {
    "今天也是充满希望的一天".to_string()
}

impl Default for VoiceTtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            active: None,
            max_chars: default_tts_max_chars(),
            preview_text: default_tts_preview_text(),
            minimax: MiniMaxTtsConfig::default(),
            mimo: MimoTtsConfig::default(),
        }
    }
}

impl Default for MiniMaxTtsConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: default_minimax_base_url(),
            model: default_tts_model(),
            voice_id: default_tts_voice(),
            speed: 1.0,
            vol: 1.0,
            pitch: 0,
            emotion: String::new(),
            language_boost: default_language_boost(),
        }
    }
}

impl VoiceTtsConfig {
    /// 播报可用:开关开着,且激活的供应商配好了(`active` 缺省当 MiniMax,
    /// 填了 key 就算配好——装上、填 key、开开关三步即可,不用再点"激活")。
    pub fn is_active(&self) -> bool {
        self.enabled
            && self
                .provider()
                .is_some_and(|provider| self.provider_has_key(provider))
    }

    /// 生效的供应商名:`active` 为空或空串时默认 MiniMax。
    pub fn provider(&self) -> Option<&str> {
        match self.active.as_deref().map(str::trim) {
            None | Some("") => Some("minimax"),
            Some(other) => Some(other),
        }
    }

    /// 某个供应商是否填了 key(未知供应商名 = 没有)。
    pub fn provider_has_key(&self, provider: &str) -> bool {
        match provider {
            "minimax" => self.minimax.has_key(),
            "mimo" => self.mimo.has_key(),
            _ => false,
        }
    }
}

impl MiniMaxTtsConfig {
    pub fn has_key(&self) -> bool {
        self.api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
    }
}

fn default_wake_keywords() -> Vec<String> {
    ["未有未有", "密友密友", "miyumiyu", "みゆみゆ"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// 把 "未有未有, 小未" 这样的文本拆成唤醒词列表(逗号/顿号/分号/换行分隔,去重)。
pub fn split_wake_keywords(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in text.split(|ch: char| matches!(ch, ',' | '，' | '、' | ';' | '；' | '\n')) {
        let part = part.trim();
        if !part.is_empty() && !out.iter().any(|seen| seen == part) {
            out.push(part.to_string());
        }
    }
    out
}

fn deserialize_wake_keywords<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    let mut out = Vec::new();
    match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(text) => out = split_wake_keywords(&text),
        OneOrMany::Many(items) => {
            for item in items {
                for keyword in split_wake_keywords(&item) {
                    if !out.contains(&keyword) {
                        out.push(keyword);
                    }
                }
            }
        }
    }
    if out.is_empty() {
        out = default_wake_keywords();
    }
    Ok(out)
}
fn default_wake_threshold() -> f32 {
    0.25
}
fn default_wake_boost() -> f32 {
    1.0
}
fn default_stt_threads() -> usize {
    2
}
fn default_stt_language() -> String {
    // SenseVoice 自动判语种会把普通话片段判成日语吐假名,默认锁中文。
    "zh".to_string()
}
fn default_stt_unload_seconds() -> u64 {
    60
}
fn default_follow_up_seconds() -> u64 {
    30
}
fn default_min_utterance_chars() -> usize {
    2
}
fn default_sound_volume() -> f32 {
    0.6
}
fn default_notify_reply_chars() -> usize {
    120
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            wake_keywords: default_wake_keywords(),
            wake_threshold: default_wake_threshold(),
            wake_boost: default_wake_boost(),
            microphone: None,
            stt_threads: default_stt_threads(),
            stt_language: default_stt_language(),
            stt_unload_seconds: default_stt_unload_seconds(),
            follow_up_seconds: default_follow_up_seconds(),
            min_utterance_chars: default_min_utterance_chars(),
            sounds: true,
            sound_volume: default_sound_volume(),
            notify_reply_chars: default_notify_reply_chars(),
            dictation_auto_submit: false,
            tts: VoiceTtsConfig::default(),
        }
    }
}
