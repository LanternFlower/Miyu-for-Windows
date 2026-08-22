use super::{awacy_query, caniplayonlinux_query, http_response, ToolRegistry, ToolSpec};
use crate::paths::MiyuPaths;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::fmt::Write as _;

const PROTONDB_BASE: &str = "https://www.protondb.com";
const ALGOLIA_URL: &str = "https://94he6yatei-dsn.algolia.net/1/indexes/steamdb/query";
const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_REPORTS: usize = 8;
const MAX_REPORTS: usize = 40;
const NOTE_CAP: usize = 300;

const TOOL_DESC: &str = "查询某个游戏在 Linux 上的兼容性。一次调用并发汇总三个来源：ProtonDB 的评级与玩家报告、AreWeAntiCheatYet 的反作弊状态（Supported/Running/Planned/Broken/Denied）、caniplayonlinux 的结论快照。支持 Steam App ID（数字）或游戏名称，中文名可直接查询。返回 Markdown。内容来自第三方站点实时解析，未返回的字段视为未知，不要编造；某个来源缺失不等于结论为否。";

/// 三个来源合并成一件 `game_compat`(08-17)：模型问的始终是同一个问题（"这游戏
/// 在 Linux 上能不能跑"），分成多个工具只是逼它先挑数据源。
///
/// 不提供选择数据源的参数(08-19)：三源并发一共约 2 秒、1.5KB，为省这点再分单源
/// 查是过度设计。唯一的旋钮是 `max_reports`——只有 ProtonDB 评论的体积随游戏
/// 热度线性增长（Apex 1776 条 vs 冷门游戏 5 条），其余两源都是定长。
pub fn register(registry: &mut ToolRegistry, paths: MiyuPaths) {
    registry.register(ToolSpec::new(
        "game_compat",
        TOOL_DESC,
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Steam App ID 或游戏名称。" },
                "max_reports": {
                    "type": "integer",
                    "description": "ProtonDB 评论条数，默认 8，最大 40。只要结论不要评论时填 0。"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths.clone();
            async move { game_compat(args, paths).await }
        },
    )
    // 只有 query 一个必填项，形状简单到不值得让模型先取契约再调。
    .with_stub_example(r#"{"query":"艾尔登法环"}"#));
}

// ── 数据结构 ─────────────────────────────────────────

struct GameIdentity {
    app_id: u64,
    name: String,
    native_linux: bool,
    /// Algolia 返回的第一条明显对不上时的警告。非 Steam 游戏（Valorant）搜出来
    /// 会是毫不相关的条目，必须让调用方看见。
    mismatch: Option<String>,
}

struct ProtonDbData {
    tier: Option<String>,
    trending: Option<String>,
    best: Option<String>,
    reports_total: u64,
    reports: Vec<Report>,
    reports_error: Option<String>,
}

struct Report {
    date: String,
    verdict: &'static str,
    proton: Option<String>,
    launch: Option<String>,
    faults: Vec<&'static str>,
    note: Option<String>,
}

// ── 入口 ─────────────────────────────────────────────

async fn game_compat(args: Value, paths: MiyuPaths) -> Result<String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if query.is_empty() {
        bail!("missing required argument: query");
    }
    let max_reports = args
        .get("max_reports")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).min(MAX_REPORTS))
        .unwrap_or(DEFAULT_REPORTS);

    // 两阶段：ProtonDB 先把查询解析成英文名和 AppID，另两个源再拿解析结果去
    // 匹配。中文名（"黑神话悟空"）既生成不了 caniplayonlinux 的 slug，也匹配
    // 不上 AWACY 的英文条目，所以不能三源无脑并发。
    let identity = resolve_identity(&query).await;
    // 误配警告的含义就是"这不是你要的游戏"，那就不能拿这个名字和 AppID 去查
    // 另两源——否则搜 Valorant 会拿 Algolia 误配到的 NoFlash 去查反作弊，
    // 而 AWACY 其实按原始名字能精确命中。
    let trustworthy = identity
        .as_ref()
        .map(|id| id.mismatch.is_none())
        .unwrap_or(false);
    let resolved_name = match identity.as_ref() {
        Ok(id) if trustworthy => id.name.clone(),
        _ => query.clone(),
    };
    let app_id = identity.as_ref().ok().map(|id| id.app_id);
    let lookup_app_id = if trustworthy { app_id } else { None };

    // ProtonDB 的后续请求与另两源可以并发——它们只依赖已解析出的名字/AppID。
    let protondb_task = async {
        match app_id {
            Some(app_id) => fetch_protondb(app_id, max_reports).await,
            // 没解析出 AppID 本身就是"这个源没有结果"，不是故障。
            None => Ok(None),
        }
    };
    let (protondb, awacy, cipol) = tokio::join!(
        protondb_task,
        awacy_query::lookup(&paths, lookup_app_id, &resolved_name),
        caniplayonlinux_query::lookup(&paths, &resolved_name),
    );

    Ok(render(&query, identity, protondb, awacy, cipol))
}

