use super::{html_conversion, http_response, ToolRegistry, ToolSpec};
use anyhow::{bail, Result};
use serde_json::{json, Value};

const ARCH_BASE: &str = "https://man.archlinux.org";
const MAN7_BASE: &str = "https://man7.org/linux/man-pages";

/// 搜索与取页合并成一件 `online_man`(08-17):同一份手册的两步操作,
/// 拆成两个工具只是让 tools 数组多背一份外壳。
pub fn register(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new(
        "online_man",
        "在线 Linux 手册。action=search 在 Arch manual pages 搜索；action=read 抓取具体手册页（Arch man pages 或 man7.org）。",
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["search", "read"], "description": "search 搜索，read 取页。" },
                "query": { "type": "string", "description": "action=search 必填：搜索词。" },
                "name": { "type": "string", "description": "action=read 必填：手册页名称。" },
                "section": { "type": "string", "description": "可选手册章节号。" },
                "language": { "type": "string", "description": "可选语言，默认 en。" },
                "limit": { "type": "integer", "description": "仅 search：最多结果数，默认 10。" },
                "source": { "type": "string", "enum": ["auto", "arch", "man7"], "description": "仅 read：来源站点，默认 auto。" },
                "max_chars": { "type": "integer", "description": "仅 read：最多返回字符数。正常阅读至少给 8000；只要摘录时才调小。" }
            },
            "required": ["action"],
            "additionalProperties": false
        }),
        |args| async move {
            match args.get("action").and_then(Value::as_str).unwrap_or_default() {
                "search" => search(args).await,
                "read" => get_page(args).await,
                other => anyhow::bail!("unknown action: {other}; expected search or read"),
            }
        },
    ));
}
async fn search(args: Value) -> Result<String> {
    let query = required(&args, "query")?;
    let section = args
        .get("section")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let language = args
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("en")
        .trim();
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .min(50) as usize;
    let mut url = format!(
        "{ARCH_BASE}/search?q={}&lang={}",
        urlencoding::encode(&query),
        urlencoding::encode(language)
    );
    if !section.is_empty() {
        url.push_str(&format!("&section={}", urlencoding::encode(section)));
    }
    let html = fetch_text(&url).await?;
    let mut results = Vec::new();
    for line in html.lines() {
        if let Some(pos) = line.find("/man/") {
            let tail = &line[pos + 5..];
            // 在 href 闭合处截断，避免把 `">systemd(1)</a>` 之类 HTML 垃圾带进链接。
            let href_end = tail
                .find(['"', '\'', '<', '>'])
                .unwrap_or(tail.len());
            let href = &tail[..href_end];
            let end = href.find('.').unwrap_or(href.len());
            let name = href[..end].trim_matches('/');
            if !name.is_empty() && !results.iter().any(|item: &String| item.contains(name)) {
                results.push(format!("- {name}: {ARCH_BASE}/man/{href}"));
            }
        }
        if results.len() >= limit {
            break;
        }
    }
    if results.is_empty() {
        Ok(format!("No man page search results for {query}"))
    } else {
        Ok(results.join("\n"))
    }
}

async fn get_page(args: Value) -> Result<String> {
    let name = required(&args, "name")?;
    let section = args
        .get("section")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let source = args.get("source").and_then(Value::as_str).unwrap_or("auto");
    let language = args.get("language").and_then(Value::as_str).unwrap_or("en");
    let max_chars = args
        .get("max_chars")
        .and_then(Value::as_u64)
        .unwrap_or(16_000)
        .clamp(2_000, 100_000) as usize;
    let sections: Vec<&str> = if section.is_empty() {
        vec!["1", "8", "5", "7", "2", "3", "4", "6"]
    } else {
        vec![section]
    };
    let try_arch = source == "auto" || source == "arch";
    let try_man7 = source == "auto" || source == "man7";
    if try_arch {
        for sec in &sections {
            let url = format!("{ARCH_BASE}/man/{name}.{sec}.{language}.txt");
            if let Ok(text) = fetch_text(&url).await {
                return Ok(clip(&format!("Source: {url}\n\n{text}"), max_chars));
            }
        }
    }
    if try_man7 {
        for sec in &sections {
            let sec_initial = sec.chars().next().unwrap_or('1');
            let url = format!("{MAN7_BASE}/man{sec_initial}/{name}.{sec}.html");
            if let Ok(html) = fetch_text(&url).await {
                let text = html_conversion::to_text_async(html, 120).await?;
                return Ok(clip(&format!("Source: {url}\n\n{text}"), max_chars));
            }
        }
    }
    bail!("man page not found: {name}")
}

async fn fetch_text(url: &str) -> Result<String> {
    let response = http_response::shared_client()
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    http_response::read_text(response, http_response::MAX_HTML_RESPONSE_BYTES).await
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

fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!(
            "{}\n...[truncated]",
            text.chars().take(max_chars).collect::<String>()
        )
    }
}
