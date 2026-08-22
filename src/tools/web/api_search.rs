//! 走 API 的搜索供应商。
//!
//! Tavily / Firecrawl / AnySearch / Exa / SearXNG。它们返回结构化 JSON，所以这
//! 一层只做字段映射，不解析 HTML。
//!
//! Exa 有两条路：MCP 端点和公开接口，后者是前者不可用时的兜底。

use crate::tools::web::*;

pub(in crate::tools::web) async fn search_tavily(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
    keys: &[String],
) -> Result<String> {
    let keys = keys
        .iter()
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        bail!("missing Tavily API key")
    }
    let payload = json!({"query": query, "max_results": max_results.min(20), "search_depth": "basic", "include_answer": false, "include_raw_content": "markdown"});
    let order = ordered_key_positions("tavily", keys.len());
    if order.is_empty() {
        bail!("all Tavily API keys are cooling down")
    }
    let mut errors = Vec::new();
    for index in order {
        let key = keys[index];
        let response = match client
            .post("https://api.tavily.com/search")
            .bearer_auth(key)
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                let message = format!("key#{} request failed: {err}", index + 1);
                mark_key_failure("tavily", index, &message);
                errors.push(message);
                continue;
            }
        };
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let message = format!(
                "key#{} HTTP {}: {}",
                index + 1,
                status.as_u16(),
                clip(&body, 240)
            );
            if let Some(duration) = cooldown_for_status(status.as_u16()) {
                if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
                    scheduler.mark_failure(key_cooldown_id("tavily", index), duration);
                }
            }
            errors.push(message);
            continue;
        }
        let data: Value = match response.json().await {
            Ok(data) => data,
            Err(err) => {
                errors.push(format!("key#{} invalid JSON: {err}", index + 1));
                continue;
            }
        };
        let results = data
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        match format_search_results(query, "Tavily", results, max_results) {
            Ok(output) => {
                mark_key_success("tavily", index);
                return Ok(output);
            }
            Err(err) => errors.push(format!("key#{}: {err}", index + 1)),
        }
    }
    bail!(
        "Tavily failed for all configured keys: {}",
        errors.join(" | ")
    )
}

pub(in crate::tools::web) async fn search_firecrawl(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
    keys: &[String],
) -> Result<String> {
    let keys = keys
        .iter()
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        bail!("missing Firecrawl API key")
    }
    let payload = json!({"query": query, "limit": max_results.min(20), "sources": [{"type":"web"}], "scrapeOptions": {"formats": [{"type":"markdown"}], "onlyMainContent": true}});
    let order = ordered_key_positions("firecrawl", keys.len());
    if order.is_empty() {
        bail!("all Firecrawl API keys are cooling down")
    }
    let mut errors = Vec::new();
    for index in order {
        let key = keys[index];
        let response = match client
            .post("https://api.firecrawl.dev/v2/search")
            .bearer_auth(key)
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                let message = format!("key#{} request failed: {err}", index + 1);
                mark_key_failure("firecrawl", index, &message);
                errors.push(message);
                continue;
            }
        };
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let message = format!(
                "key#{} HTTP {}: {}",
                index + 1,
                status.as_u16(),
                clip(&body, 240)
            );
            if let Some(duration) = cooldown_for_status(status.as_u16()) {
                if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
                    scheduler.mark_failure(key_cooldown_id("firecrawl", index), duration);
                }
            }
            errors.push(message);
            continue;
        }
        let data: Value = match response.json().await {
            Ok(data) => data,
            Err(err) => {
                errors.push(format!("key#{} invalid JSON: {err}", index + 1));
                continue;
            }
        };
        let raw = firecrawl_results(&data, max_results);
        match format_search_results(query, "Firecrawl", raw, max_results) {
            Ok(output) => {
                mark_key_success("firecrawl", index);
                return Ok(output);
            }
            Err(err) => errors.push(format!("key#{}: {err}", index + 1)),
        }
    }
    bail!(
        "Firecrawl failed for all configured keys: {}",
        errors.join(" | ")
    )
}

pub(in crate::tools::web) async fn search_anysearch(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
    keys: &[String],
) -> Result<String> {
    let keys = keys
        .iter()
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        bail!("missing AnySearch API key")
    }
    let payload = json!({"query": query, "max_results": max_results.min(20)});
    let order = ordered_key_positions("anysearch", keys.len());
    if order.is_empty() {
        bail!("all AnySearch API keys are cooling down")
    }
    let mut errors = Vec::new();
    for index in order {
        let key = keys[index];
        let response = match client
            .post("https://api.anysearch.com/v1/search")
            .bearer_auth(key)
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                let message = format!("key#{} request failed: {err}", index + 1);
                mark_key_failure("anysearch", index, &message);
                errors.push(message);
                continue;
            }
        };
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let message = format!(
                "key#{} HTTP {}: {}",
                index + 1,
                status.as_u16(),
                clip(&body, 240)
            );
            if let Some(duration) = cooldown_for_status(status.as_u16()) {
                if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
                    scheduler.mark_failure(key_cooldown_id("anysearch", index), duration);
                }
            }
            errors.push(message);
            continue;
        }
        let data: Value = match response.json().await {
            Ok(data) => data,
            Err(err) => {
                errors.push(format!("key#{} invalid JSON: {err}", index + 1));
                continue;
            }
        };
        let raw = anysearch_results(&data, max_results);
        match format_search_results(query, "AnySearch", raw, max_results) {
            Ok(output) => {
                mark_key_success("anysearch", index);
                return Ok(output);
            }
            Err(err) => errors.push(format!("key#{}: {err}", index + 1)),
        }
    }
    bail!(
        "AnySearch failed for all configured keys: {}",
        errors.join(" | ")
    )
}

