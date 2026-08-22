use super::{html_conversion, http_response, ToolRegistry, ToolSpec};
use crate::paths::MiyuPaths;
use anyhow::{bail, Result};
use serde_json::{json, Value};

const ARCH_STATUS_BASE_URL: &str = "https://status.archlinux.org";
const ARCH_STATUS_PAGE_ID: &str = "vmM5ruWEAB";
const ARCH_NEWS_FEED_URL: &str = "https://archlinux.org/feeds/news/";
const ARCH_NEWS_CACHE_FILE: &str = "arch_news_last_seen.json";

pub fn register(registry: &mut ToolRegistry, paths: &MiyuPaths) {
    // 三件 AUR 查询工具合并成 `aur`(08-17):search/info/status 都走官方 RPC,
    // 拆开只是让 tools 数组多背两份外壳。
    registry.register(ToolSpec::new(
        "aur",
        "AUR 官方 RPC 查询。action=search 搜包，action=info 取包详情，action=status 查 AUR/Arch 服务状态（含故障、降级和停机信息）。",
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["search", "info", "status"], "description": "search 搜包，info 详情，status 服务状态。" },
                "query": { "type": "string", "description": "action=search 必填：搜索词。" },
                "package_name": { "type": "string", "description": "action=info 必填：包名。" },
                "limit": { "type": "integer", "description": "仅 search：最多结果数。" },
                "search_by": { "type": "string", "description": "仅 search：搜索字段。" }
            },
            "required": ["action"],
            "additionalProperties": false
        }),
        |args| async move {
            match args.get("action").and_then(Value::as_str).unwrap_or_default() {
                "search" => aur_search(args).await,
                "info" => aur_info(args).await,
                "status" => arch_status().await,
                other => anyhow::bail!("unknown action: {other}; expected search, info or status"),
            }
        },
    ));
    registry.register(ToolSpec::new("archlinux_official_package_query", "查询 Arch Linux 官方软件包数据库，支持搜索和精确包详情。", json!({"type":"object","properties":{"package_name":{"type":"string","description":"包名。"},"repo":{"type":"string","description":"详情模式的仓库，例如 core 或 extra。"},"arch":{"type":"string","description":"详情模式架构，默认 x86_64。"},"mode":{"type":"string","enum":["auto","search","detail"],"description":"auto 在提供 repo 时查详情，否则搜索。"}},"required":["package_name"],"additionalProperties":false}), |args| async move { official_package_query(args).await }));
    registry.register(ToolSpec::new("archwiki_query", "Search or read ArchWiki pages.", json!({"type":"object","properties":{"query":{"type":"string"},"title":{"type":"string"},"mode":{"type":"string","enum":["auto","search","page"]}},"additionalProperties":false}), |args| async move { archwiki(args).await }));

    let state_dir = paths.state_dir.clone();
    registry.register(ToolSpec::new(
        "archlinux_news",
        "获取 Arch Linux 最新新闻。每次查询后自动标记已读，后续调用会标注新增文章。",
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "返回文章数量上限（1-20），默认 10。"
                }
            },
            "additionalProperties": false
        }),
        move |args| {
            let state_dir = state_dir.clone();
            async move { archlinux_news(args, &state_dir).await }
        },
    ));
}

async fn official_package_query(args: Value) -> Result<String> {
    let package = required(&args, "package_name")?;
    let repo = args
        .get("repo")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let arch = args
        .get("arch")
        .and_then(Value::as_str)
        .unwrap_or("x86_64")
        .trim();
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("auto");
    let mode = if mode == "auto" && !repo.is_empty() {
        "detail"
    } else if mode == "auto" {
        "search"
    } else {
        mode
    };
    let url = match mode {
        "detail" => {
            if repo.is_empty() {
                bail!("repo is required for detail mode")
            }
            format!(
                "https://archlinux.org/packages/{}/{}/{}/json/",
                urlencoding::encode(repo),
                urlencoding::encode(arch),
                urlencoding::encode(&package)
            )
        }
        "search" => format!(
            "https://archlinux.org/packages/search/json/?name={}",
            urlencoding::encode(&package)
        ),
        _ => bail!("mode must be auto, search, or detail"),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("miyu-archlinux-official-package-query/0.1")
        .build()?;
    let resp = client.get(&url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        bail!(
            "Arch official package API returned HTTP {} for {}",
            status,
            url
        )
    }
    let data: Value = resp.json().await?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "mode": mode,
        "package_name": package,
        "repo": if repo.is_empty() { Value::Null } else { json!(repo) },
        "arch": arch,
        "url": url,
        "data": data,
    }))?)
}

