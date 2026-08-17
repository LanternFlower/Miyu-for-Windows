use super::{html_conversion, http_response, ToolRegistry, ToolSpec};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::time::Duration;

const BASE_URL: &str = "https://caniplayonlinux.com";
const TOOL_NAME: &str = "query_caniplayonlinux";
const TOOL_DISPLAY_NAME: &str = "查询是否能在Linux上玩";
const PAGE_SIZE: usize = 24;
const MAX_LIMIT: usize = 10;

const TOOL_DESC: &str = "查询 caniplayonlinux.com 的实时 Linux 游戏兼容性信息。适用于用户询问某个游戏是否能在 Linux 上玩、是否可通过 Proton 运行、是否 Steam Deck Verified、推荐 Proton 版本、是否有已知 Linux 问题或修复方法的场景。该工具只读抓取网页并返回结构化结果，包括标题、来源链接、兼容性结论、Proton 推荐、Steam Deck 状态、摘要、备注、已知问题、修复建议和验证时间等可用字段。返回内容来自第三方站点实时解析；不要编造缺失字段，未返回的信息应视为未知。允许子代理调用。";

#[derive(Clone, Debug)]
struct GameEntry {
    title: String,
    url: String,
}

#[derive(Clone, Debug)]
struct ScoredGame {
    entry: GameEntry,
    score: i64,
    reason: String,
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(create_toolspec());
}

pub fn create_toolspec() -> ToolSpec {
    ToolSpec::new(
        TOOL_NAME,
        TOOL_DESC,
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "要查询的游戏名称或部分标题。"
                },
                "limit": {
                    "type": "integer",
                    "description": "最多返回的匹配结果数，默认 5，最大 10。"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        |args| async move { query_caniplayonlinux(args).await },
    )
    .with_display_name(TOOL_DISPLAY_NAME)
}

async fn query_caniplayonlinux(args: Value) -> Result<String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if query.is_empty() {
        bail!("missing required argument: query");
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, MAX_LIMIT as u64) as usize;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .user_agent("miyu-caniplayonlinux-query/0.1")
        .build()?;

    let first_html = fetch_text(&client, &format!("{BASE_URL}/games/")).await?;
    let total_games = extract_total_games(&first_html).unwrap_or(PAGE_SIZE);
    let pages = total_games.div_ceil(PAGE_SIZE).max(1);
    let mut all_games = extract_games_from_list_page(&first_html);
    let mut warnings = Vec::new();

    for page in 2..=pages {
        let url = format!("{BASE_URL}/games/{page}/");
        match fetch_text(&client, &url).await {
            Ok(html) => all_games.extend(extract_games_from_list_page(&html)),
            Err(err) => warnings.push(json!({
                "kind": "page_fetch_failed",
                "url": url,
                "error": err.to_string(),
            })),
        }
    }

    let mut matches = unique_by_url(all_games)
        .into_iter()
        .filter_map(|entry| score_game(entry, query))
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.entry.title.cmp(&b.entry.title))
    });
    matches.truncate(limit);

    let mut results = Vec::new();
    for game in matches {
        match fetch_detail(&client, &game).await {
            Ok(result) => results.push(result),
            Err(err) => {
                warnings.push(json!({
                    "kind": "detail_fetch_failed",
                    "title": game.entry.title,
                    "url": game.entry.url,
                    "error": err.to_string(),
                }));
                results.push(json!({
                    "title": game.entry.title,
                    "url": game.entry.url,
                    "match": {"score": game.score, "reason": game.reason},
                    "metadata": {
                        "sourceUrl": game.entry.url,
                        "dataFreshness": "live_fetch",
                        "confidence": "low",
                        "missingFields": ["detail_page"]
                    }
                }));
            }
        }
    }

    Ok(serde_json::to_string_pretty(&json!({
        "query": query,
        "source": {
            "name": "Can I Play on Linux?",
            "site": "caniplayonlinux.com",
            "mode": "live_fetch_no_database"
        },
        "results": results,
        "warnings": warnings,
    }))?)
}

