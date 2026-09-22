//! 抓 HTML 的搜索供应商。
//!
//! DuckDuckGo / Yahoo / 360 / 搜狗。这些没有 API，只能解析页面，所以随时会
//! 坏，也随时会弹验证码（`looks_like_ddg_challenge`）——被挡了就记状态，别继续
//! 撞（`set_ddg_blocked`）。
//!
//! 结果链接常是跳转包装（`unwrap_ddg_url`、`resolve_sogou_url`），要还原成真实
//! 地址再交出去。

use crate::tools::web::*;
use futures_util::StreamExt;

/// 跳转解析的并发度。跟仓库其它抓取路径（`web_images`、`api_quota`、
/// `caniplayonlinux_query`）一样取 4。
const RESOLVE_CONCURRENCY: usize = 4;

pub(in crate::tools::web) fn is_ddg_blocked() -> bool {
    DDG_BLOCKED_UNTIL
        .lock()
        .ok()
        .and_then(|guard| guard.filter(|&t| t > Instant::now()))
        .is_some()
}

pub(in crate::tools::web) fn set_ddg_blocked(duration: Duration) {
    if let Ok(mut guard) = DDG_BLOCKED_UNTIL.lock() {
        *guard = Some(Instant::now() + duration);
    }
}

pub(in crate::tools::web) fn is_sogou_blocked() -> bool {
    SOGOU_BLOCKED_UNTIL
        .lock()
        .ok()
        .and_then(|guard| guard.filter(|&t| t > Instant::now()))
        .is_some()
}

pub(in crate::tools::web) fn set_sogou_blocked(duration: Duration) {
    if let Ok(mut guard) = SOGOU_BLOCKED_UNTIL.lock() {
        *guard = Some(Instant::now() + duration);
    }
}

pub(in crate::tools::web) fn looks_like_ddg_challenge(status: u16, html: &str) -> bool {
    if !matches!(status, 200 | 202 | 403 | 429) {
        return false;
    }
    html.contains("bots use DuckDuckGo too")
        || html.contains("complete the following challenge")
        || html.contains("anomaly.js")
        || html.contains("Select all squares")
}

pub(in crate::tools::web) fn unwrap_ddg_url(url: &str) -> String {
    let url = html_unescape(url.trim());
    if let Some(q_pos) = url.find('?') {
        let query = &url[q_pos + 1..];
        for pair in query.split('&') {
            if let Some(val) = pair.strip_prefix("uddg=") {
                if let Ok(decoded) = url_decode(val) {
                    return decoded.to_string();
                }
                return val.to_string();
            }
        }
    }
    if url.starts_with("//") {
        return format!("https:{url}");
    }
    url
}

pub(in crate::tools::web) fn unwrap_yahoo_url(url: &str) -> String {
    let url = html_unescape(url.trim());
    if url.contains("r.search.yahoo.com") {
        if let Some(pos) = url.find("/RU=") {
            let rest = &url[pos + 4..];
            let end = rest.find('/').unwrap_or(rest.len());
            if let Ok(decoded) = url_decode(&rest[..end]) {
                return decoded.to_string();
            }
            return rest[..end].to_string();
        }
    }
    url
}

pub(in crate::tools::web) fn extract_snippet_after(text: &str, marker: &str) -> Option<String> {
    let pos = text.find(marker)?;
    let rest = &text[pos..];
    let open_end = rest.find('>')?;
    let close = rest[open_end + 1..].find("</")?;
    Some(clean_html_text(&rest[open_end + 1..open_end + 1 + close]))
}