async fn aur_search(args: Value) -> Result<String> {
    let query = required(&args, "query")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .min(50) as usize;
    let by = args
        .get("search_by")
        .and_then(Value::as_str)
        .unwrap_or("name-desc");
    let url = format!(
        "https://aur.archlinux.org/rpc/?v=5&type=search&by={}&arg={}",
        urlencoding::encode(by),
        urlencoding::encode(&query)
    );
    let data: Value = reqwest::get(url).await?.error_for_status()?.json().await?;
    let results = data
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(limit)
        .map(|item| normalize_search_item(&item))
        .collect::<Vec<_>>();
    Ok(serde_json::to_string_pretty(
        &json!({"success": true, "query": query, "results": results}),
    )?)
}

async fn aur_info(args: Value) -> Result<String> {
    let names_raw = required(&args, "package_name")?;
    let names: Vec<String> = names_raw
        .split([',', ' '])
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .take(5)
        .collect();
    if names.is_empty() {
        bail!("package_name is required");
    }
    let mut url = "https://aur.archlinux.org/rpc/?v=5&type=info".to_string();
    for name in &names {
        url.push_str("&arg[]=");
        url.push_str(&urlencoding::encode(name));
    }
    let data: Value = reqwest::get(url).await?.error_for_status()?.json().await?;
    let raw_results = data
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let found_names: Vec<String> = raw_results
        .iter()
        .filter_map(|item| item.get("Name").and_then(Value::as_str).map(String::from))
        .collect();
    let missing: Vec<String> = names
        .iter()
        .filter(|n| !found_names.iter().any(|f| f.eq_ignore_ascii_case(n)))
        .cloned()
        .collect();
    let results = raw_results
        .iter()
        .map(|item| normalize_info_item(item))
        .collect::<Vec<_>>();
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "requested": names,
        "found": found_names,
        "missing": missing,
        "results": results,
    }))?)
}

async fn arch_status() -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("miyu-arch-status/0.1")
        .build()?;

    let event_url = format!(
        "{}/api/getEventFeed/{}",
        ARCH_STATUS_BASE_URL, ARCH_STATUS_PAGE_ID
    );
    let monitor_url = format!(
        "{}/api/getMonitorList/{}",
        ARCH_STATUS_BASE_URL, ARCH_STATUS_PAGE_ID
    );

    let (event_resp, monitor_resp) = tokio::try_join!(
        client.get(&event_url).send(),
        client.get(&monitor_url).send()
    )?;

    let event_data: Value = event_resp.error_for_status()?.json().await?;
    let monitor_data: Value = monitor_resp.error_for_status()?.json().await?;

    let events = event_data
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let monitors = monitor_data
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let latest_event = events.first().map(|e| normalize_status_event(e));
    let aur_monitor = find_aur_monitor(&monitors);
    let monitor_id = aur_monitor
        .and_then(|m| m.get("monitorId"))
        .and_then(Value::as_u64);

    let monitor_detail = if let Some(mid) = monitor_id {
        let detail_url = format!(
            "{}/api/getMonitor/{}?m={}",
            ARCH_STATUS_BASE_URL, ARCH_STATUS_PAGE_ID, mid
        );
        let resp = client.get(&detail_url).send().await?;
        if resp.status().is_success() {
            resp.json::<Value>().await.unwrap_or(json!({}))
        } else {
            json!({})
        }
    } else {
        json!({})
    };

    let detail_monitor = monitor_detail.get("monitor").cloned().unwrap_or(json!({}));
    let monitor_status = detail_monitor
        .get("statusClass")
        .and_then(Value::as_str)
        .or_else(|| {
            aur_monitor
                .and_then(|m| m.get("statusClass"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    let current_state = normalize_current_state(monitor_status);
    let (is_degraded, degraded_reason) =
        derive_degraded_state(monitor_status, latest_event.as_ref());

    let logs = detail_monitor
        .get("logs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let latest_down = find_latest_down(&logs);

    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "current_state": current_state,
        "is_degraded": is_degraded,
        "degraded_reason": degraded_reason,
        "latest_down": latest_down,
        "latest_event": latest_event,
        "monitor": {
            "name": aur_monitor.and_then(|m| m.get("name")).and_then(Value::as_str).unwrap_or("AUR"),
            "status_class": monitor_status,
            "monitor_id": monitor_id,
        },
        "source": ARCH_STATUS_BASE_URL,
    }))?)
}