// ── ProtonDB ─────────────────────────────────────────

async fn fetch_json(url: &str) -> Result<Value> {
    let response = http_response::shared_client()
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    http_response::read_json(response, MAX_JSON_BYTES).await
}

async fn resolve_identity(query: &str) -> Result<GameIdentity> {
    let numeric = query.chars().all(|c| c.is_ascii_digit());
    let hits = algolia_search(query, if numeric { 1 } else { 5 }).await?;
    let hit = hits
        .first()
        .ok_or_else(|| anyhow::anyhow!("no Steam match for {query:?}"))?;

    let app_id: u64 = if numeric {
        query.parse()?
    } else {
        hit["objectID"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("invalid objectID in search result"))?
    };
    let name = hit["name"].as_str().unwrap_or(query).to_string();
    let oslist: Vec<String> = hit["oslist"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_ascii_lowercase))
                .collect()
        })
        .unwrap_or_default();

    // 相关性校验：Algolia 对不在 Steam 的游戏会返回毫不相关的第一条
    // （搜 "Valorant" 得到 "NoFlash"）。非拉丁查询无法直接比对
    // （"黑神话悟空" vs "Black Myth: Wukong"），跳过。
    let mismatch = if numeric || !query.is_ascii() {
        None
    } else {
        let similarity = token_overlap(query, &name);
        (similarity < 0.5).then(|| {
            let others: Vec<&str> = hits
                .iter()
                .skip(1)
                .take(3)
                .filter_map(|h| h["name"].as_str())
                .collect();
            let mut msg = format!(
                "search for {query:?} returned {name:?}; may not be the same game \
                 (it might not be on Steam)"
            );
            if !others.is_empty() {
                let _ = write!(msg, ". Other candidates: {}", others.join(", "));
            }
            msg
        })
    };

    Ok(GameIdentity {
        app_id,
        name,
        native_linux: oslist.iter().any(|os| os == "linux"),
        mismatch,
    })
}

fn token_overlap(query: &str, name: &str) -> f64 {
    let normalize = |value: &str| -> Vec<String> {
        value
            .to_ascii_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(str::to_string)
            .collect()
    };
    let left = normalize(query);
    let right = normalize(name);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let matched = left.iter().filter(|token| right.contains(token)).count();
    matched as f64 / left.len() as f64
}

async fn algolia_search(query: &str, hits: usize) -> Result<Vec<Value>> {
    let body = json!({
        "query": query,
        "facetFilters": [["appType:Game"]],
        "hitsPerPage": hits,
        "attributesToRetrieve": ["name", "objectID", "oslist"],
        "page": 0,
    });
    let response = http_response::shared_client()
        .post(ALGOLIA_URL)
        .header("x-algolia-api-key", "9ba0e69fb2974316cdaec8f5f257088f")
        .header("x-algolia-application-id", "94HE6YATEI")
        .header("Referer", PROTONDB_BASE)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let value: Value = http_response::read_json(response, MAX_JSON_BYTES).await?;
    Ok(value["hits"].as_array().cloned().unwrap_or_default())
}

