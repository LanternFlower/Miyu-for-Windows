use anyhow::{bail, Result};
use chrono::{Local, TimeZone};
use miyu_base::paths::MiyuPaths;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlarmRecord {
    pub id: String,
    pub label: String,
    pub time: String,
    pub audio_file: Option<PathBuf>,
    pub due_at: i64,
    pub pid: Option<u32>,
    pub status: AlarmStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlarmStatus {
    Scheduled,
    Ringing,
}

pub fn alarms_file(paths: &MiyuPaths) -> PathBuf {
    paths.state_dir.join("alarms.json")
}

pub fn alarm_log_file(paths: &MiyuPaths) -> PathBuf {
    paths.logs_dir().join("alarm.log")
}

pub fn parse_alarm_seconds(value: &str) -> Result<u64> {
    // 时钟写法先走(HH:MM),归一化只对相对时长做。
    let raw = value.trim();
    if raw.split_whitespace().count() == 1 && raw.contains(':') {
        return seconds_until_clock(raw);
    }
    let normalized = normalize_duration(raw);
    let parts = normalized.split_whitespace().collect::<Vec<_>>();
    if parts.len() == 1 && parts[0].contains(':') {
        return seconds_until_clock(parts[0]);
    }
    let mut total = 0u64;
    for part in &parts {
        let part = *part;
        // 按 数字+单位 分段扫描：兼容 "1h30m" 这类无空格复合写法，且不做字节
        // 切分（"5分钟" 之类多字节输入走 bail 而非 panic）。
        let mut number = String::new();
        let mut segments = 0usize;
        for ch in part.chars() {
            if ch.is_ascii_digit() {
                number.push(ch);
                continue;
            }
            if number.is_empty() {
                bail!("invalid alarm time: {value}")
            }
            let amount = number.parse::<u64>()?;
            number.clear();
            segments += 1;
            let multiplier = match ch.to_ascii_lowercase() {
                'd' => 86_400,
                'h' => 3600,
                'm' => 60,
                's' => 1,
                _ => bail!("invalid alarm time unit: {ch}"),
            };
            total = amount
                .checked_mul(multiplier)
                .and_then(|secs| total.checked_add(secs))
                .ok_or_else(|| anyhow::anyhow!("alarm time overflows: {value}"))?;
        }
        if !number.is_empty() || segments == 0 {
            bail!("invalid alarm time: {value}")
        }
    }
    if total == 0 {
        bail!("alarm time must be greater than zero")
    }
    Ok(total)
}

/// 把相对时长里的中文 / 英文单位和噪声词归一化成 `数字+d/h/m/s` 的紧凑写法,
/// 让主扫描器复用。闹钟以前「只认 18:00」的真因就是:中文模型自然写「5分钟」
/// 「2小时后」「半小时」这类,一律撞上「invalid alarm time unit」而 bail。
/// 时钟写法(HH:MM)不走这里。中文口语时钟「18点」暂不处理(涉及上午/下午换算)。
fn normalize_duration(input: &str) -> String {
    let mut s = input.to_ascii_lowercase();
    // 半小时 / 半分钟先处理(带 Chinese 数词的唯一常见写法)。
    for (from, to) in [("半个小时", "30m"), ("半小时", "30m"), ("半分钟", "30s")] {
        s = s.replace(from, to);
    }
    // 单位:长的先替,免得「小时」被「时」抢先、「分钟」被「分」抢先。
    for (from, to) in [
        ("小时", "h"),
        ("个小时", "h"),
        ("個小時", "h"),
        ("钟头", "h"),
        ("鐘頭", "h"),
        ("hours", "h"),
        ("hour", "h"),
        ("hrs", "h"),
        ("hr", "h"),
        ("分钟", "m"),
        ("分鐘", "m"),
        ("minutes", "m"),
        ("minute", "m"),
        ("mins", "m"),
        ("min", "m"),
        ("秒钟", "s"),
        ("秒鐘", "s"),
        ("seconds", "s"),
        ("second", "s"),
        ("secs", "s"),
        ("sec", "s"),
        ("days", "d"),
        ("day", "d"),
        ("天", "d"),
        ("日", "d"),
        ("时", "h"),
        ("分", "m"),
        ("秒", "s"),
    ] {
        s = s.replace(from, to);
    }
    // 去掉方位 / 连接噪声词(此时单位已全替成 d/h/m/s,「in」「min」不会误伤)。
    for noise in [
        "之后", "以后", "後", "后", "later", "after", "过", "整", "再", "等", "的", "钟", "in",
        "at",
    ] {
        s = s.replace(noise, "");
    }
    // 数字与单位间的空格收拢:「90 m」→「90m」,让「1h 2m」式和「90 minutes」式都过扫描器。
    for unit in [" d", " h", " m", " s"] {
        s = s.replace(unit, unit.trim());
    }
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

pub fn due_at_from_time(value: &str) -> Result<i64> {
    let secs = i64::try_from(parse_alarm_seconds(value)?)
        .map_err(|_| anyhow::anyhow!("alarm time overflows: {value}"))?;
    Local::now()
        .timestamp()
        .checked_add(secs)
        .ok_or_else(|| anyhow::anyhow!("alarm time overflows: {value}"))
}

pub fn load(paths: &MiyuPaths) -> Result<Vec<AlarmRecord>> {
    let file = alarms_file(paths);
    if !file.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(file)?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&content)?)
}