async fn archwiki(args: Value) -> Result<String> {
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("auto");
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if mode == "search" || (mode == "auto" && title.is_empty()) {
        let q = if query.is_empty() { title } else { query };
        let url = format!("https://wiki.archlinux.org/api.php?action=opensearch&search={}&limit=8&namespace=0&format=json", urlencoding::encode(q));
        let data: Value = reqwest::get(url).await?.error_for_status()?.json().await?;
        if mode == "search" {
            return Ok(serde_json::to_string_pretty(&data)?);
        }
        if let Some(first) = data
            .get(1)
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str)
        {
            return fetch_archwiki_page(first).await;
        }
    }
    fetch_archwiki_page(if title.is_empty() { query } else { title }).await
}

async fn fetch_archwiki_page(title: &str) -> Result<String> {
    if title.trim().is_empty() {
        bail!("query or title is required")
    }
    let url = format!(
        "https://wiki.archlinux.org/api.php?action=parse&page={}&prop=text&format=json",
        urlencoding::encode(title)
    );
    let response = reqwest::get(url).await?.error_for_status()?;
    let data: Value =
        http_response::read_json(response, http_response::MAX_HTML_RESPONSE_BYTES).await?;
    let html = data
        .pointer("/parse/text/*")
        .and_then(Value::as_str)
        .unwrap_or_default();
    html_conversion::to_markdown(html.to_string()).await
}

fn required(args: &Value, key: &str) -> Result<String> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        bail!("{key} is required")
    } else {
        Ok(value.to_string())
    }
}

fn as_list(value: &Value) -> Vec<Value> {
    match value {
        Value::Null => Vec::new(),
        Value::Array(arr) => arr.clone(),
        _ => vec![value.clone()],
    }
}

fn format_timestamp(value: &Value) -> Option<String> {
    let ts = value.as_i64()?;
    let dt = chrono::DateTime::from_timestamp(ts, 0)?;
    Some(dt.to_rfc3339())
}

fn normalize_search_item(item: &Value) -> Value {
    let name = item.get("Name").and_then(Value::as_str).unwrap_or_default();
    let last_mod = item.get("LastModified").unwrap_or(&Value::Null);
    json!({
        "name": name,
        "package_base": item.get("PackageBase"),
        "version": item.get("Version"),
        "description": item.get("Description"),
        "votes": item.get("NumVotes"),
        "popularity": item.get("Popularity"),
        "maintainer": item.get("Maintainer"),
        "out_of_date": item.get("OutOfDate").map(|v| !v.is_null()).unwrap_or(false),
        "out_of_date_at": item.get("OutOfDate"),
        "last_modified": item.get("LastModified"),
        "last_modified_iso": format_timestamp(last_mod),
        "upstream_url": item.get("URL"),
        "aur_url": if name.is_empty() { Value::Null } else { json!(format!("https://aur.archlinux.org/packages/{name}")) },
    })
}

