//! 场所层说的「这次回合走哪条人格车道」。
//!
//! 场所(CLI `--dev`、IPC 与会话记录里的 `normal|dev`、WebUI 开关)从来说的都是
//! 「用当前激活的人格,还是保留的开发人格 `dev`」——不是一种运行「模式」。回合引擎
//! 只认人格:`Agent::new` 把它折成 `core.dev`,提示词、工具面、记忆全按人格清单裁决。
//! 老词 `normal|dev` 只留在线上,用 [`PersonaLane::mode_word`] / [`PersonaLane::from_mode_word`] 进出。
//! 原「闲聊(Chat)」模式已删除:平台路径从来只走当前人格,安全靠 restricted registry
//! (工具不存在)而非模式门。

use super::{AppConfig, DEV_PERSONA};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum PersonaLane {
    /// 当前激活的人格(`config.active_persona_scope()`):人格全能力。
    Active,
    /// 保留人格 `dev`:极简开发形态(一行可编辑提示词、无人格全家、精简工具目录)。
    Dev,
}

impl PersonaLane {
    pub fn is_dev(self) -> bool {
        matches!(self, Self::Dev)
    }

    pub fn from_dev(dev: bool) -> Self {
        if dev {
            Self::Dev
        } else {
            Self::Active
        }
    }

    /// 这条车道对应的人格作用域 id:工具清单、记忆库、派生目录都按它找。
    pub fn scope(self, config: &AppConfig) -> String {
        match self {
            Self::Dev => DEV_PERSONA.to_string(),
            Self::Active => config.active_persona_scope(),
        }
    }

    /// 线上的词(IPC `mode` 字段、会话记录、CLI 输出):`normal` / `dev`,一字不改。
    pub fn mode_word(self) -> &'static str {
        match self {
            Self::Active => "normal",
            Self::Dev => "dev",
        }
    }

    /// 从线上的词进来:只有 `dev` 是开发车道,其余(含缺省)都是当前人格。
    pub fn from_mode_word(word: Option<&str>) -> Self {
        if word == Some("dev") {
            Self::Dev
        } else {
            Self::Active
        }
    }

    /// 给人看的标签(banner / footer)。
    pub fn label(self) -> &'static str {
        if crate::i18n::is_zh() {
            match self {
                Self::Active => "普通",
                Self::Dev => "开发",
            }
        } else {
            match self {
                Self::Active => "NORMAL",
                Self::Dev => "DEV",
            }
        }
    }
}