pub(in crate::tools::web) async fn search_exa(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
    keys: &[String],
) -> Result<String> {
    let keys = keys
        .iter()
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        // 无 key：走官方 MCP 端点的免费公共额度
        return search_exa_public(client, query, max_results).await;
    }
    let payload = json!({
        "query": query,
        "numResults": max_results.min(10),
        "contents": {"text": true},
    });
    let order = ordered_key_positions("exa", keys.len());
    if order.is_empty() {
        bail!("all Exa API keys are cooling down")
    }
    let mut errors = Vec::new();
    for index in order {
        let key = keys[index];
        let response = match client
            .post("https://api.exa.ai/search")
            .header("x-api-key", key)
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                let message = format!("key#{} request failed: {err}", index + 1);
                mark_key_failure("exa", index, &message);
                errors.push(message);
                continue;
            }
        };
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let message = format!(
                "key#{} HTTP {}: {}",
                index + 1,
                status.as_u16(),
                clip(&body, 240)
            );
            if let Some(duration) = cooldown_for_status(status.as_u16()) {
                if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
                    scheduler.mark_failure(key_cooldown_id("exa", index), duration);
                }
            }
            errors.push(message);
            continue;
        }
        let data: Value = match response.json().await {
            Ok(data) => data,
            Err(err) => {
                errors.push(format!("key#{} invalid JSON: {err}", index + 1));
                continue;
            }
        };
        let raw = data
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(max_results)
            .collect::<Vec<_>>();
        match format_search_results(query, "Exa", raw, max_results) {
            Ok(output) => {
                mark_key_success("exa", index);
                return Ok(output);
            }
            Err(err) => errors.push(format!("key#{}: {err}", index + 1)),
        }
    }
    bail!("Exa failed for all configured keys: {}", errors.join(" | "))
}

pub(in crate::tools::web) const EXA_MCP_ENDPOINT: &str = "https://mcp.exa.ai/mcp";

/// 免 key 通道的 MCP 端点，可用 MIYU_EXA_MCP_ENDPOINT 覆盖（自建代理/测试用）
pub(in crate::tools::web) fn exa_mcp_endpoint() -> String {
    std::env::var("MIYU_EXA_MCP_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| EXA_MCP_ENDPOINT.to_string())
}

/// 通过 Exa 官方 MCP 端点使用免费公共额度（无需 API key）。
/// 额度用尽或被拒会 bail，错误串带 HTTP 状态码，交由上层 cooldown 冷却并回退爬虫。
pub(in crate::tools::web) async fn search_exa_public(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Result<String> {
    let endpoint = exa_mcp_endpoint();
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "miyu", "version": env!("CARGO_PKG_VERSION")},
        },
    });
    let response = client
        .post(&endpoint)
        .header("Accept", "application/json, text/event-stream")
        .json(&initialize)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!(
            "Exa free quota initialize HTTP {}: {}",
            status.as_u16(),
            clip(&body, 240)
        );
    }
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let _ = parse_mcp_body(response).await;

    let with_session = |request: reqwest::RequestBuilder| match &session_id {
        Some(id) => request.header("mcp-session-id", id.as_str()),
        None => request,
    };
    // 部分服务端要求 initialized 通知；失败不致命
    let _ = with_session(client.post(&endpoint))
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await;

    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "web_search_exa",
            "arguments": {"query": query, "numResults": max_results.min(10)},
        },
    });
    let response = with_session(client.post(&endpoint))
        .header("Accept", "application/json, text/event-stream")
        .json(&call)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!(
            "Exa free quota HTTP {}: {}",
            status.as_u16(),
            clip(&body, 240)
        );
    }
    let body = parse_mcp_body(response).await?;
    if let Some(error) = body.get("error") {
        bail!("Exa free quota RPC error: {}", clip(&error.to_string(), 240));
    }
    let result = body.get("result").cloned().unwrap_or(Value::Null);
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let text = mcp_content_text(&result);
        // MCP 把限额错误包在正常响应里，补上 429 字样让 cooldown 识别
        let hint = if text.to_ascii_lowercase().contains("rate limit")
            || text.to_ascii_lowercase().contains("quota")
        {
            " (HTTP 429)"
        } else {
            ""
        };
        bail!("Exa free quota tool error{hint}: {}", clip(&text, 240));
    }
    let text = mcp_content_text(&result);
    if text.trim().is_empty() {
        bail!("Exa free quota returned no usable results")
    }
    // 兼容两种返回：JSON 字符串（含 results 数组）或 "Title:/URL:/..." 纯文本块
    if let Ok(parsed) = serde_json::from_str::<Value>(text.trim()) {
        let results = parsed
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(max_results)
            .collect::<Vec<_>>();
        if !results.is_empty() {
            return format_search_results(query, "Exa (free quota)", results, max_results);
        }
    }
    let results = exa_public_results(&text, max_results);
    if !results.is_empty() {
        return format_search_results(query, "Exa (free quota)", results, max_results);
    }
    Ok(format!(
        "## Search results for: {query}\n**Provider**: Exa (free quota)\n\n{}",
        clip(&text, 8_000)
    ))
}