pub(in crate::tools::web) async fn search_duckduckgo(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Result<String> {
    if is_ddg_blocked() {
        let fallback = search_fallback_html(client, query, max_results).await;
        if !fallback.is_empty() {
            return Ok(format_crawler_results(
                query,
                "DuckDuckGo (via fallback)",
                fallback,
            ));
        }
        bail!("DuckDuckGo is blocked by captcha and fallback engines returned no results");
    }

    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );
    let response = client
        .get(&url)
        .header("User-Agent", CRAWLER_USER_AGENT)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await;

    let html = match response {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            if looks_like_ddg_challenge(status, &text) {
                set_ddg_blocked(Duration::from_secs(60));
                let fallback = search_fallback_html(client, query, max_results).await;
                if !fallback.is_empty() {
                    return Ok(format_crawler_results(
                        query,
                        "DuckDuckGo (via fallback - DDG captcha)",
                        fallback,
                    ));
                }
                bail!(
                    "DuckDuckGo returned a captcha page and fallback engines returned no results"
                );
            }
            if status != 200 {
                let fallback = search_fallback_html(client, query, max_results).await;
                if !fallback.is_empty() {
                    return Ok(format_crawler_results(
                        query,
                        "DuckDuckGo (via fallback - DDG HTTP error)",
                        fallback,
                    ));
                }
                bail!("DuckDuckGo HTTP {status} and fallback returned no results");
            }
            text
        }
        Err(_) => {
            let fallback = search_fallback_html(client, query, max_results).await;
            if !fallback.is_empty() {
                return Ok(format_crawler_results(
                    query,
                    "DuckDuckGo (via fallback - DDG request failed)",
                    fallback,
                ));
            }
            bail!("DuckDuckGo request failed and fallback returned no results");
        }
    };

    let results = parse_duckduckgo_html(&html, max_results);
    if !results.is_empty() {
        return Ok(format_crawler_results(query, "DuckDuckGo HTML", results));
    }

    let fallback = search_fallback_html(client, query, max_results).await;
    if !fallback.is_empty() {
        return Ok(format_crawler_results(
            query,
            "DuckDuckGo (via fallback - DDG no results)",
            fallback,
        ));
    }
    bail!("DuckDuckGo returned no parseable results and fallback returned no results");
}

pub(in crate::tools::web) fn parse_duckduckgo_html(
    html: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut rest = html;
    while let Some(link_pos) = rest.find("result__a") {
        rest = &rest[link_pos..];
        let Some(href_pos) = rest.find("href=\"") else {
            break;
        };
        let href_start = href_pos + "href=\"".len();
        let Some(href_end) = rest[href_start..].find('"') else {
            break;
        };
        let raw_url = unwrap_ddg_url(&rest[href_start..href_start + href_end]);
        let Some(tag_end) = rest[href_start + href_end..].find('>') else {
            break;
        };
        let title_start = href_start + href_end + tag_end + 1;
        let Some(title_end) = rest[title_start..].find("</a>") else {
            break;
        };
        let title = clean_html_text(&rest[title_start..title_start + title_end]);
        let snippet =
            if let Some(snippet_pos) = rest[title_start + title_end..].find("result__snippet") {
                let snippet_rest = &rest[title_start + title_end + snippet_pos..];
                if let Some(open_end) = snippet_rest.find('>') {
                    if let Some(close) = snippet_rest[open_end + 1..].find("</") {
                        clean_html_text(&snippet_rest[open_end + 1..open_end + 1 + close])
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
        if !title.is_empty() && !raw_url.is_empty() && is_result_url_allowed(&raw_url) {
            let key = dedupe_key(&raw_url);
            if seen.insert(key) {
                results.push(CrawlerResult {
                    title,
                    url: raw_url,
                    snippet,
                    source: "DuckDuckGo".to_string(),
                });
            }
        }
        if results.len() >= max_results {
            break;
        }
        rest = &rest[title_start + title_end..];
    }
    results
}

pub(in crate::tools::web) async fn search_yahoo_html(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let url = format!(
        "https://search.yahoo.com/search?p={}",
        urlencoding::encode(query)
    );
    let html = match client
        .get(&url)
        .header("User-Agent", CRAWLER_USER_AGENT)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status().as_u16() != 200 {
                return Vec::new();
            }
            resp.text().await.unwrap_or_default()
        }
        Err(_) => return Vec::new(),
    };
    parse_yahoo_html(&html, max_results)
}

pub(in crate::tools::web) fn parse_yahoo_html(
    html: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut rest = html;
    while let Some(pos) = rest.find("class=\"dd algo") {
        rest = &rest[pos..];
        let anchor_start = match rest.find("href=\"") {
            Some(p) => p + "href=\"".len(),
            None => {
                rest = &rest[10..];
                continue;
            }
        };
        let Some(href_end) = rest[anchor_start..].find('"') else {
            break;
        };
        let raw_url = unwrap_yahoo_url(&rest[anchor_start..anchor_start + href_end]);
        let Some(tag_end) = rest[anchor_start + href_end..].find('>') else {
            rest = &rest[anchor_start + href_end..];
            continue;
        };
        let title_start = anchor_start + href_end + tag_end + 1;
        let Some(title_end) = rest[title_start..].find("</a>") else {
            break;
        };
        let title = clean_html_text(&rest[title_start..title_start + title_end]);
        let snippet = extract_snippet_after(&rest[title_start + title_end..], "compText")
            .or_else(|| extract_snippet_after(&rest[title_start + title_end..], "<p"))
            .unwrap_or_default();
        if !title.is_empty() && !raw_url.is_empty() && is_result_url_allowed(&raw_url) {
            let key = dedupe_key(&raw_url);
            if seen.insert(key) {
                results.push(CrawlerResult {
                    title,
                    url: raw_url,
                    snippet,
                    source: "Yahoo".to_string(),
                });
            }
        }
        if results.len() >= max_results {
            break;
        }
        rest = &rest[title_start + title_end..];
    }
    results
}

pub(in crate::tools::web) async fn search_so_html(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let url = format!("https://www.so.com/s?q={}", urlencoding::encode(query));
    let html = match client
        .get(&url)
        .header("User-Agent", CRAWLER_USER_AGENT)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status().as_u16() != 200 {
                return Vec::new();
            }
            resp.text().await.unwrap_or_default()
        }
        Err(_) => return Vec::new(),
    };
    parse_so_html(client, &html, max_results).await
}