pub fn save(paths: &MiyuPaths, records: &[AlarmRecord]) -> Result<()> {
    std::fs::create_dir_all(&paths.state_dir)?;
    let file = alarms_file(paths);
    let temp = tempfile::NamedTempFile::new_in(&paths.state_dir)?;
    std::fs::write(temp.path(), serde_json::to_vec_pretty(records)?)?;
    temp.persist(file)?;
    Ok(())
}

pub fn upsert(paths: &MiyuPaths, record: AlarmRecord) -> Result<()> {
    let mut records = load(paths)?;
    records.retain(|existing| existing.id != record.id);
    records.push(record);
    save(paths, &records)
}

pub fn update_status(paths: &MiyuPaths, id: &str, status: AlarmStatus) -> Result<()> {
    let mut records = load(paths)?;
    if let Some(record) = records.iter_mut().find(|record| record.id == id) {
        record.status = status;
    }
    save(paths, &records)
}

pub fn remove(paths: &MiyuPaths, id: &str) -> Result<Option<AlarmRecord>> {
    let mut records = load(paths)?;
    let mut removed = None;
    records.retain(|record| {
        if record.id == id {
            removed = Some(record.clone());
            false
        } else {
            true
        }
    });
    save(paths, &records)?;
    Ok(removed)
}

pub fn cleanup_dead(paths: &MiyuPaths) -> Result<Vec<AlarmRecord>> {
    let records = load(paths)?;
    let active = records
        .into_iter()
        .filter(|record| record.pid.is_none_or(process_exists))
        .collect::<Vec<_>>();
    save(paths, &active)?;
    Ok(active)
}

pub fn stop_process(pid: u32) -> Result<()> {
    if process_exists(pid) {
        crate::sys::terminate_process(pid, false);
    }
    Ok(())
}

pub fn process_exists(pid: u32) -> bool {
    crate::sys::process_alive(pid)
}