async fn fetch_detail(client: &reqwest::Client, game: &ScoredGame) -> Result<Value> {
    let html = fetch_text(client, &game.entry.url).await?;
    let title = extract_title(&html).unwrap_or_else(|| game.entry.title.clone());
    let summary = extract_meta_description(&html);
    let verdict = extract_html_verdict(&html);
    let text = html_conversion::to_text_async(html, 120).await?;
    let summary = summary.or_else(|| first_nonempty_line_after(&text, &title));
    let verdict = verdict.or_else(|| extract_text_verdict(&text));
    let recommended_proton = value_after_label(&text, "Recommended Proton")
        .or_else(|| value_after_label(&text, "Proton"));
    let steam_deck_status = extract_steam_deck_status(&text);
    let known_issues = section_excerpt(&text, "Known issues", "Fixes", 1600);
    let fixes = section_excerpt(&text, "Fixes", "Verdict", 1600)
        .or_else(|| section_excerpt(&text, "Fixes", "Details", 1600));
    let last_verified = value_after_label(&text, "Last verified");
    let developer = value_after_label(&text, "Developer");
    let publisher = value_after_label(&text, "Publisher");
    let (year, genres) = extract_year_and_genres(&text);
    let protondb = extract_protondb(&text);
    let notes = extract_notes(summary.as_deref(), &text);
    let missing_fields = missing_fields(&[
        ("compatibility.verdict", verdict.as_deref()),
        (
            "compatibility.recommendedProton",
            recommended_proton.as_deref(),
        ),
        (
            "compatibility.steamDeck.status",
            steam_deck_status.as_deref(),
        ),
        ("metadata.lastVerified", last_verified.as_deref()),
        ("game.developer", developer.as_deref()),
        ("game.publisher", publisher.as_deref()),
    ]);
    let native_linux = summary
        .as_deref()
        .map(mentions_native_linux)
        .unwrap_or_else(|| mentions_native_linux(&text));
    let requires_proton = !native_linux
        && (recommended_proton.is_some()
            || text.to_ascii_lowercase().contains("via proton")
            || text.to_ascii_lowercase().contains("protondb"));

    Ok(json!({
        "title": title,
        "url": game.entry.url,
        "match": {
            "score": game.score,
            "reason": game.reason,
        },
        "compatibility": {
            "verdict": verdict,
            "verdictLabel": verdict_label(verdict.as_deref(), requires_proton, native_linux),
            "playable": playable(verdict.as_deref()),
            "nativeLinux": native_linux,
            "requiresProton": requires_proton,
            "recommendedProton": recommended_proton,
            "steamDeck": {
                "status": steam_deck_status,
                "verified": steam_deck_status.as_deref().map(|status| status.contains("Verified")),
            },
            "antiCheat": extract_anticheat(&text),
        },
        "protondb": protondb,
        "game": {
            "year": year,
            "genres": genres,
            "developer": developer,
            "publisher": publisher,
        },
        "summary": summary,
        "notes": notes,
        "knownIssues": known_issues.map(|item| vec![item]).unwrap_or_default(),
        "fixes": fixes.map(|item| vec![item]).unwrap_or_default(),
        "metadata": {
            "lastVerified": last_verified,
            "sourceUrl": game.entry.url,
            "dataFreshness": "live_fetch",
            "confidence": confidence(verdict.as_deref(), missing_fields.len()),
            "missingFields": missing_fields,
        }
    }))
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let response = client.get(url).send().await?.error_for_status()?;
    http_response::read_text(response, http_response::MAX_HTML_RESPONSE_BYTES).await
}

fn extract_games_from_list_page(html: &str) -> Vec<GameEntry> {
    let mut games = extract_games_from_json_ld(html);
    if !games.is_empty() {
        return games;
    }

    let mut cursor = 0;
    while let Some(relative) = html[cursor..].find("<a") {
        let start = cursor + relative;
        let Some(tag_end_rel) = html[start..].find('>') else {
            break;
        };
        let tag_end = start + tag_end_rel + 1;
        let tag = &html[start..tag_end];
        let Some(path) = attr_value(tag, "href") else {
            cursor = tag_end;
            continue;
        };
        if !is_game_detail_path(&path) {
            cursor = tag_end;
            continue;
        }
        let Some(close_rel) = html[tag_end..].find("</a>") else {
            break;
        };
        let close = tag_end + close_rel;
        let inner = &html[tag_end..close];
        let title = extract_heading(inner).unwrap_or_else(|| strip_tags(inner));
        if !title.is_empty() {
            games.push(GameEntry {
                title,
                url: absolute_url(&path),
            });
        }
        cursor = close + "</a>".len();
    }
    games
}

