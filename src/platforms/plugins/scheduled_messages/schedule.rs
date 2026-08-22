//! 定时表的纯时间计算：解析与到期判定，无 IO，便于单测。

use chrono::Datelike;

/// 每周全天的位掩码：bit0=周一 … bit6=周日。
pub(super) const EVERY_DAY: u8 = 0b0111_1111;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ScheduledTask {
    /// `group` 或 `private`。
    pub(super) conversation_kind: String,
    pub(super) conversation_id: String,
    /// 为空时用当前已连接的第一个账号。
    pub(super) account: Option<i64>,
    /// (hour, minute) 列表，本地时区。
    pub(super) times: Vec<(u32, u32)>,
    /// 生效的星期位掩码，bit0=周一。
    pub(super) days: u8,
    pub(super) message: String,
}

/// `HH:MM` → (hour, minute)。
pub(super) fn parse_time(value: &str) -> Option<(u32, u32)> {
    let (hour, minute) = value.trim().split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// 星期名 → 位。接受 mon/monday 等英文缩写与全称，大小写不敏感。
pub(super) fn parse_day(value: &str) -> Option<u8> {
    let index = match value.trim().to_ascii_lowercase().as_str() {
        "mon" | "monday" => 0,
        "tue" | "tuesday" => 1,
        "wed" | "wednesday" => 2,
        "thu" | "thursday" => 3,
        "fri" | "friday" => 4,
        "sat" | "saturday" => 5,
        "sun" | "sunday" => 6,
        _ => return None,
    };
    Some(1u8 << index)
}

/// 本次 tick 到期的 `(task_index, fire_key)` 列表。
///
/// 调度时刻落在 `(now - window_secs, now]` 内才算到期：tick 有间隔，进程也可能
/// 短暂卡顿；窗口外的过期时点一律跳过，**不补发**。fire_key 以日期开头，供
/// 去重表按天清理。凌晨窗口可能跨日，因此昨天与今天的时点都要检查。
pub(super) fn due_fires(
    tasks: &[ScheduledTask],
    now: chrono::DateTime<chrono::Local>,
    window_secs: i64,
) -> Vec<(usize, String)> {
    let mut due = Vec::new();
    for (index, task) in tasks.iter().enumerate() {
        for &(hour, minute) in &task.times {
            for date in [
                now.date_naive() - chrono::Days::new(1),
                now.date_naive(),
            ] {
                let Some(scheduled) = date
                    .and_hms_opt(hour, minute, 0)
                    .and_then(|naive| naive.and_local_timezone(chrono::Local).earliest())
                else {
                    continue;
                };
                let day_bit = 1u8 << scheduled.weekday().num_days_from_monday();
                if task.days & day_bit == 0 {
                    continue;
                }
                let elapsed = (now - scheduled).num_seconds();
                if elapsed < 0 || elapsed >= window_secs {
                    continue;
                }
                due.push((
                    index,
                    format!(
                        "{}|{:02}:{:02}|{}:{}",
                        scheduled.date_naive(),
                        hour,
                        minute,
                        task.conversation_kind,
                        task.conversation_id
                    ),
                ));
            }
        }
    }
    due
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn task(times: Vec<(u32, u32)>, days: u8) -> ScheduledTask {
        ScheduledTask {
            conversation_kind: "group".to_string(),
            conversation_id: "123".to_string(),
            account: None,
            times,
            days,
            message: "hello".to_string(),
        }
    }

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<chrono::Local> {
        chrono::Local
            .with_ymd_and_hms(y, mo, d, h, mi, s)
            .single()
            .expect("unambiguous local time")
    }

    #[test]
    fn parses_times_and_rejects_garbage() {
        assert_eq!(parse_time("08:30"), Some((8, 30)));
        assert_eq!(parse_time(" 23:59 "), Some((23, 59)));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("12:60"), None);
        assert_eq!(parse_time("noon"), None);
    }

    #[test]
    fn fires_inside_window_only() {
        let tasks = vec![task(vec![(8, 30)], EVERY_DAY)];
        // 2026-08-19 是周三。
        assert_eq!(due_fires(&tasks, local(2026, 8, 19, 8, 30, 5), 70).len(), 1);
        assert_eq!(due_fires(&tasks, local(2026, 8, 19, 8, 31, 30), 70).len(), 0);
        assert_eq!(due_fires(&tasks, local(2026, 8, 19, 8, 29, 55), 70).len(), 0);
    }

    #[test]
    fn missed_times_are_skipped_not_backfilled() {
        let tasks = vec![task(vec![(8, 30)], EVERY_DAY)];
        // 停机到 10 点才恢复:窗口外,不补发。
        assert!(due_fires(&tasks, local(2026, 8, 19, 10, 0, 0), 70).is_empty());
    }

    #[test]
    fn midnight_window_reaches_back_into_yesterday() {
        let tasks = vec![task(vec![(23, 59)], EVERY_DAY)];
        let due = due_fires(&tasks, local(2026, 8, 20, 0, 0, 30), 120);
        assert_eq!(due.len(), 1);
        assert!(due[0].1.starts_with("2026-08-19|23:59"), "key: {}", due[0].1);
    }

    #[test]
    fn respects_day_of_week_mask() {
        let monday_only = parse_day("mon").unwrap();
        let tasks = vec![task(vec![(8, 30)], monday_only)];
        // 2026-08-19 是周三,不发;2026-08-17 是周一,发。
        assert!(due_fires(&tasks, local(2026, 8, 19, 8, 30, 5), 70).is_empty());
        assert_eq!(due_fires(&tasks, local(2026, 8, 17, 8, 30, 5), 70).len(), 1);
    }

    #[test]
    fn multiple_times_and_tasks_fire_independently() {
        let tasks = vec![
            task(vec![(8, 30), (21, 0)], EVERY_DAY),
            ScheduledTask {
                conversation_id: "456".to_string(),
                ..task(vec![(8, 30)], EVERY_DAY)
            },
        ];
        let due = due_fires(&tasks, local(2026, 8, 19, 8, 30, 5), 70);
        assert_eq!(due.len(), 2);
        assert_ne!(due[0].1, due[1].1);
    }
}