fn normalize_info_item(item: &Value) -> Value {
    let name = item.get("Name").and_then(Value::as_str).unwrap_or_default();
    let last_mod = item.get("LastModified").unwrap_or(&Value::Null);
    let first_sub = item.get("FirstSubmitted").unwrap_or(&Value::Null);
    json!({
        "name": name,
        "package_base": item.get("PackageBase"),
        "version": item.get("Version"),
        "description": item.get("Description"),
        "votes": item.get("NumVotes"),
        "popularity": item.get("Popularity"),
        "maintainer": item.get("Maintainer"),
        "out_of_date": item.get("OutOfDate").map(|v| !v.is_null()).unwrap_or(false),
        "out_of_date_at": item.get("OutOfDate"),
        "first_submitted": item.get("FirstSubmitted"),
        "first_submitted_iso": format_timestamp(first_sub),
        "last_modified": item.get("LastModified"),
        "last_modified_iso": format_timestamp(last_mod),
        "upstream_url": item.get("URL"),
        "aur_url": if name.is_empty() { Value::Null } else { json!(format!("https://aur.archlinux.org/packages/{name}")) },
        "url_path": item.get("URLPath"),
        "license": as_list(item.get("License").unwrap_or(&Value::Null)),
        "keywords": as_list(item.get("Keywords").unwrap_or(&Value::Null)),
        "depends": as_list(item.get("Depends").unwrap_or(&Value::Null)),
        "make_depends": as_list(item.get("MakeDepends").unwrap_or(&Value::Null)),
        "check_depends": as_list(item.get("CheckDepends").unwrap_or(&Value::Null)),
        "opt_depends": as_list(item.get("OptDepends").unwrap_or(&Value::Null)),
        "provides": as_list(item.get("Provides").unwrap_or(&Value::Null)),
        "conflicts": as_list(item.get("Conflicts").unwrap_or(&Value::Null)),
    })
}

fn find_aur_monitor(monitors: &[Value]) -> Option<&Value> {
    for monitor in monitors {
        let name = monitor
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let url = monitor
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        if name == "aur" || url.contains("aur.archlinux.org") {
            return Some(monitor);
        }
    }
    monitors.first()
}

fn normalize_current_state(status_class: &str) -> &'static str {
    match status_class.to_lowercase().as_str() {
        "success" => "up",
        "danger" | "down" | "error" => "down",
        _ => "unknown",
    }
}

fn derive_degraded_state(
    monitor_status: &str,
    latest_event: Option<&Value>,
) -> (bool, Option<String>) {
    let status = monitor_status.to_lowercase();
    if let Some(event) = latest_event {
        let is_active = event
            .get("is_active")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let content = event
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let title = event
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let affected: Vec<String> = event
            .get("affected_services")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let aur_affected = affected
            .iter()
            .any(|s| s.to_lowercase().contains("aur.archlinux.org") || s.to_lowercase() == "aur");
        let mentions_aur = content.contains("aur.archlinux.org")
            || title.contains("aur")
            || content.contains("aur");
        if is_active && (aur_affected || mentions_aur) {
            return (
                true,
                Some("Arch status page has an unresolved incident affecting AUR".to_string()),
            );
        }
    }
    if status == "warning" || status == "degraded" {
        return (
            true,
            Some("AUR monitor status is not fully healthy".to_string()),
        );
    }
    (false, None)
}

fn normalize_status_event(item: &Value) -> Value {
    let content = item
        .get("content")
        .or_else(|| item.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let ended_at = item
        .get("endDateGMT")
        .or_else(|| item.get("endDate"))
        .cloned();
    let is_active = ended_at.as_ref().map(|v| v.is_null()).unwrap_or(true);
    let timestamp = item.get("timestamp").unwrap_or(&Value::Null);
    json!({
        "title": item.get("title"),
        "type": item.get("type"),
        "event_type": item.get("eventType"),
        "is_active": is_active,
        "started_at": format_timestamp(timestamp),
        "started_at_raw": item.get("timeGMT"),
        "ended_at": ended_at,
        "content": content,
        "status": item.get("status"),
        "affected_services": extract_affected_services(content),
    })
}

fn extract_affected_services(content: &str) -> Vec<String> {
    let mut services = Vec::new();
    let mut capture = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.to_lowercase().starts_with("affected services:") {
            capture = true;
            continue;
        }
        if capture && line.starts_with('-') {
            services.push(line.trim_start_matches("- ").trim().to_string());
            continue;
        }
        if capture && !services.is_empty() {
            break;
        }
    }
    services
}