fn extract_games_from_json_ld(html: &str) -> Vec<GameEntry> {
    let mut games = Vec::new();
    let mut cursor = 0;
    let marker = "application/ld+json";
    while let Some(marker_rel) = html[cursor..].find(marker) {
        let marker_pos = cursor + marker_rel;
        let Some(open_rel) = html[marker_pos..].find('>') else {
            break;
        };
        let json_start = marker_pos + open_rel + 1;
        let Some(close_rel) = html[json_start..].find("</script>") else {
            break;
        };
        let json_end = json_start + close_rel;
        if let Ok(value) = serde_json::from_str::<Value>(&html[json_start..json_end]) {
            if value["@type"].as_str() == Some("ItemList") {
                if let Some(items) = value["itemListElement"].as_array() {
                    for item in items {
                        let title = item["name"].as_str().unwrap_or_default().trim();
                        let url = item["url"].as_str().unwrap_or_default().trim();
                        if !title.is_empty() && !url.is_empty() {
                            games.push(GameEntry {
                                title: decode_entities(title),
                                url: url.to_string(),
                            });
                        }
                    }
                }
            }
        }
        cursor = json_end + "</script>".len();
    }
    games
}

fn extract_total_games(html: &str) -> Option<usize> {
    if let Some(pos) = html.find("\"numberOfItems\":") {
        let rest = &html[pos + "\"numberOfItems\":".len()..];
        let digits = rest
            .chars()
            .skip_while(|ch| ch.is_whitespace())
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if let Ok(value) = digits.parse() {
            return Some(value);
        }
    }
    let needle = " games in the database";
    let pos = html.find(needle)?;
    let prefix = &html[..pos];
    let digits = prefix
        .chars()
        .rev()
        .take_while(|ch| ch.is_ascii_digit() || *ch == ',')
        .collect::<String>()
        .chars()
        .rev()
        .filter(|ch| *ch != ',')
        .collect::<String>();
    digits.parse().ok()
}

fn score_game(entry: GameEntry, query: &str) -> Option<ScoredGame> {
    let title = entry.title.to_ascii_lowercase();
    let query = query.trim().to_ascii_lowercase();
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    let (score, reason) = match title.cmp(&query) {
        Ordering::Equal => (1000, "exact title match"),
        _ if title.starts_with(&query) => {
            (800 - entry.title.len() as i64, "title starts with query")
        }
        _ if title.contains(&query) => (
            600 - title.find(&query).unwrap_or_default() as i64,
            "title contains query",
        ),
        _ if tokens.len() > 1 && tokens.iter().all(|token| title.contains(token)) => (
            500 - (title.len() as i64 - query.len() as i64).abs(),
            "title contains all query terms",
        ),
        _ if tokens.len() == 1 && tokens.iter().any(|token| title.contains(token)) => (
            100 - (title.len() as i64 - query.len() as i64).abs(),
            "title contains query term",
        ),
        _ => return None,
    };
    Some(ScoredGame {
        entry,
        score,
        reason: reason.to_string(),
    })
}

fn unique_by_url(games: Vec<GameEntry>) -> Vec<GameEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();
    for game in games {
        if seen.insert(game.url.clone()) {
            unique.push(game);
        }
    }
    unique
}

fn attr_value(tag: &str, attr: &str) -> Option<String> {
    for quote in ['\"', '\''] {
        let marker = format!("{attr}={quote}");
        if let Some(pos) = tag.find(&marker) {
            let start = pos + marker.len();
            let end = tag[start..].find(quote)?;
            return Some(tag[start..start + end].to_string());
        }
    }
    None
}