pub(in crate::tools::web) async fn parse_so_html(
    client: &reqwest::Client,
    html: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let mut candidates: Vec<(String, String, String)> = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("class=\"result") {
        rest = &rest[pos..];
        let h3_pos = match rest.find("<h3") {
            Some(p) => p,
            None => {
                rest = &rest[10..];
                continue;
            }
        };
        let h3_rest = &rest[h3_pos..];
        let href_start = match h3_rest.find("href=\"") {
            Some(p) => p + "href=\"".len(),
            None => {
                rest = &rest[h3_pos + 3..];
                continue;
            }
        };
        let Some(href_end) = h3_rest[href_start..].find('"') else {
            break;
        };
        let href = html_unescape(&h3_rest[href_start..href_start + href_end]);
        let Some(tag_end) = h3_rest[href_start + href_end..].find('>') else {
            rest = &rest[h3_pos + 3..];
            continue;
        };
        let title_start = href_start + href_end + tag_end + 1;
        let Some(title_end) = h3_rest[title_start..].find("</a>") else {
            break;
        };
        let title = clean_html_text(&h3_rest[title_start..title_start + title_end]);
        let snippet = extract_snippet_after(&h3_rest[title_start + title_end..], "res-desc")
            .or_else(|| extract_snippet_after(&h3_rest[title_start + title_end..], "fz-mid"))
            .or_else(|| extract_snippet_after(&h3_rest[title_start + title_end..], "<p"))
            .unwrap_or_default();
        if !title.is_empty() && !href.is_empty() {
            candidates.push((title, href, snippet));
        }
        if candidates.len() >= max_results * 2 {
            break;
        }
        rest = &h3_rest[title_start + title_end..];
    }

    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // 保序的有界并发。`buffered`（不是 `buffer_unordered`）按**输入顺序**交付，
    // 所以下面的去重和「够了就停」看到的次序跟串行版一模一样，结果不会变。
    // 代价是最多多解析 3 个候选（在飞的那些），换掉
    // max_results 次串行往返——这条路上每个候选都是一次跳转解析的 HTTP 往返。
    let mut resolving = futures_util::stream::iter(candidates.into_iter().map(
        |(title, href, snippet)| async move {
            let resolved = resolve_so_url(client, &href).await;
            (title, resolved, snippet)
        },
    ))
    .buffered(RESOLVE_CONCURRENCY);
    while let Some((title, resolved, snippet)) = resolving.next().await {
        if results.len() >= max_results {
            break;
        }
        if !resolved.is_empty() && is_result_url_allowed(&resolved) {
            let key = dedupe_key(&resolved);
            if seen.insert(key) {
                results.push(CrawlerResult {
                    title,
                    url: resolved,
                    snippet,
                    source: "360".to_string(),
                });
            }
        }
    }
    results
}

