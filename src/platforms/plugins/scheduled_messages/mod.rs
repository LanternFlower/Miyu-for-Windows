//! QQ 定时消息插件：到点把配置的固定文本直接发到会话，不经过 AI。
//!
//! 调度是一个 daemon 级后台循环（`spawn_scheduled_message_worker`），每个 tick
//! 重新读一次配置——改配置无需重启即可生效。发送走
//! `onebot::send_direct_text`，与回合调度完全无关。错过的时点不补发；同一
//! 时点用 fire key 去重，key 按天清理。

mod schedule;

use super::{PlatformPlugin, PluginDescriptor};
use crate::config::{AppConfig, QQ_SCHEDULED_MESSAGES_PLUGIN_ID};
use schedule::{due_fires, parse_day, parse_time, ScheduledTask, EVERY_DAY};
use serde_json::Value;
use std::collections::HashSet;

/// tick 间隔。窗口必须显著大于 tick，否则卡顿一个 tick 就会漏发。
const TICK_SECONDS: u64 = 20;
const FIRE_WINDOW_SECONDS: i64 = 70;

/// 注册表里的占位实现：本插件没有入站钩子，注册它只为让 enabled 开关
/// 与其它插件走同一套 `platforms.qq.plugins.<id>` 配置面。
pub(super) struct ScheduledMessagesPlugin;

impl ScheduledMessagesPlugin {
    pub(super) fn new() -> Self {
        Self
    }
}

impl PlatformPlugin for ScheduledMessagesPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: QQ_SCHEDULED_MESSAGES_PLUGIN_ID,
            priority: 0,
            default_enabled: false,
        }
    }
}

/// daemon 启动时挂一次。循环本身常驻；插件未启用或无任务时每个 tick 直接跳过。
pub(crate) fn spawn_scheduled_message_worker(state: crate::runtime::DaemonState) {
    tokio::spawn(async move {
        let mut fired: HashSet<String> = HashSet::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(TICK_SECONDS)).await;
            // 锁内只读开关并解析任务表，不整份深拷贝 AppConfig：这个 tick 在
            // QQ 关闭/无任务的空闲期也一直在跑，拷贝是纯浪费。
            let tasks = {
                let manager = state.manager.lock().unwrap();
                if !manager.config.platforms.qq.enabled {
                    continue;
                }
                enabled_tasks(&manager.config)
            };
            if tasks.is_empty() {
                if !fired.is_empty() {
                    fired.clear();
                }
                continue;
            }
            let now = chrono::Local::now();
            let today = now.date_naive().to_string();
            let yesterday = (now.date_naive() - chrono::Days::new(1)).to_string();
            fired.retain(|key| key.starts_with(&today) || key.starts_with(&yesterday));
            for (index, key) in due_fires(&tasks, now, FIRE_WINDOW_SECONDS) {
                // 先记账再发送:发送失败不重试,否则每个 tick 都会撞一次失败。
                if !fired.insert(key.clone()) {
                    continue;
                }
                let task = &tasks[index];
                let result = crate::platforms::onebot::send_direct_text(
                    &state,
                    task.account,
                    &task.conversation_kind,
                    &task.conversation_id,
                    &task.message,
                )
                .await;
                match result {
                    Ok(()) => tracing::info!(fire = %key, "scheduled message sent"),
                    Err(error) => {
                        tracing::warn!(%error, fire = %key, "scheduled message failed")
                    }
                }
            }
        }
    });
}

/// 从当前配置读出启用且合法的任务。运行时静默跳过非法项——配置加载时的
/// 校验器已经把格式错误挡在保存/启动阶段，这里只是防御。
fn enabled_tasks(config: &AppConfig) -> Vec<ScheduledTask> {
    let Some(instance) = config
        .platforms
        .qq
        .plugins
        .get(QQ_SCHEDULED_MESSAGES_PLUGIN_ID)
    else {
        return Vec::new();
    };
    if !instance.enabled_or(false) {
        return Vec::new();
    }
    let Some(tasks) = instance.settings.get("tasks").and_then(Value::as_array) else {
        return Vec::new();
    };
    tasks.iter().filter_map(parse_task).collect()
}

fn parse_task(value: &Value) -> Option<ScheduledTask> {
    let conversation = value.get("conversation")?.as_str()?;
    let (kind, id) = conversation.trim().split_once(':')?;
    if !matches!(kind, "group" | "private") || id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let times: Vec<(u32, u32)> = value
        .get("times")?
        .as_array()?
        .iter()
        .map(|time| time.as_str().and_then(parse_time))
        .collect::<Option<_>>()?;
    if times.is_empty() {
        return None;
    }
    let message = value.get("message")?.as_str()?.trim().to_string();
    if message.is_empty() {
        return None;
    }
    let days = match value.get("days") {
        None => EVERY_DAY,
        Some(days) => {
            let mask = days
                .as_array()?
                .iter()
                .map(|day| day.as_str().and_then(parse_day))
                .collect::<Option<Vec<u8>>>()?
                .into_iter()
                .fold(0u8, |mask, bit| mask | bit);
            if mask == 0 {
                return None;
            }
            mask
        }
    };
    Some(ScheduledTask {
        conversation_kind: kind.to_string(),
        conversation_id: id.to_string(),
        account: value.get("account").and_then(Value::as_i64),
        times,
        days,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_full_task() {
        let task = parse_task(&json!({
            "conversation": "group:123456",
            "times": ["08:30", "21:00"],
            "message": "早安！",
            "days": ["mon", "fri"],
            "account": 10001
        }))
        .expect("valid task");
        assert_eq!(task.conversation_kind, "group");
        assert_eq!(task.conversation_id, "123456");
        assert_eq!(task.times, vec![(8, 30), (21, 0)]);
        assert_eq!(task.days, 0b0001_0001);
        assert_eq!(task.account, Some(10001));
    }

    #[test]
    fn rejects_malformed_tasks() {
        for bad in [
            json!({ "conversation": "channel:1", "times": ["08:30"], "message": "hi" }),
            json!({ "conversation": "group:abc", "times": ["08:30"], "message": "hi" }),
            json!({ "conversation": "group:1", "times": [], "message": "hi" }),
            json!({ "conversation": "group:1", "times": ["25:00"], "message": "hi" }),
            json!({ "conversation": "group:1", "times": ["08:30"], "message": "  " }),
            json!({ "conversation": "group:1", "times": ["08:30"], "message": "hi", "days": [] }),
            json!({ "conversation": "group:1", "times": ["08:30"], "message": "hi", "days": ["holiday"] }),
        ] {
            assert!(parse_task(&bad).is_none(), "should reject: {bad}");
        }
    }
}