fn is_game_detail_path(path: &str) -> bool {
    if !path.starts_with("/games/") {
        return false;
    }
    if path.trim_matches('/').split('/').count() != 2 {
        return false;
    }
    let slug = path.trim_matches('/').trim_start_matches("games/");
    !matches!(
        slug,
        "native"
            | "works"
            | "partial"
            | "broken"
            | "steam-deck-verified"
            | "action"
            | "rpg"
            | "strategy"
            | "simulation"
            | "indie"
            | "puzzle"
            | "adventure"
            | "fps"
            | "racing"
            | "sports"
            | "survival"
    ) && !slug.chars().all(|ch| ch.is_ascii_digit())
}

fn absolute_url(path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else {
        format!("{BASE_URL}{path}")
    }
}

fn extract_heading(html: &str) -> Option<String> {
    for tag in ["h1", "h2", "h3"] {
        let start_marker = format!("<{tag}");
        let Some(start) = html.find(&start_marker) else {
            continue;
        };
        let Some(open_end_rel) = html[start..].find('>') else {
            continue;
        };
        let content_start = start + open_end_rel + 1;
        let close_marker = format!("</{tag}>");
        let Some(close_rel) = html[content_start..].find(&close_marker) else {
            continue;
        };
        let content = strip_tags(&html[content_start..content_start + close_rel]);
        if !content.is_empty() {
            return Some(content);
        }
    }
    None
}

fn extract_title(html: &str) -> Option<String> {
    extract_heading(html).or_else(|| {
        let start = html.find("<title>")? + "<title>".len();
        let end = html[start..].find("</title>")?;
        Some(
            strip_tags(&html[start..start + end])
                .split('|')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string(),
        )
    })
}

fn extract_meta_description(html: &str) -> Option<String> {
    let marker = "name=\"description\"";
    let pos = html
        .find(marker)
        .or_else(|| html.find("name='description'"))?;
    let tag_end = html[pos..].find('>')?;
    attr_value(&html[pos..pos + tag_end], "content").map(|value| decode_entities(&value))
}

fn extract_html_verdict(html: &str) -> Option<String> {
    let classes = [
        ("badge-native", "Native"),
        ("badge-works", "Works"),
        ("badge-partial", "Partial"),
        ("badge-broken", "Broken"),
        ("badge-unknown", "Unknown"),
    ];
    for (class, verdict) in classes {
        if html.contains(class) {
            return Some(verdict.to_string());
        }
    }
    None
}

fn extract_text_verdict(text: &str) -> Option<String> {
    ["Native", "Works", "Partial", "Broken", "Unknown"]
        .into_iter()
        .find(|verdict| text.lines().any(|line| line.trim() == *verdict))
        .map(str::to_string)
}

fn extract_steam_deck_status(text: &str) -> Option<String> {
    for status in [
        "Steam Deck Verified",
        "Steam Deck Playable",
        "Steam Deck Unsupported",
        "Community standing",
    ] {
        if text.contains(status) {
            return Some(status.to_string());
        }
    }
    value_after_label(text, "Steam Deck")
}

fn extract_year_and_genres(text: &str) -> (Option<u64>, Vec<String>) {
    for line in text.lines().map(str::trim) {
        if line.contains('·') {
            let parts = line.split('·').map(str::trim).collect::<Vec<_>>();
            if let Some(year) = parts.first().and_then(|part| part.parse::<u64>().ok()) {
                let genres = parts
                    .iter()
                    .skip(1)
                    .filter(|part| !part.is_empty())
                    .map(|part| (*part).to_string())
                    .collect::<Vec<_>>();
                if !genres.is_empty() {
                    return (Some(year), genres);
                }
            }
        }
    }
    (None, Vec::new())
}

fn extract_protondb(text: &str) -> Value {
    let lower = text.to_ascii_lowercase();
    let tier = ["platinum", "gold", "silver", "bronze", "borked"]
        .into_iter()
        .find(|tier| lower.contains(tier));
    let reports = extract_reports_count(text);
    json!({
        "tier": tier.map(capitalize),
        "reports": reports,
        "summary": tier.map(|tier| format!("{} ProtonDB signal detected in page text.", capitalize(tier))),
    })
}