pub(in crate::tools::web) async fn resolve_so_url(client: &reqwest::Client, href: &str) -> String {
    let href = html_unescape(href.trim());
    if href.is_empty() {
        return String::new();
    }
    let absolute = if href.starts_with("http://") || href.starts_with("https://") {
        href.clone()
    } else {
        format!("https://www.so.com{}", href)
    };
    if !(absolute.contains("so.com") && absolute.contains("/link")) {
        return absolute;
    }
    match client.get(&absolute).send().await {
        Ok(resp) => {
            let final_url = resp.url().to_string();
            if final_url != absolute
                && (final_url.starts_with("http://") || final_url.starts_with("https://"))
            {
                return final_url;
            }
            let text = resp.text().await.unwrap_or_default();
            if let Some(pos) = text.find("window.location") {
                let rest = &text[pos..];
                if let Some(q1) = rest.find('"') {
                    if let Some(q2) = rest[q1 + 1..].find('"') {
                        return html_unescape(&rest[q1 + 1..q1 + 1 + q2]);
                    }
                }
            }
            if let Some(pos) = text.find("URL=") {
                let rest = &text[pos + 4..];
                let end = rest
                    .find('"')
                    .or_else(|| rest.find('>'))
                    .unwrap_or(rest.len());
                let url_str = rest[..end].trim_matches('\'');
                return html_unescape(url_str);
            }
            absolute
        }
        Err(_) => absolute,
    }
}

pub(in crate::tools::web) async fn search_sogou_html(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    if is_sogou_blocked() {
        return Vec::new();
    }
    let url = format!(
        "https://www.sogou.com/web?query={}&ie=utf8",
        urlencoding::encode(query)
    );
    let html = match client
        .get(&url)
        .header("User-Agent", CRAWLER_USER_AGENT)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
    {
        Ok(resp) => {
            let final_url = resp.url().to_string();
            let text = resp.text().await.unwrap_or_default();
            if final_url.contains("antispider")
                || text.contains("SourceVerifyCode")
                || text.contains("\u{6b64}\u{9a8c}\u{8bc1}\u{7801}\u{7528}\u{4e8e}\u{786e}\u{8ba4}")
            {
                set_sogou_blocked(Duration::from_secs(300));
                return Vec::new();
            }
            text
        }
        Err(_) => return Vec::new(),
    };
    parse_sogou_html(client, &html, max_results).await
}

pub(in crate::tools::web) async fn parse_sogou_html(
    client: &reqwest::Client,
    html: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let mut candidates: Vec<(String, String, String)> = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("class=\"vrwrap") {
        rest = &rest[pos..];
        let h3_pos = match rest.find("<h3") {
            Some(p) => p,
            None => {
                rest = &rest[10..];
                continue;
            }
        };
        let h3_rest = &rest[h3_pos..];
        let href_start = match h3_rest.find("href=\"") {
            Some(p) => p + "href=\"".len(),
            None => {
                rest = &rest[h3_pos + 3..];
                continue;
            }
        };
        let Some(href_end) = h3_rest[href_start..].find('"') else {
            break;
        };
        let href = html_unescape(&h3_rest[href_start..href_start + href_end]);
        let Some(tag_end) = h3_rest[href_start + href_end..].find('>') else {
            rest = &rest[h3_pos + 3..];
            continue;
        };
        let title_start = href_start + href_end + tag_end + 1;
        let Some(title_end) = h3_rest[title_start..].find("</a>") else {
            break;
        };
        let title = clean_html_text(&h3_rest[title_start..title_start + title_end]);
        let snippet = extract_snippet_after(&h3_rest[title_start + title_end..], "fz-mid")
            .or_else(|| extract_snippet_after(&h3_rest[title_start + title_end..], "str_info"))
            .or_else(|| extract_snippet_after(&h3_rest[title_start + title_end..], "<p"))
            .unwrap_or_default();
        if !title.is_empty() && !href.is_empty() {
            candidates.push((title, href, snippet));
        }
        if candidates.len() >= max_results * 2 {
            break;
        }
        rest = &h3_rest[title_start + title_end..];
    }

    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // 保序的有界并发。`buffered`（不是 `buffer_unordered`）按**输入顺序**交付，
    // 所以下面的去重和「够了就停」看到的次序跟串行版一模一样，结果不会变。
    // 代价是最多多解析 3 个候选（在飞的那些），换掉
    // max_results 次串行往返——这条路上每个候选都是一次跳转解析的 HTTP 往返。
    let mut resolving = futures_util::stream::iter(candidates.into_iter().map(
        |(title, href, snippet)| async move {
            let resolved = resolve_sogou_url(client, &href).await;
            (title, resolved, snippet)
        },
    ))
    .buffered(RESOLVE_CONCURRENCY);
    while let Some((title, resolved, snippet)) = resolving.next().await {
        if results.len() >= max_results {
            break;
        }
        if !resolved.is_empty() && is_result_url_allowed(&resolved) {
            let key = dedupe_key(&resolved);
            if seen.insert(key) {
                results.push(CrawlerResult {
                    title,
                    url: resolved,
                    snippet,
                    source: "Sogou".to_string(),
                });
            }
        }
    }
    results
}

