//! AreWeAntiCheatYet 反作弊状态查询。
//!
//! 数据取自项目仓库的 `games.json` 全表（约 1200 条、460KB），而不是抓
//! `areweanticheatyet.com/game/{slug}` 的页面：全表带 `storeIds.steam`，可以按
//! Steam AppID 精确匹配，不必猜 slug；`status` 也是干净的五值枚举，不用从 HTML
//! 里正则捞。
//!
//! 全表按天缓存到 `cache_dir/awacy-games.json`——它变动频率是天级，而每次游戏
//! 兼容性查询都要用它。

use super::http_response;
use crate::paths::MiyuPaths;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::{Duration, SystemTime};

const GAMES_JSON: &str = "https://raw.githubusercontent.com/AreWeAntiCheatYet/\
                          AreWeAntiCheatYet/HEAD/games.json";
const CACHE_FILE: &str = "awacy-games.json";
const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct AwacyGame {
    pub name: String,
    pub slug: String,
    /// Supported / Running / Planned / Broken / Denied
    pub status: String,
    #[serde(default)]
    pub anticheats: Vec<String>,
    #[serde(default)]
    pub native: bool,
    #[serde(default, rename = "dateChanged")]
    pub date_changed: String,
    #[serde(default)]
    pub updates: Vec<AwacyUpdate>,
    /// `[[正文, 参考链接], ...]`。参考链接经常是 `null`（实测 1166 条里有 110 个），
    /// 所以内层必须容忍空值，否则整表反序列化直接失败。
    #[serde(default)]
    pub notes: Vec<Vec<Option<String>>>,
    #[serde(default, rename = "storeIds")]
    pub store_ids: StoreIds,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct StoreIds {
    #[serde(default)]
    pub steam: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct AwacyUpdate {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub date: String,
}

impl AwacyGame {
    pub fn url(&self) -> String {
        format!("https://areweanticheatyet.com/game/{}", self.slug)
    }

    /// 最近两条更新。日期格式不统一（ISO 与 `Oct 31, 2024, 4:07 PM UTC` 混用），
    /// 排序前先归一化成可比较的键。
    pub fn recent_updates(&self) -> Vec<&AwacyUpdate> {
        let mut updates: Vec<&AwacyUpdate> = self.updates.iter().collect();
        updates.sort_by_key(|u| std::cmp::Reverse(sort_key(&u.date)));
        updates.into_iter().take(2).collect()
    }
}

/// 把两种日期写法都压成 `YYYYMMDD` 数字用于排序，认不出的排到最后。
fn sort_key(raw: &str) -> u32 {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let trimmed = raw.trim();
    // ISO: 2022-03-01T18:00:27+00:00
    if trimmed.len() >= 10 && trimmed.as_bytes().get(4) == Some(&b'-') {
        let digits: String = trimmed[..10].chars().filter(char::is_ascii_digit).collect();
        if let Ok(value) = digits.parse() {
            return value;
        }
    }
    // 人类可读: Oct 31, 2024, 4:07 PM UTC
    let lower = trimmed.to_ascii_lowercase();
    let mut parts = lower.split([' ', ',']).filter(|p| !p.is_empty());
    let month = parts.next().and_then(|m| {
        MONTHS
            .iter()
            .position(|candidate| m.starts_with(candidate))
            .map(|index| index as u32 + 1)
    });
    let day: Option<u32> = parts.next().and_then(|d| d.parse().ok());
    let year: Option<u32> = parts.next().and_then(|y| y.parse().ok());
    match (year, month, day) {
        (Some(y), Some(m), Some(d)) if (1970..=2999).contains(&y) => y * 10000 + m * 100 + d,
        _ => 0,
    }
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

async fn load_table(paths: &MiyuPaths) -> Result<Vec<AwacyGame>> {
    let cache_path = paths.cache_dir.join(CACHE_FILE);
    let fresh = tokio::fs::metadata(&cache_path)
        .await
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < CACHE_TTL);

    if fresh {
        if let Ok(raw) = tokio::fs::read(&cache_path).await {
            if let Ok(games) = serde_json::from_slice::<Vec<AwacyGame>>(&raw) {
                return Ok(games);
            }
            // 缓存损坏不是致命错误，重新拉一次即可。
        }
    }

    let response = http_response::shared_client()
        .get(GAMES_JSON)
        .send()
        .await?
        .error_for_status()?;
    let raw = http_response::read_bytes(response, MAX_BYTES).await?;
    let games: Vec<AwacyGame> =
        serde_json::from_slice(&raw).context("解析 AWACY games.json 失败")?;

    if let Some(parent) = cache_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::write(&cache_path, &raw).await;
    Ok(games)
}

/// 按 Steam AppID 优先、游戏名兜底查找。返回 `None` 表示 AWACY 未收录——
/// 这不等于该游戏没有反作弊，只代表没有记录，调用方必须把两者区分开。
pub(super) async fn lookup(
    paths: &MiyuPaths,
    app_id: Option<u64>,
    name: &str,
) -> Result<Option<AwacyGame>> {
    let games = load_table(paths).await?;

    if let Some(app_id) = app_id {
        let wanted = app_id.to_string();
        if let Some(hit) = games
            .iter()
            .find(|g| g.store_ids.steam.as_deref() == Some(wanted.as_str()))
        {
            return Ok(Some(hit.clone()));
        }
    }

    // 全表约四成条目没有 Steam ID（Valorant、原神这类非 Steam 游戏），
    // 名称匹配是它们唯一的入口。
    let wanted = normalize(name);
    if !wanted.is_empty() {
        if let Some(hit) = games.iter().find(|g| normalize(&g.name) == wanted) {
            return Ok(Some(hit.clone()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_key_handles_both_date_shapes() {
        assert_eq!(sort_key("2022-03-01T18:00:27+00:00"), 20220301);
        assert_eq!(sort_key("Oct 31, 2024, 4:07 PM UTC"), 20241031);
        assert_eq!(sort_key("Fri, 26 Jan 2024 01:27:06 GMT"), 0);
        assert_eq!(sort_key(""), 0);
    }

    #[test]
    fn recent_updates_are_newest_first_and_capped() {
        let game = AwacyGame {
            name: "x".into(),
            slug: "x".into(),
            status: "Denied".into(),
            anticheats: vec![],
            native: false,
            date_changed: String::new(),
            updates: vec![
                AwacyUpdate {
                    name: "old".into(),
                    date: "2022-03-01T18:00:27+00:00".into(),
                },
                AwacyUpdate {
                    name: "new".into(),
                    date: "Oct 31, 2024, 4:07 PM UTC".into(),
                },
                AwacyUpdate {
                    name: "middle".into(),
                    date: "2023-01-01T00:00:00+00:00".into(),
                },
            ],
            notes: vec![],
            store_ids: StoreIds::default(),
        };
        let recent = game.recent_updates();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].name, "new");
        assert_eq!(recent[1].name, "middle");
    }

    #[test]
    fn normalize_ignores_punctuation_and_case() {
        assert_eq!(normalize("Apex Legends™"), "apexlegends");
        assert_eq!(normalize("Baldur's Gate 3"), "baldursgate3");
    }
}