fn seconds_until_clock(value: &str) -> Result<u64> {
    let Some((hour, minute)) = value.split_once(':') else {
        bail!("invalid clock time: {value}")
    };
    let hour = hour.parse::<u32>()?;
    let minute = minute.parse::<u32>()?;
    if hour > 23 || minute > 59 {
        bail!("invalid clock time: {value}")
    }
    let now = Local::now();
    let today = now.date_naive();
    let target_time = chrono::NaiveTime::from_hms_opt(hour, minute, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid clock time: {value}"))?;
    let mut target = today.and_time(target_time);
    if target <= now.naive_local() {
        target += chrono::Duration::days(1);
    }
    Ok((target - now.naive_local()).num_seconds().max(1) as u64)
}

pub fn format_due_at(timestamp: i64) -> String {
    Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(state_dir: PathBuf) -> MiyuPaths {
        let cache_dir = state_dir.join("cache");
        MiyuPaths {
            root_dir: PathBuf::new(),
            config_dir: PathBuf::new(),
            config_file: PathBuf::new(),
            skills_dir: PathBuf::new(),
            data_dir: PathBuf::new(),
            cache_dir,
            state_dir,
            pictures_dir: PathBuf::new(),
            fish_hook_file: PathBuf::new(),
            bash_hook_file: PathBuf::new(),
            zsh_hook_file: PathBuf::new(),
            scripts_dir: PathBuf::new(),
            system_scripts_dir: PathBuf::new(),
        }
    }

    #[test]
    fn parses_alarm_durations() {
        assert_eq!(parse_alarm_seconds("30s").unwrap(), 30);
        assert_eq!(parse_alarm_seconds("10m").unwrap(), 600);
        assert_eq!(parse_alarm_seconds("1h 2m 3s").unwrap(), 3723);
        assert_eq!(parse_alarm_seconds("1h30m").unwrap(), 5400);
        assert_eq!(parse_alarm_seconds("2d").unwrap(), 172_800);
        assert!(parse_alarm_seconds("0s").is_err());
    }

    #[test]
    fn parses_natural_language_durations() {
        // 中文口语相对时长(闹钟以前只认 18:00 的真因就是这些一律 bail)。
        assert_eq!(parse_alarm_seconds("5分钟").unwrap(), 300);
        assert_eq!(parse_alarm_seconds("2小时后").unwrap(), 7200);
        assert_eq!(parse_alarm_seconds("半小时").unwrap(), 1800);
        assert_eq!(parse_alarm_seconds("30秒").unwrap(), 30);
        assert_eq!(parse_alarm_seconds("1小时30分钟").unwrap(), 5400);
        assert_eq!(parse_alarm_seconds("in 90 minutes").unwrap(), 5400);
        assert_eq!(parse_alarm_seconds("3天").unwrap(), 259_200);
        // 时钟写法仍走原路径,不被归一化。
        assert!(parse_alarm_seconds("14:30").is_ok());
    }

    #[test]
    fn rejects_bad_input_without_panicking() {
        // 没有数字的裸单位、无法识别的多字节输入仍然报错、不 panic。
        assert!(parse_alarm_seconds("分钟").is_err());
        assert!(parse_alarm_seconds("喵喵喵").is_err());
        assert!(parse_alarm_seconds("5喵").is_err());
    }

    #[test]
    fn rejects_overflowing_durations() {
        assert!(parse_alarm_seconds("9999999999999999999h").is_err());
        assert!(parse_alarm_seconds("18446744073709551615s 1s").is_err());
        assert!(due_at_from_time("9999999999999999999h").is_err());
    }

    #[test]
    fn alarm_log_uses_cache_directory() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path().join("state"));

        assert_eq!(
            alarm_log_file(&paths),
            paths.cache_dir.join("logs/alarm.log")
        );
    }

    #[test]
    fn saves_updates_and_removes_alarm_records() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path().to_path_buf());
        let record = AlarmRecord {
            id: "alarm-test".to_string(),
            label: "test".to_string(),
            time: "30s".to_string(),
            audio_file: None,
            due_at: 123,
            pid: None,
            status: AlarmStatus::Scheduled,
        };
        upsert(&paths, record).unwrap();
        assert_eq!(load(&paths).unwrap().len(), 1);
        update_status(&paths, "alarm-test", AlarmStatus::Ringing).unwrap();
        assert_eq!(load(&paths).unwrap()[0].status, AlarmStatus::Ringing);
        assert!(remove(&paths, "alarm-test").unwrap().is_some());
        assert!(load(&paths).unwrap().is_empty());
    }
}