pub(in crate::tools::web) async fn resolve_sogou_url(
    client: &reqwest::Client,
    href: &str,
) -> String {
    let href = html_unescape(href.trim());
    if href.is_empty() {
        return String::new();
    }
    let absolute = if href.starts_with("http://") || href.starts_with("https://") {
        href.clone()
    } else {
        format!("https://www.sogou.com{}", href)
    };
    if !(absolute.contains("sogou.com") && absolute.contains("/link")) {
        return absolute;
    }
    match client.get(&absolute).send().await {
        Ok(resp) => {
            let final_url = resp.url().to_string();
            if final_url != absolute
                && (final_url.starts_with("http://") || final_url.starts_with("https://"))
            {
                return final_url;
            }
            let text = resp.text().await.unwrap_or_default();
            if let Some(pos) = text.find("window.location") {
                let rest = &text[pos..];
                if let Some(q1) = rest.find('"') {
                    if let Some(q2) = rest[q1 + 1..].find('"') {
                        return html_unescape(&rest[q1 + 1..q1 + 1 + q2]);
                    }
                }
            }
            if let Some(pos) = text.find("URL=") {
                let rest = &text[pos + 4..];
                let end = rest
                    .find('"')
                    .or_else(|| rest.find('>'))
                    .unwrap_or(rest.len());
                let url_str = rest[..end].trim_matches('\'');
                return html_unescape(url_str);
            }
            absolute
        }
        Err(_) => absolute,
    }
}

pub(in crate::tools::web) async fn search_fallback_html(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let yahoo_results = search_yahoo_html(client, query, max_results).await;
    if yahoo_results.len() >= max_results.min(5) {
        return yahoo_results;
    }

    let mut combined: Vec<CrawlerResult> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in yahoo_results {
        let key = dedupe_key(&r.url);
        if seen.insert(key) {
            combined.push(r);
        }
    }

    let so_results = search_so_html(client, query, max_results).await;
    for r in so_results {
        if combined.len() >= max_results {
            break;
        }
        let key = dedupe_key(&r.url);
        if seen.insert(key) {
            combined.push(r);
        }
    }

    if combined.len() < max_results {
        let sogou_results = search_sogou_html(client, query, max_results).await;
        for r in sogou_results {
            if combined.len() >= max_results {
                break;
            }
            let key = dedupe_key(&r.url);
            if seen.insert(key) {
                combined.push(r);
            }
        }
    }

    combined
}