fn extract_reports_count(text: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    let pos = lower.find(" reports")?;
    let prefix = &lower[..pos];
    let digits = prefix
        .chars()
        .rev()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| ch.is_ascii_digit() || *ch == ',')
        .collect::<String>()
        .chars()
        .rev()
        .filter(|ch| *ch != ',')
        .collect::<String>();
    digits.parse().ok()
}

fn extract_anticheat(text: &str) -> Value {
    let lower = text.to_ascii_lowercase();
    let name = if lower.contains("easyanticheat") || lower.contains("easy anti-cheat") {
        Some("EasyAntiCheat")
    } else if lower.contains("battleye") || lower.contains("battl-eye") {
        Some("BattlEye")
    } else {
        None
    };
    let supported = if lower.contains("linux/proton mode active")
        || lower.contains("proton-compatible mode")
        || lower.contains("online multiplayer works")
        || lower.contains("co-op multiplayer works")
    {
        Some(true)
    } else if lower.contains("anti-cheat") && (lower.contains("broken") || lower.contains("denied"))
    {
        Some(false)
    } else {
        None
    };
    json!({
        "present": name.is_some(),
        "name": name,
        "linuxProtonSupported": supported,
        "summary": anticheat_summary(name, supported),
    })
}

fn anticheat_summary(name: Option<&str>, supported: Option<bool>) -> Option<String> {
    match (name, supported) {
        (Some(name), Some(true)) => Some(format!(
            "{name} appears to support Linux/Proton for this game based on the parsed page text."
        )),
        (Some(name), Some(false)) => Some(format!(
            "{name} appears to block or break Linux/Proton play based on the parsed page text."
        )),
        (Some(name), None) => Some(format!(
            "{name} is mentioned, but support status was not clear from the parsed page text."
        )),
        _ => None,
    }
}

fn extract_notes(summary: Option<&str>, text: &str) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(summary) = summary {
        for sentence in summary.split(['.', '。']).map(str::trim) {
            if !sentence.is_empty()
                && (sentence.contains("Steam Deck")
                    || sentence.contains("No setup")
                    || sentence.contains("launch")
                    || sentence.contains("anti-cheat")
                    || sentence.contains("multiplayer"))
            {
                notes.push(sentence.to_string());
            }
        }
    }
    if text.contains("No setup required") {
        notes.push("No setup required.".to_string());
    }
    notes.sort();
    notes.dedup();
    notes
}

fn first_nonempty_line_after(text: &str, title: &str) -> Option<String> {
    let mut seen_title = false;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if !seen_title {
            seen_title = line == title;
            continue;
        }
        if line.len() > 30 {
            return Some(line.chars().take(500).collect());
        }
    }
    None
}

fn value_after_label(text: &str, label: &str) -> Option<String> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    while let Some(line) = lines.next() {
        if let Some((key, value)) = line.split_once('│') {
            if key.trim() == label {
                return Some(value.trim().chars().take(180).collect());
            }
        }
        if line == label {
            return lines.next().map(|value| value.chars().take(180).collect());
        }
    }
    None
}

fn section_excerpt(text: &str, start: &str, end: &str, max_chars: usize) -> Option<String> {
    if let Some(section) = markdown_section(text, start, end) {
        return Some(excerpt(&section, max_chars));
    }
    let after = text.split(start).nth(1)?;
    let section = after.split(end).next().unwrap_or(after);
    let value = excerpt(section, max_chars);
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn markdown_section(text: &str, title_prefix: &str, end_prefix: &str) -> Option<String> {
    let mut lines = text.lines();
    let start_level = lines.find_map(|line| {
        let line = line.trim_start();
        let level = line.bytes().take_while(|byte| *byte == b'#').count();
        (level > 0 && line[level..].trim().starts_with(title_prefix)).then_some(level)
    })?;
    let mut section = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        let level = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        if level > 0 && level <= start_level {
            break;
        }
        if trimmed
            .trim_matches('*')
            .trim_start()
            .starts_with(end_prefix)
        {
            break;
        }
        section.push(line);
    }
    let section = section.join("\n");
    (!section.trim().is_empty()).then_some(section)
}