/// `Ok(None)` = ProtonDB 没有这个游戏的数据（summary 返回 404），与"抓取失败"
/// 是两回事，渲染时分别落到 `no result` 和错误行。
async fn fetch_protondb(app_id: u64, max_reports: usize) -> Result<Option<ProtonDbData>> {
    let url = format!("{PROTONDB_BASE}/api/v1/reports/summaries/{app_id}.json");
    let response = http_response::shared_client().get(&url).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let summary: Value =
        http_response::read_json(response.error_for_status()?, MAX_JSON_BYTES).await?;

    let as_tier = |key: &str| summary[key].as_str().map(str::to_string);
    let mut data = ProtonDbData {
        tier: as_tier("tier"),
        trending: as_tier("trendingTier"),
        best: as_tier("bestReportedTier"),
        reports_total: summary["total"].as_u64().unwrap_or(0),
        reports: Vec::new(),
        reports_error: None,
    };

    if max_reports == 0 {
        return Ok(Some(data));
    }
    match fetch_reports(app_id).await {
        Ok(raw) => {
            if let Some(total) = raw["total"].as_u64() {
                data.reports_total = total;
            }
            data.reports = raw["reports"]
                .as_array()
                .map(|items| items.iter().take(max_reports).map(extract_report).collect())
                .unwrap_or_default();
        }
        // 拉取失败与"确实没人评论"必须可区分,否则模型会把故障当成零评论。
        Err(err) => data.reports_error = Some(err.to_string()),
    }
    Ok(Some(data))
}

async fn fetch_reports(app_id: u64) -> Result<Value> {
    let counts = fetch_json(&format!("{PROTONDB_BASE}/data/counts.json")).await?;
    let reports_count = counts["reports"].as_u64().unwrap_or(0);
    let timestamp = counts["timestamp"].as_u64().unwrap_or(0);
    if reports_count == 0 || timestamp == 0 {
        bail!("invalid counts.json");
    }
    let pid = calculate_protondb_id(app_id, reports_count, timestamp);
    fetch_json(&format!(
        "{PROTONDB_BASE}/data/reports/all-devices/app/{pid}.json"
    ))
    .await
}

/// Hash computation — reverse-engineered from ProtonDB frontend JS.
/// R(e, t, n) = t + "p" + (e * (t % n))
fn hash_r(e: u64, t: u64, n: u64) -> String {
    format!("{}p{}", t, e.wrapping_mul(t % n))
}

/// I(s) = abs(foldl(s + "m", |0, (acc, ch) => ((acc << 5) - acc + charCode) | 0))
/// JS `| 0` truncates to signed 32-bit; we replicate with i32 wrapping arithmetic.
fn hash_i(s: &str) -> u32 {
    let mut val: i32 = 0;
    for ch in s.chars().chain(['m']) {
        val = val
            .wrapping_shl(5)
            .wrapping_sub(val)
            .wrapping_add(ch as i32);
    }
    val.unsigned_abs()
}

fn calculate_protondb_id(steam_id: u64, reports_count: u64, timestamp: u64) -> u32 {
    let h1 = hash_r(steam_id, reports_count, timestamp);
    let h2 = hash_r(1, steam_id, timestamp);
    hash_i(&format!("p{h1}*vRT{h2}undefined"))
}

const FAULTS: [(&str, &str); 8] = [
    ("audioFaults", "audio"),
    ("graphicalFaults", "graphics"),
    ("windowingFaults", "windowing"),
    ("inputFaults", "input"),
    ("saveGameFaults", "save_game"),
    ("performanceFaults", "performance"),
    ("stabilityFaults", "stability"),
    ("significantBugs", "bugs"),
];