/// 解析 Exa MCP 免费额度返回的纯文本结果（Title:/URL:/Published:/Author:/Highlights:
/// 字段行，多条结果以 --- 行分隔）为统一的结果对象。
pub(in crate::tools::web) fn exa_public_results(text: &str, max_results: usize) -> Vec<Value> {
    text.split("\n---\n")
        .filter_map(|block| {
            let block = block.trim();
            if block.is_empty() {
                return None;
            }
            let mut title = String::new();
            let mut url = String::new();
            let mut published = String::new();
            let mut author = String::new();
            let mut body = Vec::new();
            let mut in_body = false;
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("Title: ") {
                    title = value.trim().to_string();
                    in_body = false;
                } else if let Some(value) = line.strip_prefix("URL: ") {
                    url = value.trim().to_string();
                } else if let Some(value) = line.strip_prefix("Published: ") {
                    published = value.trim().to_string();
                } else if let Some(value) = line.strip_prefix("Author: ") {
                    author = value.trim().to_string();
                } else if let Some(rest) = line
                    .strip_prefix("Highlights:")
                    .or_else(|| line.strip_prefix("Text:"))
                    .or_else(|| line.strip_prefix("Summary:"))
                {
                    in_body = true;
                    let rest = rest.trim();
                    if !rest.is_empty() {
                        body.push(rest.to_string());
                    }
                } else if in_body {
                    body.push(line.to_string());
                }
            }
            if title.is_empty() && url.is_empty() {
                return None;
            }
            let mut meta = Vec::new();
            if !published.is_empty() && published != "N/A" {
                meta.push(format!("Published: {published}"));
            }
            if !author.is_empty() && author != "N/A" {
                meta.push(format!("Author: {author}"));
            }
            Some(json!({
                "title": title,
                "url": url,
                "snippet": meta.join(" · "),
                "raw_content": body.join("\n"),
            }))
        })
        .take(max_results)
        .collect()
}

/// 解析 MCP Streamable HTTP 响应：application/json 直接解析，
/// text/event-stream 取最后一条 data: 行的 JSON。
pub(in crate::tools::web) async fn parse_mcp_body(response: reqwest::Response) -> Result<Value> {
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let text = response.text().await?;
    if content_type.contains("text/event-stream") {
        let mut last = None;
        for line in text.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                if let Ok(value) = serde_json::from_str::<Value>(data.trim()) {
                    last = Some(value);
                }
            }
        }
        return last.ok_or_else(|| anyhow::anyhow!("empty MCP event stream"));
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|err| anyhow::anyhow!("invalid MCP JSON: {err}"))
}

pub(in crate::tools::web) fn mcp_content_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub(in crate::tools::web) async fn search_searxng(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
    base_url: &str,
) -> Result<String> {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        bail!("missing SearXNG base URL")
    }
    let url = format!(
        "{base_url}/search?q={}&format=json&language=auto&safesearch=0",
        urlencoding::encode(query)
    );
    let data: Value = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let results = data
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        bail!("SearXNG returned no results")
    }
    format_search_results(query, "SearXNG", results, max_results)
}

pub(in crate::tools::web) fn firecrawl_results(data: &Value, max_results: usize) -> Vec<Value> {
    let data_value = data.get("data").unwrap_or(data);
    let results = data_value
        .as_array()
        .or_else(|| data_value.get("web").and_then(Value::as_array))
        .or_else(|| data_value.get("results").and_then(Value::as_array));
    results
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(max_results)
        .collect()
}

pub(in crate::tools::web) fn anysearch_results(data: &Value, max_results: usize) -> Vec<Value> {
    let results = data
        .get("results")
        .and_then(Value::as_array)
        .or_else(|| data.pointer("/data/results").and_then(Value::as_array));
    results
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(max_results)
        .collect()
}
