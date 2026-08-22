//! caniplayonlinux.com 的兼容性结论快照。
//!
//! 定位游戏走两步：先按 slug 直接拼详情页 URL（实测 145 条真值样本命中 96%，
//! 约 60 ms），404 再退到站点 sitemap 做模糊匹配（一次请求拿全部 2600+ 条目，
//! 约 145 ms）。
//!
//! 这里**不**扫目录。此前的实现因为站点没有搜索接口（`?q=` 被忽略，照样返回
//! 全部 108 页），只能整本目录抓下来本地筛：108 个请求换一个游戏的信息，实测
//! 1.8 秒。sitemap 覆盖 2646 条，比目录页宣称的 2586 还多，且带 `lastmod`。

use super::http_response;
use crate::paths::MiyuPaths;
use anyhow::Result;
use std::time::{Duration, SystemTime};

const BASE_URL: &str = "https://caniplayonlinux.com";
const SITEMAP_URL: &str = "https://caniplayonlinux.com/sitemap-0.xml";
const SITEMAP_CACHE: &str = "caniplayonlinux-sitemap.xml";
const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone)]
pub(super) struct CipolEntry {
    /// meta description。信息密度远高于正文分节抽取——一句话里通常同时给出
    /// ProtonDB 评级、报告数、有无反作弊、推荐 Proton 和验证时间。
    pub summary: Option<String>,
    /// 绝对日期（`YYYY-MM-DD`）。页面同时提供相对时间（"1 month ago"），
    /// 但那个一旦缓存就失真，也没法和 AWACY 的日期比对。
    pub last_verified: Option<String>,
    pub url: String,
}

/// caniplayonlinux 的 slug 规则：撇号和 & 直接删除，不转成连字符。
///
/// `Baldur's Gate 3` → `baldurs-gate-3`（而非 `baldur-s-gate-3`）。实测 145 条
/// 真值样本：朴素实现命中 80%，本实现 96%。
fn slugify(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_dash = false;
    for ch in value.chars() {
        if matches!(ch, '\'' | '\u{2019}' | '&' | '\u{FF06}') {
            continue;
        }
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

async fn fetch_text(url: &str) -> Result<String> {
    let response = http_response::shared_client()
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    http_response::read_text(response, http_response::MAX_HTML_RESPONSE_BYTES).await
}

async fn sitemap(paths: &MiyuPaths) -> Result<String> {
    let cache_path = paths.cache_dir.join(SITEMAP_CACHE);
    let fresh = tokio::fs::metadata(&cache_path)
        .await
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < CACHE_TTL);
    if fresh {
        if let Ok(text) = tokio::fs::read_to_string(&cache_path).await {
            if !text.is_empty() {
                return Ok(text);
            }
        }
    }

    let text = fetch_text(SITEMAP_URL).await?;
    if let Some(parent) = cache_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::write(&cache_path, &text).await;
    Ok(text)
}

/// 从 sitemap 里挑最接近的游戏页 URL。
fn best_sitemap_match(sitemap: &str, slug: &str) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for chunk in sitemap.split("<loc>").skip(1) {
        let Some(url) = chunk.split("</loc>").next() else {
            continue;
        };
        let Some(candidate) = url.trim().strip_prefix(&format!("{BASE_URL}/games/")) else {
            continue;
        };
        let candidate = candidate.trim_end_matches('/');
        // 目录分页（/games/3/）和空 slug 都不是游戏页。
        if candidate.is_empty() || candidate.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let score = similarity(slug, candidate);
        if score >= 75 && best.as_ref().is_none_or(|(best, _)| score > *best) {
            best = Some((score, url.trim().to_string()));
        }
    }
    best.map(|(_, url)| url)
}

/// 粗糙的相似度（0-100）：以较长串为分母的公共字符前缀/包含度。
/// 只用来在 slug 直连失败后兜底，不需要精确。
fn similarity(a: &str, b: &str) -> usize {
    if a == b {
        return 100;
    }
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    if short.is_empty() {
        return 0;
    }
    if long.starts_with(short) || long.contains(short) {
        return 70 + 30 * short.len() / long.len();
    }
    let common = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    100 * common / long.len()
}