fn extract_report(raw: &Value) -> Report {
    let responses = &raw["responses"];
    let notes = &responses["notes"];

    let verdict_value = responses["verdictOob"]
        .as_str()
        .or_else(|| responses["verdict"].as_str());
    let verdict = if responses["startsPlay"].as_str() != Some("yes") {
        "broken"
    } else {
        match verdict_value {
            Some("yes") => "recommended",
            Some(_) => "not_recommended",
            None => "unknown",
        }
    };

    let proton = match responses["variant"].as_str() {
        Some("experimental") => Some("Proton Experimental".to_string()),
        Some("ge") => responses["customProtonVersion"]
            .as_str()
            .map(str::to_string),
        _ => responses["protonVersion"].as_str().map(str::to_string),
    };

    let non_empty = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    let note = non_empty(
        notes["concludingNotes"]
            .as_str()
            .or_else(|| responses["concludingNotes"].as_str()),
    )
    .map(|note| flatten(&note, NOTE_CAP));

    Report {
        date: format_date(raw["timestamp"].as_u64().unwrap_or(0)),
        verdict,
        proton,
        launch: non_empty(responses["launchOptions"].as_str()).map(|l| flatten(&l, NOTE_CAP)),
        faults: FAULTS
            .iter()
            .filter(|(key, _)| responses[*key].as_str() == Some("yes"))
            .map(|(_, label)| *label)
            .collect(),
        note,
    }
}