fn find_latest_down(logs: &[Value]) -> Option<Value> {
    for item in logs {
        let label = item
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let class = item
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        if label != "down" && class != "danger" {
            continue;
        }
        let reason_detail = item
            .get("reason")
            .and_then(|r| r.get("detail"))
            .and_then(|d| d.get("short"))
            .and_then(Value::as_str);
        return Some(json!({
            "started_at": item.get("dateGMTISO").or_else(|| item.get("timeGMT")),
            "duration": item.get("duration"),
            "reason": reason_detail,
        }));
    }
    None
}

async fn archlinux_news(args: Value, state_dir: &std::path::Path) -> Result<String> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 20) as usize;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("miyu-archlinux-news/0.1")
        .build()?;
    let resp = client.get(ARCH_NEWS_FEED_URL).send().await?;
    if !resp.status().is_success() {
        bail!("Arch news feed returned HTTP {}", resp.status());
    }
    let xml = resp.text().await?;
    let articles = parse_rss_feed(&xml, limit);

    let cache_path = state_dir.join(ARCH_NEWS_CACHE_FILE);
    let last_seen_url: Option<String> = if cache_path.is_file() {
        let raw = std::fs::read_to_string(&cache_path)?;
        serde_json::from_str::<Value>(&raw).ok().and_then(|v| {
            v.get("last_seen_url")
                .and_then(Value::as_str)
                .map(String::from)
        })
    } else {
        None
    };

    let new_count = if let Some(ref last_url) = last_seen_url {
        articles.iter().take_while(|a| &a.url != last_url).count()
    } else {
        articles.len()
    };

    let result_articles: Vec<Value> = articles
        .iter()
        .enumerate()
        .map(|(i, a)| {
            json!({
                "title": a.title,
                "url": a.url,
                "published": a.published,
                "author": a.author,
                "description": a.description,
                "is_new": i < new_count,
            })
        })
        .collect();

    if let Some(first) = articles.first() {
        let cache = json!({ "last_seen_url": first.url });
        std::fs::write(&cache_path, serde_json::to_string_pretty(&cache)?)?;
    }

    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "total": result_articles.len(),
        "new_count": new_count,
        "last_seen_url": last_seen_url,
        "articles": result_articles,
        "source": ARCH_NEWS_FEED_URL,
    }))?)
}

struct NewsArticle {
    title: String,
    url: String,
    published: String,
    author: String,
    description: String,
}