fn excerpt(text: &str, max_chars: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

fn strip_tags(value: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    decode_entities(&out.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn decode_entities(value: &str) -> String {
    // `&amp;` 必须最后解:排第一会把字面 `&amp;lt;` 双重解码成 `<`。
    value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

fn missing_fields(fields: &[(&str, Option<&str>)]) -> Vec<String> {
    fields
        .iter()
        .filter_map(|(name, value)| {
            if value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some()
            {
                None
            } else {
                Some((*name).to_string())
            }
        })
        .collect()
}

fn mentions_native_linux(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("native linux")
        || lower.contains("native build")
        || lower.contains("runs natively")
}

fn playable(verdict: Option<&str>) -> Option<bool> {
    match verdict {
        Some("Native") | Some("Works") => Some(true),
        Some("Broken") => Some(false),
        Some("Partial") | Some("Unknown") => None,
        _ => None,
    }
}

fn verdict_label(
    verdict: Option<&str>,
    requires_proton: bool,
    native_linux: bool,
) -> Option<String> {
    match verdict {
        Some("Native") if native_linux => Some("Native Linux".to_string()),
        Some("Works") if requires_proton => Some("Works via Proton".to_string()),
        Some(value) => Some(value.to_string()),
        None => None,
    }
}

fn confidence(verdict: Option<&str>, missing_count: usize) -> &'static str {
    match (verdict, missing_count) {
        (Some("Native" | "Works" | "Partial" | "Broken"), 0..=2) => "high",
        (Some(_), _) => "medium",
        _ => "low",
    }
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_exact_match_highest() {
        let exact = score_game(
            GameEntry {
                title: "Elden Ring".to_string(),
                url: "https://example.test/elden".to_string(),
            },
            "elden ring",
        )
        .unwrap();
        let partial = score_game(
            GameEntry {
                title: "ELDEN RING NIGHTREIGN".to_string(),
                url: "https://example.test/nightreign".to_string(),
            },
            "elden ring",
        )
        .unwrap();
        assert!(exact.score > partial.score);
    }

    #[test]
    fn parses_json_ld_games() {
        let html = r#"<script type="application/ld+json">{"@type":"ItemList","numberOfItems":1,"itemListElement":[{"name":"Cyberpunk 2077","url":"https://caniplayonlinux.com/games/cyberpunk-2077/"}]}</script>"#;
        let games = extract_games_from_list_page(html);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].title, "Cyberpunk 2077");
    }

    #[test]
    fn parses_html_card_title() {
        let html = r#"<a href="/games/cyberpunk-2077/"><div><h3>Cyberpunk 2077</h3><p>Works</p></div></a>"#;
        let games = extract_games_from_list_page(html);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].title, "Cyberpunk 2077");
    }

    #[test]
    fn rejects_navigation_links() {
        assert!(!is_game_detail_path("/games/works/"));
        assert!(!is_game_detail_path("/games/2/"));
        assert!(is_game_detail_path("/games/elden-ring/"));
    }

    #[test]
    fn extracts_label_value() {
        let text = "Recommended Proton\n9.0-3\nLast verified\nMay 8, 2026";
        assert_eq!(
            value_after_label(text, "Recommended Proton"),
            Some("9.0-3".to_string())
        );
        assert_eq!(
            value_after_label("Recommended Proton│9.0-3", "Recommended Proton"),
            Some("9.0-3".to_string())
        );
    }

    #[test]
    fn sections_ignore_navigation_links() {
        let text = "3. [Known issues][1]\n4. [Fixes & workarounds][2]\n\n## Known issues\nReal issue\n\n## Fixes & workarounds\nReal fix\n\n**Verdict: Works**\nUnrelated prose";

        assert_eq!(
            section_excerpt(text, "Known issues", "Fixes", 100),
            Some("Real issue".to_string())
        );
        assert_eq!(
            section_excerpt(text, "Fixes", "Verdict", 100),
            Some("Real fix".to_string())
        );
    }
}