/// 玩家评论是自由文本，会自带换行，也可能以 Markdown 结构字符开头。
/// 压成一行并中和行首——否则会串出所在的列表项。
fn flatten(value: &str, cap: usize) -> String {
    let mut out = String::with_capacity(value.len().min(cap) + 1);
    let mut last_space = true;
    for ch in value.chars() {
        if out.chars().count() >= cap {
            out.push('…');
            break;
        }
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    let trimmed = out.trim().to_string();
    match trimmed.chars().next() {
        Some(first) if "#>-*+|=".contains(first) => format!("\\{trimmed}"),
        _ => trimmed,
    }
}

fn format_date(timestamp: u64) -> String {
    if timestamp == 0 {
        return "unknown".to_string();
    }
    let (year, month, day) = days_to_ymd(timestamp as i64 / 86400);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert days since Unix epoch (1970-01-01) to (year, month, day).
fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    // Algorithm from Howard Hinnant's date library (civil_from_days)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

// ── Markdown 渲染 ────────────────────────────────────

/// 输出 Markdown 而非 JSON：同样的信息省约一半 token，模型也不用从嵌套结构里
/// 找字段。正文一律英文——三个数据源本身都是英文，混排中文只多一层翻译损耗；
/// 小节名直接用来源本名。
fn render(
    query: &str,
    identity: Result<GameIdentity>,
    protondb: Result<Option<ProtonDbData>>,
    awacy: Result<Option<awacy_query::AwacyGame>>,
    cipol: Result<Option<caniplayonlinux_query::CipolEntry>>,
) -> String {
    let mut out = String::new();

    match &identity {
        // 误配时标题保留用户问的名字：把 Algolia 搜岔的游戏名印成标题，等于
        // 替模型把错答案坐实了。
        Ok(id) if id.mismatch.is_some() => {
            let _ = writeln!(out, "# {query}");
            let _ = writeln!(out, "\n> ⚠ {}", id.mismatch.as_deref().unwrap_or_default());
        }
        Ok(id) => {
            let _ = writeln!(out, "# {}", id.name);
            let mut meta = format!("appid {}", id.app_id);
            if id.native_linux {
                meta.push_str(" · native Linux build");
            }
            let _ = writeln!(out, "{meta}");
        }
        Err(_) => {
            let _ = writeln!(out, "# {query}");
        }
    }

    // ── protondb ──
    match (&identity, &protondb) {
        (Ok(_), Ok(Some(data))) => {
            let tier = data.tier.as_deref().unwrap_or("unknown");
            let mut bits = vec![tier.to_string()];
            if let Some(trending) = data.trending.as_deref().filter(|t| *t != tier) {
                bits.push(format!("trending {trending}"));
            }
            if let Some(best) = data.best.as_deref().filter(|b| *b != tier) {
                bits.push(format!("best {best}"));
            }
            bits.push(format!("{} reports", data.reports_total));
            let app_id = identity.as_ref().map(|id| id.app_id).unwrap_or(0);
            let _ = writeln!(out, "\n## protondb — {}", bits.join(" · "));
            let _ = writeln!(out, "{PROTONDB_BASE}/app/{app_id}");
            if let Some(err) = &data.reports_error {
                let _ = writeln!(out, "reports fetch failed: {err}");
            }
            if !data.reports.is_empty() {
                let _ = writeln!(out, "\n### reports");
                for report in &data.reports {
                    let mut head = format!(
                        "- {} {} {}",
                        report.date,
                        report.verdict,
                        report.proton.as_deref().unwrap_or("unknown")
                    );
                    if !report.faults.is_empty() {
                        let _ = write!(head, " [{}]", report.faults.join(","));
                    }
                    let _ = writeln!(out, "{head}");
                    // 启动参数和评论正文长度不可控，各占一行；两空格缩进仍属
                    // 同一列表项。
                    if let Some(launch) = &report.launch {
                        let _ = writeln!(out, "  launch: {launch}");
                    }
                    if let Some(note) = &report.note {
                        let _ = writeln!(out, "  {note}");
                    }
                }
            }
        }
        // 查无此游戏（Algolia 没解析出 AppID、或 ProtonDB summary 404）不是故障，
        // 与真正的抓取失败分开渲染。
        (Ok(_), Ok(None)) => {
            let _ = writeln!(out, "\n## protondb — no result");
        }
        (Ok(_), Err(err)) | (Err(err), _) => {
            let _ = writeln!(out, "\n## protondb — error\n{err}");
        }
    }

    // ── areweanticheatyet ──
    match &awacy {
        Ok(Some(game)) => {
            let anticheats = if game.anticheats.is_empty() {
                "none listed".to_string()
            } else {
                game.anticheats.join(", ")
            };
            let _ = writeln!(
                out,
                "\n## areweanticheatyet — {} · {anticheats}",
                game.status
            );
            let date = game.date_changed.get(..10).unwrap_or(&game.date_changed);
            let _ = writeln!(out, "changed {date} · {}", game.url());
            for update in game.recent_updates() {
                let _ = writeln!(
                    out,
                    "- {} ({})",
                    flatten(&update.name, NOTE_CAP),
                    update.date
                );
            }
            for note in game.notes.iter().take(3) {
                if let Some(text) = note.first().and_then(Option::as_deref) {
                    let _ = writeln!(out, "- {}", flatten(text, NOTE_CAP));
                }
            }
        }
        // "AWACY 未收录不等于没有反作弊"这条读法写在工具描述和 skill 里，
        // 不在每次返回值里重复。
        Ok(None) => {
            let _ = writeln!(out, "\n## areweanticheatyet — no result");
        }
        Err(err) => {
            let _ = writeln!(out, "\n## areweanticheatyet — error\n{err}");
        }
    }

    // ── caniplayonlinux ──
    match &cipol {
        Ok(Some(entry)) => {
            let mut head = "\n## caniplayonlinux".to_string();
            if let Some(verified) = &entry.last_verified {
                let _ = write!(head, " — verified {verified}");
            }
            let _ = writeln!(out, "{head}");
            if let Some(summary) = &entry.summary {
                let _ = writeln!(out, "{}", flatten(summary, 600));
            }
            let _ = writeln!(out, "{}", entry.url);
        }
        Ok(None) => {
            let _ = writeln!(out, "\n## caniplayonlinux — no result");
        }
        Err(err) => {
            let _ = writeln!(out, "\n## caniplayonlinux — error\n{err}");
        }
    }

    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_r() {
        assert_eq!(hash_r(1, 2, 3), "2p2");
    }

    #[test]
    fn test_hash_i_basic() {
        assert!(hash_i("test") > 0);
    }

    #[test]
    fn test_days_to_ymd() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        assert_eq!(days_to_ymd(20089), (2025, 1, 1));
        assert_eq!(days_to_ymd(19782), (2024, 2, 29));
    }

    #[test]
    fn test_format_date() {
        assert_eq!(format_date(0), "unknown");
        assert_eq!(format_date(1735689600), "2025-01-01");
    }

    #[test]
    fn test_extract_report_basic() {
        let report = extract_report(&json!({
            "timestamp": 1735689600,
            "responses": {
                "startsPlay": "yes",
                "verdict": "yes",
                "variant": "ge",
                "customProtonVersion": "GE-Proton9-4",
                "notes": { "concludingNotes": "Works great!" }
            }
        }));
        assert_eq!(report.verdict, "recommended");
        assert_eq!(report.proton.as_deref(), Some("GE-Proton9-4"));
        assert_eq!(report.note.as_deref(), Some("Works great!"));
    }

    #[test]
    fn test_broken_report() {
        let report = extract_report(&json!({
            "timestamp": 1735689600,
            "responses": { "startsPlay": "no", "installs": "no" }
        }));
        assert_eq!(report.verdict, "broken");
    }

    #[test]
    fn test_faults_only_include_yes() {
        let report = extract_report(&json!({
            "timestamp": 1735689600,
            "responses": {
                "startsPlay": "yes", "verdict": "yes",
                "audioFaults": "yes", "graphicalFaults": "no", "inputFaults": "yes"
            }
        }));
        // 取值为 "no" 的 fault 不该出现——原实现把八个字段全量传出，
        // 其中大半是纯噪声。
        assert_eq!(report.faults, vec!["audio", "input"]);
    }

    #[test]
    fn flatten_collapses_newlines_and_escapes_markdown() {
        assert_eq!(flatten("line one\n\nline two", 300), "line one line two");
        assert_eq!(
            flatten("- looks like a bullet", 300),
            "\\- looks like a bullet"
        );
        assert_eq!(flatten("## heading", 300), "\\## heading");
    }

    #[test]
    fn flatten_caps_long_notes() {
        let long = "x".repeat(500);
        let out = flatten(&long, 300);
        assert_eq!(out.chars().count(), 301);
        assert!(out.ends_with('…'));
    }

    /// 联网探针：`cargo test --lib live_probe -- --ignored --nocapture`
    ///
    /// **会真的去抓 ProtonDB / AWACY / caniplayonlinux**，所以是 `#[ignore]`：
    /// 默认不跑，别把人家站点当 CI 的一部分。
    #[tokio::test]
    #[ignore]
    async fn live_probe() {
        let temp = tempfile::tempdir().unwrap();
        let paths = super::super::tests::test_paths(temp.path());
        for (query, why) in [
            ("黑神话悟空", "中文名 + AWACY 未收录"),
            ("Apex Legends", "反作弊 Denied + 趋势下滑"),
            ("Valorant", "非 Steam：ProtonDB 失败，另两源仍有结果"),
            ("Dota 2", "原生 Linux 版本"),
        ] {
            let started = std::time::Instant::now();
            let out = game_compat(json!({ "query": query, "max_reports": 3 }), paths.clone())
                .await
                .unwrap();
            println!(
                "\n═══ {query} — {why} ({:.2}s)\n{out}",
                started.elapsed().as_secs_f64()
            );
        }
    }

    #[test]
    fn token_overlap_flags_unrelated_search_hits() {
        // 真实案例：Valorant 不在 Steam，Algolia 返回 NoFlash。
        assert!(token_overlap("Valorant", "NoFlash") < 0.5);
        assert!(token_overlap("Apex Legends", "Apex Legends™") >= 0.5);
        assert!(token_overlap("Elden Ring", "ELDEN RING") >= 0.5);
    }
}