pub(in crate::tools::web) fn clean_html_text(value: &str) -> String {
    html_unescape(&html_conversion::to_text_lossy(value, 120))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(in crate::tools::web) fn html_unescape(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// 去掉片段里成串的 markdown 链接。网页正文抽取常把导航栏/页脚整条带进
/// snippet(实测某条结果整段是"[网易首页](…) [应用](…) [网易公开课](…)…"),
/// 那是零信息量的样板。
///
/// 门槛定在连续 3 条:正文里"参见 [文档](u), [示例](u)"这种两条并排很常见,
/// 按 2 条删会误伤真内容;导航条实测都是 5 条以上。
pub(in crate::tools::web) fn strip_nav_link_runs(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    // 先扫出所有 [text](url) 的字符区间。
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] != '[' {
            index += 1;
            continue;
        }
        let Some(close) = (index + 1..chars.len()).find(|i| chars[*i] == ']') else {
            break;
        };
        if chars.get(close + 1) != Some(&'(') {
            index = close + 1;
            continue;
        }
        let Some(end) = (close + 2..chars.len()).find(|i| chars[*i] == ')') else {
            break;
        };
        spans.push((index, end + 1));
        index = end + 1;
    }
    if spans.len() < 3 {
        return text.to_string();
    }
    // 相邻(中间只隔空白或轻标点)的链接归为一串;串长 >= 2 才删。
    let separator_only = |from: usize, to: usize| {
        chars[from..to]
            .iter()
            .all(|ch| ch.is_whitespace() || matches!(ch, '|' | '·' | '-' | '*' | ',' | '、'))
    };
    let mut drop = vec![false; spans.len()];
    let mut run_start = 0usize;
    for i in 1..=spans.len() {
        let joins = i < spans.len() && separator_only(spans[i - 1].1, spans[i].0);
        if !joins {
            if i - run_start >= 3 {
                for item in drop.iter_mut().take(i).skip(run_start) {
                    *item = true;
                }
            }
            run_start = i;
        }
    }
    if !drop.iter().any(|dropped| *dropped) {
        return text.to_string();
    }
    let mut out = String::new();
    let mut cursor = 0usize;
    for (span, dropped) in spans.iter().zip(&drop) {
        if !dropped {
            continue;
        }
        out.extend(&chars[cursor..span.0]);
        cursor = span.1;
    }
    out.extend(&chars[cursor..]);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod resolve_concurrency_probe {
    use super::*;

    /// 改前的写法：跳转一个一个串行解析。
    async fn resolve_serially(client: &reqwest::Client, hrefs: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        for href in hrefs {
            out.push(resolve_so_url(client, href).await);
        }
        out
    }

    /// 量尺：`cargo test --lib resolve_concurrency_probe -- --ignored --nocapture`
    ///
    /// **会真的去请求 so.com**，所以 `#[ignore]`。
    ///
    /// 360/搜狗的结果链接是跳转包装，每条都要一次 HTTP 往返才能还原成真实地址。
    /// 原来这是个串行 for 循环，条数就是 `max_results`。
    ///
    /// 这里用的是**打不通的 URL**，所以每条都吃满 15 秒超时——量的是**最坏
    /// 情况**（站点吊死时用户要等多久），不是日常延迟。日常每条几百毫秒，
    /// 加速比同样约 4×，但绝对值小得多。
    ///
    /// 顺带钉住顺序：`buffered` 保序，去重和「够了就停」都依赖它。
    #[tokio::test]
    #[ignore]
    async fn resolving_links_concurrently_is_faster() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("Mozilla/5.0")
            .build()
            .unwrap();
        let hrefs: Vec<String> = (0..8)
            .map(|index| format!("https://www.so.com/link?m=probe{index}"))
            .collect();

        let started = std::time::Instant::now();
        let serial = resolve_serially(&client, &hrefs).await;
        let serial_time = started.elapsed();

        let started = std::time::Instant::now();
        let concurrent: Vec<String> =
            futures_util::stream::iter(hrefs.iter().map(|href| resolve_so_url(&client, href)))
                .buffered(RESOLVE_CONCURRENCY)
                .collect()
                .await;
        let concurrent_time = started.elapsed();

        println!(
            "\n  {} 条打不通的跳转（每条吃满 15s 超时，最坏情况）：\n  \
             串行 {:.2}s → 并发{RESOLVE_CONCURRENCY} {:.2}s（快 {:.1}×）",
            hrefs.len(),
            serial_time.as_secs_f64(),
            concurrent_time.as_secs_f64(),
            serial_time.as_secs_f64() / concurrent_time.as_secs_f64().max(0.001),
        );
        // 顺序必须保住——去重和「够了就停」都依赖它
        assert_eq!(serial, concurrent, "并发版打乱了顺序");
    }
}
