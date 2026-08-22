use super::{html_conversion, http_response, ToolRegistry, ToolSpec};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::time::Duration;

const MAX_PAGE_BYTES: usize = 512 * 1024;
const MAX_OUTPUT_CHARS: usize = 20_000;

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new(
        "query_moegirl",
        "Search or read Moegirlpedia pages. Supports zh/cn, uk, and ja sites.",
        json!({"type":"object","properties":{"query":{"type":"string"},"title":{"type":"string"},"mode":{"type":"string","enum":["auto","search","page"]},"site":{"type":"string","enum":["zh","cn","uk","ja",""]}},"additionalProperties":false}),
        |args| async move { query(args).await },
    ));
}

async fn query(args: Value) -> Result<String> {
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("auto");
    let site = site(args.get("site").and_then(Value::as_str).unwrap_or("zh"));
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if mode == "search" || (mode == "auto" && title.is_empty()) {
        let q = if query.is_empty() { title } else { query };
        if q.is_empty() {
            bail!("query or title is required")
        }
        let url = format!(
            "{}/api.php?action=opensearch&search={}&limit=5&namespace=0&format=json",
            site.api,
            urlencoding::encode(q)
        );
        let response = client().get(url).send().await?.error_for_status()?;
        let data: Value = http_response::read_json(response, MAX_PAGE_BYTES).await?;
        if mode == "search" {
            return Ok(serde_json::to_string_pretty(&data)?);
        }
        if let Some(first) = data
            .get(1)
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str)
        {
            return fetch_page(site, first).await;
        }
    }
    fetch_page(site, if title.is_empty() { query } else { title }).await
}

async fn fetch_page(site: Site, title: &str) -> Result<String> {
    if title.trim().is_empty() {
        bail!("query or title is required")
    }
    let url = format!(
        "{}/rest.php/v1/page/{}/html",
        site.base,
        urlencoding::encode(title)
    );
    // REST 这一路任何一种失败都要能落到 api.php:两个站点的可用性正好
    // 互补——zh 的 rest.php 好用而 api.php 被禁,uk 反过来 rest.php 返回
    // 501。此处早前用 `error_for_status()?` 外抛,HTTP 错误码压根进不了
    // fallback,uk 站因此整个不可用。
    let html = match client()
        .get(&url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
    {
        Ok(response) => match limited_text(response).await {
            Ok(text) => text,
            Err(_) => fetch_page_via_api(site, title).await?,
        },
        Err(_) => fetch_page_via_api(site, title).await?,
    };
    let markdown = html_conversion::to_markdown(html).await?;
    Ok(format!(
        "Source: {}{}\n\n{}",
        site.page,
        urlencoding::encode(title),
        clip(&markdown)
    ))
}

async fn fetch_page_via_api(site: Site, title: &str) -> Result<String> {
    let url = format!(
        "{}/api.php?action=parse&page={}&prop=text&format=json",
        site.api,
        urlencoding::encode(title)
    );
    let response = client().get(url).send().await?.error_for_status()?;
    let data: Value =
        http_response::read_json(response, http_response::MAX_HTML_RESPONSE_BYTES).await?;
    let html = data
        .pointer("/parse/text/*")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if html.trim().is_empty() {
        bail!("Moegirlpedia page not found or returned empty content")
    }
    Ok(html)
}

/// 超过 MAX_PAGE_BYTES 就截断,不整页失败。
///
/// 正文随后还要 clip 到 MAX_OUTPUT_CHARS,字节上限只是内存护栏。此前
/// 超限即报错,导致「初音未来」这种 609 KB 的长条目落进 fallback,再被
/// 禁用的 api.php 顶回来,最后报「page not found」——页面明明存在,抓取
/// 也是 HTTP 200。序章在最前面,取前缀正是想要的部分。
async fn limited_text(response: reqwest::Response) -> Result<String> {
    http_response::read_text_prefix(response, MAX_PAGE_BYTES).await
}

fn clip(value: &str) -> String {
    if value.chars().count() <= MAX_OUTPUT_CHARS {
        value.to_string()
    } else {
        format!(
            "{}\n...[truncated to {MAX_OUTPUT_CHARS} chars]",
            value.chars().take(MAX_OUTPUT_CHARS).collect::<String>()
        )
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("miyu/0.1")
        .build()
        .expect("valid reqwest client")
}

#[derive(Clone, Copy)]
struct Site {
    api: &'static str,
    base: &'static str,
    page: &'static str,
}

fn site(value: &str) -> Site {
    match value {
        "uk" => Site {
            api: "https://moegirl.uk",
            base: "https://moegirl.uk",
            page: "https://moegirl.uk/",
        },
        "ja" => Site {
            api: "https://ja.moegirl.org",
            base: "https://ja.moegirl.org",
            page: "https://ja.moegirl.org/",
        },
        _ => Site {
            api: "https://zh.moegirl.org.cn",
            base: "https://zh.moegirl.org.cn",
            page: "https://zh.moegirl.org.cn/",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 联网用例，默认 ignore，改动抓取路径时手动跑：
    /// `cargo test --release moegirl -- --ignored --nocapture`
    ///
    /// 覆盖两个各自失败过的站点：
    /// - zh：rest.php 好用但正文 609 KB 超字节上限，api.php 又被禁用
    /// - uk：rest.php 返回 501，必须落到 api.php
    #[tokio::test]
    #[ignore = "requires network"]
    async fn fetches_a_long_page_from_either_site() {
        for site_key in ["zh", "uk"] {
            let text = fetch_page(site(site_key), "初音未来")
                .await
                .unwrap_or_else(|err| panic!("{site_key}: {err}"));
            assert!(
                text.contains("初音") && text.len() > 2_000,
                "{site_key}: 正文太短（{} 字符）",
                text.len()
            );
            println!("  {site_key}: {} 字符", text.chars().count());
        }
    }
}