fn meta_description(html: &str) -> Option<String> {
    for marker in [
        "<meta name=\"description\" content=\"",
        "<meta property=\"og:description\" content=\"",
    ] {
        if let Some(pos) = html.find(marker) {
            let rest = &html[pos + marker.len()..];
            if let Some(end) = rest.find('"') {
                let value = decode_entities(rest[..end].trim());
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// 标签转行分隔符。行结构必须保住——`value_after_label` 靠换行界定字段末尾，
/// 若把 `\n` 一并压成空格，整个文档会变成一行，所有按标签抽取都会失效。
fn to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut skip_until: Option<&str> = None;
    let lower = html.to_ascii_lowercase();
    let mut index = 0;
    while index < html.len() {
        if let Some(close) = skip_until {
            match lower[index..].find(close) {
                Some(rel) => {
                    index += rel + close.len();
                    skip_until = None;
                }
                None => break,
            }
            continue;
        }
        for (open, close) in [
            ("<script", "</script>"),
            ("<style", "</style>"),
            ("<svg", "</svg>"),
        ] {
            if lower[index..].starts_with(open) {
                skip_until = Some(close);
                break;
            }
        }
        if skip_until.is_some() {
            continue;
        }
        let ch = html[index..].chars().next().unwrap_or('\0');
        match ch {
            '<' => {
                in_tag = true;
                out.push('\n');
            }
            '>' => in_tag = false,
            _ if in_tag => {}
            '\n' | '\r' | '\t' => out.push('\n'),
            _ => out.push(ch),
        }
        index += ch.len_utf8();
    }
    // 折叠空白，但保留换行。
    let mut collapsed = String::with_capacity(out.len());
    let mut last_newline = true;
    let mut last_space = false;
    for ch in out.chars() {
        match ch {
            '\n' => {
                if !last_newline {
                    collapsed.push('\n');
                }
                last_newline = true;
                last_space = false;
            }
            ' ' => {
                if !last_space && !last_newline {
                    collapsed.push(' ');
                }
                last_space = true;
            }
            _ => {
                collapsed.push(ch);
                last_newline = false;
                last_space = false;
            }
        }
    }
    decode_entities(collapsed.trim())
}

/// 取标签后面那一行的值。同一标签可能出现多次（相对时间一次、绝对日期一次），
/// `want_year` 时优先返回带四位年份的那个。
fn value_after_label(text: &str, label: &str, want_year: bool) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let needle = label.to_ascii_lowercase();
    let mut fallback = None;
    let mut cursor = 0;
    while let Some(rel) = lower[cursor..].find(&needle) {
        let start = cursor + rel + needle.len();
        cursor = start;
        let line = text[start..]
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && *line != ":")
            .map(|line| line.trim_start_matches(':').trim().to_string());
        let Some(line) = line.filter(|line| !line.is_empty() && line.len() <= 40) else {
            continue;
        };
        if !want_year {
            return Some(line);
        }
        if has_year(&line) {
            return Some(line);
        }
        fallback.get_or_insert(line);
    }
    fallback
}

fn has_year(value: &str) -> bool {
    value
        .as_bytes()
        .windows(4)
        .any(|w| w.iter().all(u8::is_ascii_digit) && matches!(&w[..2], b"19" | b"20"))
}

/// `May 26, 2026` → `2026-05-26`。认不出就原样返回。
fn to_iso_date(value: &str) -> String {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = value.to_ascii_lowercase();
    let mut parts = lower.split([' ', ',']).filter(|part| !part.is_empty());
    let month = parts.next().and_then(|m| {
        MONTHS
            .iter()
            .position(|candidate| m.starts_with(candidate))
            .map(|index| index + 1)
    });
    let day: Option<u32> = parts.next().and_then(|d| d.parse().ok());
    let year: Option<u32> = parts.next().and_then(|y| y.parse().ok());
    match (year, month, day) {
        (Some(y), Some(m), Some(d)) if (1970..=2999).contains(&y) && d <= 31 => {
            format!("{y:04}-{m:02}-{d:02}")
        }
        _ => value.to_string(),
    }
}

fn decode_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
}