fn parse_rss_feed(xml: &str, limit: usize) -> Vec<NewsArticle> {
    let mut articles = Vec::new();
    let mut current: Option<NewsArticle> = None;
    let mut tag_stack = Vec::new();
    let mut in_item = false;

    for token in tokenize_xml(xml) {
        match token {
            XmlToken::OpenTag(tag) => {
                tag_stack.push(tag.clone());
                if tag == "item" {
                    in_item = true;
                    current = Some(NewsArticle {
                        title: String::new(),
                        url: String::new(),
                        published: String::new(),
                        author: String::new(),
                        description: String::new(),
                    });
                }
            }
            XmlToken::CloseTag(tag) => {
                if tag == "item" {
                    if let Some(article) = current.take() {
                        articles.push(article);
                    }
                    in_item = false;
                    if articles.len() >= limit {
                        break;
                    }
                }
                tag_stack.pop();
            }
            XmlToken::Text(text) => {
                if !in_item {
                    continue;
                }
                let current_tag = tag_stack.last().map(|s| s.as_str()).unwrap_or("");
                let article = current.as_mut().unwrap();
                match current_tag {
                    "title" => {
                        if article.title.is_empty() {
                            article.title = decode_xml_entities(&text);
                        }
                    }
                    "link" => {
                        if article.url.is_empty() {
                            article.url = text.trim().to_string();
                        }
                    }
                    "pubDate" => {
                        if article.published.is_empty() {
                            article.published = text.trim().to_string();
                        }
                    }
                    "dc:creator" | "author" => {
                        if article.author.is_empty() {
                            article.author = decode_xml_entities(&text);
                        }
                    }
                    "description" => {
                        if article.description.is_empty() {
                            let stripped = html_conversion::to_text_lossy(&text, 2000);
                            article.description = clip_string(&stripped, 500);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    articles
}

enum XmlToken {
    OpenTag(String),
    CloseTag(String),
    Text(String),
}

fn tokenize_xml(xml: &str) -> Vec<XmlToken> {
    let mut tokens = Vec::new();
    let mut chars = xml.chars().peekable();
    let mut text = String::new();

    while let Some(ch) = chars.next() {
        if ch == '<' {
            if !text.trim().is_empty() {
                tokens.push(XmlToken::Text(std::mem::take(&mut text).trim().to_string()));
            } else {
                text.clear();
            }
            let mut tag_content = String::new();
            let mut is_closing = false;
            if chars.peek() == Some(&'/') {
                is_closing = true;
                chars.next();
            }
            while let Some(&c) = chars.peek() {
                if c == '>' {
                    chars.next();
                    break;
                }
                tag_content.push(c);
                chars.next();
            }
            let tag_content = tag_content.trim();
            if tag_content.is_empty()
                || tag_content.starts_with('?')
                || tag_content.starts_with('!')
            {
                continue;
            }
            let tag_name = tag_content
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            if is_closing {
                tokens.push(XmlToken::CloseTag(tag_name));
            } else {
                if tag_content.ends_with('/') {
                    tokens.push(XmlToken::OpenTag(tag_name.clone()));
                    tokens.push(XmlToken::CloseTag(tag_name));
                } else {
                    tokens.push(XmlToken::OpenTag(tag_name));
                }
            }
        } else {
            text.push(ch);
        }
    }
    tokens
}

fn decode_xml_entities(s: &str) -> String {
    // `&amp;` 必须最后解:排第一会把字面 `&amp;lt;` 双重解码成 `<`。
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn clip_string(s: &str, max_chars: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod news_tests {
    use super::*;

    #[test]
    fn parses_simple_rss_feed() {
        let xml = r#"<?xml version="1.0"?>
<rss>
  <channel>
    <item>
      <title>Test News &amp; Update</title>
      <link>https://archlinux.org/news/test</link>
      <pubDate>Mon, 01 Jan 2024 00:00:00 +0000</pubDate>
      <dc:creator>Arch Team</dc:creator>
      <description>&lt;p&gt;Some description&lt;/p&gt;</description>
    </item>
    <item>
      <title>Second News</title>
      <link>https://archlinux.org/news/second</link>
      <pubDate>Tue, 02 Jan 2024 00:00:00 +0000</pubDate>
      <dc:creator>Another Author</dc:creator>
      <description>Plain text description</description>
    </item>
  </channel>
</rss>"#;
        let articles = parse_rss_feed(xml, 10);
        assert_eq!(articles.len(), 2);
        assert_eq!(articles[0].title, "Test News & Update");
        assert_eq!(articles[0].url, "https://archlinux.org/news/test");
        assert_eq!(articles[0].author, "Arch Team");
        assert_eq!(articles[1].title, "Second News");
    }

    #[test]
    fn parses_rss_feed_with_limit() {
        let xml = r#"<?xml version="1.0"?>
<rss>
  <channel>
    <item><title>A</title><link>url-a</link><pubDate></pubDate><dc:creator></dc:creator><description></description></item>
    <item><title>B</title><link>url-b</link><pubDate></pubDate><dc:creator></dc:creator><description></description></item>
    <item><title>C</title><link>url-c</link><pubDate></pubDate><dc:creator></dc:creator><description></description></item>
  </channel>
</rss>"#;
        let articles = parse_rss_feed(xml, 2);
        assert_eq!(articles.len(), 2);
        assert_eq!(articles[0].title, "A");
        assert_eq!(articles[1].title, "B");
    }

    #[test]
    fn decodes_xml_entities() {
        assert_eq!(
            decode_xml_entities("a &amp; b &lt; c &gt; d"),
            "a & b < c > d"
        );
        assert_eq!(decode_xml_entities("&quot;hi&quot;"), "\"hi\"");
    }
}
