//! [`PersonaLane`] 里唯一需要 `AppConfig` 的那一个方法。
//!
//! 类型本身住在基础层(`crate::persona_lane`),因为基础层的 `workspace` 要拿它记
//! task-local;而「这条车道对应哪个人格作用域」得问配置,所以留在这儿。同一个
//! crate 内固有 impl 可以拆在两个模块里。

use super::{AppConfig, DEV_PERSONA};
use crate::persona_lane::PersonaLane;

impl PersonaLane {
    /// 这条车道对应的人格作用域 id:工具清单、记忆库、派生目录都按它找。
    pub fn scope(self, config: &AppConfig) -> String {
        match self {
            Self::Dev => DEV_PERSONA.to_string(),
            Self::Active => config.active_persona_scope(),
        }
    }
}