/// `Ok(None)` = 站点没收录这个游戏，与"抓取失败"是两回事：前者渲染成
/// `no result`，后者要把错误原样暴露出来。
pub(super) async fn lookup(paths: &MiyuPaths, name: &str) -> Result<Option<CipolEntry>> {
    let slug = slugify(name);
    // 纯非拉丁名（如中文原名）slugify 后为空，拼出的 `/games//` 会命中目录首页
    // 并返回 200 —— 静默产出一份通用简介。必须挡在这里。
    if slug.is_empty() {
        return Ok(None);
    }

    let direct = format!("{BASE_URL}/games/{slug}/");
    let (url, html) = match fetch_text(&direct).await {
        Ok(html) => (direct, html),
        Err(_) => {
            let sitemap = sitemap(paths).await?;
            let Some(url) = best_sitemap_match(&sitemap, &slug) else {
                return Ok(None);
            };
            let html = fetch_text(&url).await?;
            (url, html)
        }
    };

    let text = to_text(&html);
    let last_verified = value_after_label(&text, "Last verified", true).map(|v| to_iso_date(&v));
    Ok(Some(CipolEntry {
        summary: meta_description(&html),
        last_verified,
        url,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_drops_apostrophes_instead_of_hyphenating() {
        assert_eq!(slugify("Baldur's Gate 3"), "baldurs-gate-3");
        assert_eq!(slugify("Black Myth: Wukong"), "black-myth-wukong");
        assert_eq!(
            slugify("Alan Wake's American Nightmare"),
            "alan-wakes-american-nightmare"
        );
        assert_eq!(slugify("Apex Legends™"), "apex-legends");
    }

    #[test]
    fn slugify_returns_empty_for_non_latin_names() {
        // 空 slug 必须被调用方拦住，否则会命中目录首页。
        assert_eq!(slugify("黑神话悟空"), "");
    }

    #[test]
    fn sitemap_match_skips_pagination_pages() {
        let sitemap = "\
<loc>https://caniplayonlinux.com/games/3/</loc>\
<loc>https://caniplayonlinux.com/games/elden-ring/</loc>";
        assert_eq!(
            best_sitemap_match(sitemap, "elden-ring").as_deref(),
            Some("https://caniplayonlinux.com/games/elden-ring/")
        );
        assert_eq!(best_sitemap_match(sitemap, "totally-unrelated-xyz"), None);
    }

    #[test]
    fn meta_description_is_decoded() {
        let html = r#"<meta name="description" content="Works &amp; runs fine">"#;
        assert_eq!(meta_description(html).as_deref(), Some("Works & runs fine"));
    }

    #[test]
    fn text_conversion_keeps_line_structure() {
        // 行结构塌掉的话 value_after_label 会全线失效。
        let text = to_text("<div>Last verified</div><span>May 26, 2026</span>");
        assert!(text.contains('\n'), "expected newlines, got {text:?}");
        assert_eq!(
            value_after_label(&text, "Last verified", true).as_deref(),
            Some("May 26, 2026")
        );
    }

    #[test]
    fn label_lookup_prefers_absolute_date_over_relative() {
        let text = "Last verified\n1 month ago\nLast verified\nMay 26, 2026";
        assert_eq!(
            value_after_label(text, "Last verified", true).as_deref(),
            Some("May 26, 2026")
        );
    }

    #[test]
    fn iso_date_conversion() {
        assert_eq!(to_iso_date("May 26, 2026"), "2026-05-26");
        assert_eq!(to_iso_date("Oct 3, 2024"), "2024-10-03");
        assert_eq!(to_iso_date("1 month ago"), "1 month ago");
    }
}
