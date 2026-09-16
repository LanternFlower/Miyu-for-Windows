use anyhow::{anyhow, bail, Context, Result};
use htmd::{
    element_handler::{HandlerResult, Handlers},
    Element, HtmlToMarkdown, Node,
};
use markup5ever_rcdom::NodeData;
use std::{rc::Rc, sync::LazyLock};
use tokio::sync::Semaphore;

const MAX_HTML_BYTES: usize = 5 * 1024 * 1024;
const MAX_TEXT_HTML_BYTES: usize = 512 * 1024;
const MAX_TEXT_TAGS: usize = 20_000;
const MAX_FRAGMENT_HTML_BYTES: usize = 64 * 1024;
const MAX_FRAGMENT_WIDTH: usize = 240;
const MAX_CONVERTED_BYTES: usize = 10 * 1024 * 1024;
const MAX_DOM_NODES: usize = 100_000;
const MAX_DOM_DEPTH: usize = 64;
const MAX_CONCURRENT_CONVERSIONS: usize = 2;

static HTML_CONVERSION_SLOTS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_CONVERSIONS);
static MARKDOWN_CONVERTER: LazyLock<HtmlToMarkdown> = LazyLock::new(|| {
    HtmlToMarkdown::builder()
        .add_handler(vec!["pre"], preformatted_handler)
        .add_handler(vec!["s", "del"], strikethrough_handler)
        .add_handler(vec!["q"], quote_handler)
        .add_handler(vec!["cite"], citation_handler)
        .add_handler(
            vec!["details", "summary", "sub", "sup"],
            preserved_tag_handler,
        )
        .add_handler(vec!["iframe"], iframe_handler)
        .build()
});

pub(super) async fn to_markdown(html: String) -> Result<String> {
    ensure_html_size(&html)?;
    run_blocking(move || {
        let tree = MARKDOWN_CONVERTER
            .html_to_tree(&html)
            .context("HTML-to-Markdown parsing failed")?;
        ensure_dom_complexity(&tree)?;
        let markdown = MARKDOWN_CONVERTER.tree_to_markdown(&tree);
        ensure_output_size(&markdown)?;
        Ok(markdown)
    })
    .await
}

pub(super) fn to_text(html: &str, width: usize) -> Result<String> {
    if html.len() > MAX_FRAGMENT_HTML_BYTES {
        bail!("synchronous HTML fragment too large (exceeds 64 KiB limit)")
    }
    ensure_text_complexity(html)?;
    render_text(html, width.min(MAX_FRAGMENT_WIDTH))
}

pub(super) async fn to_text_async(html: String, width: usize) -> Result<String> {
    ensure_text_complexity(&html)?;
    run_blocking(move || render_text(&html, width)).await
}

pub(super) fn to_text_lossy(html: &str, width: usize) -> String {
    match to_text(html, width) {
        Ok(text) => text,
        Err(error) => {
            tracing::warn!(error = %error, "discarding HTML text after conversion failure");
            String::new()
        }
    }
}

async fn run_blocking<T>(operation: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T>
where
    T: Send + 'static,
{
    let permit = HTML_CONVERSION_SLOTS
        .acquire()
        .await
        .context("HTML conversion worker limit closed")?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .context("HTML conversion task panicked")?
}

fn ensure_html_size(html: &str) -> Result<()> {
    if html.len() > MAX_HTML_BYTES {
        bail!("HTML input too large (exceeds 5 MiB limit)")
    }
    Ok(())
}

fn ensure_text_complexity(html: &str) -> Result<()> {
    if html.len() > MAX_TEXT_HTML_BYTES {
        bail!("HTML text input too large (exceeds 512 KiB limit)")
    }
    let tags = html
        .bytes()
        .filter(|byte| *byte == b'<')
        .take(MAX_TEXT_TAGS + 1)
        .count();
    if tags > MAX_TEXT_TAGS {
        bail!("HTML document has too many tags to render safely")
    }
    Ok(())
}

fn render_text(html: &str, width: usize) -> Result<String> {
    let text = std::panic::catch_unwind(|| html2text::from_read(html.as_bytes(), width))
        .map_err(|_| anyhow!("HTML-to-text conversion panicked"))?
        .context("HTML-to-text conversion failed")?;
    ensure_output_size(&text)?;
    Ok(text)
}

fn ensure_output_size(output: &str) -> Result<()> {
    if output.len() > MAX_CONVERTED_BYTES {
        bail!("converted HTML output too large (exceeds 10 MiB limit)")
    }
    Ok(())
}

fn ensure_dom_complexity(root: &Rc<Node>) -> Result<()> {
    let mut stack = vec![(Rc::clone(root), 0_usize)];
    let mut nodes = 0_usize;
    while let Some((node, depth)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_DOM_NODES || depth > MAX_DOM_DEPTH {
            bail!("HTML document is too complex to convert safely")
        }
        stack.extend(
            node.children
                .borrow()
                .iter()
                .map(|child| (Rc::clone(child), depth + 1)),
        );
    }
    Ok(())
}

fn preformatted_handler(_handlers: &dyn Handlers, element: Element<'_>) -> Option<HandlerResult> {
    let mut content = String::new();
    collect_raw_text(element.node, &mut content);
    let content = content.strip_suffix('\n').unwrap_or(&content);
    if content.is_empty() {
        return None;
    }

    let language = element
        .attrs
        .iter()
        .find(|attribute| &attribute.name.local == "class")
        .and_then(|attribute| language_from_classes(&attribute.value))
        .or_else(|| {
            element.node.children.borrow().iter().find_map(|child| {
                let NodeData::Element { name, attrs, .. } = &child.data else {
                    return None;
                };
                if name.local.as_ref() != "code" {
                    return None;
                }
                attrs
                    .borrow()
                    .iter()
                    .find(|attribute| &attribute.name.local == "class")
                    .and_then(|attribute| language_from_classes(&attribute.value))
            })
        })
        .unwrap_or_default();
    let fence = code_fence(content);
    Some(format!("\n\n{fence}{language}\n{content}\n{fence}\n\n").into())
}

fn collect_raw_text(node: &Rc<Node>, output: &mut String) {
    let mut stack = vec![Rc::clone(node)];
    while let Some(node) = stack.pop() {
        if let NodeData::Text { contents } = &node.data {
            output.push_str(&contents.borrow());
            continue;
        }
        stack.extend(node.children.borrow().iter().rev().map(Rc::clone));
    }
}

fn language_from_classes(classes: &str) -> Option<String> {
    let language = classes
        .split_whitespace()
        .find_map(|class| class.strip_prefix("language-"))?;
    (!language.is_empty()
        && language.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '#' | '.' | '_' | '-')
        }))
    .then(|| language.to_string())
}

fn strikethrough_handler(handlers: &dyn Handlers, element: Element<'_>) -> Option<HandlerResult> {
    let content = handlers.walk_children(element.node).content;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Some(content.into());
    }
    let start = content.find(trimmed)?;
    let end = start + trimmed.len();
    Some(format!("{}~~{}~~{}", &content[..start], trimmed, &content[end..]).into())
}

fn quote_handler(handlers: &dyn Handlers, element: Element<'_>) -> Option<HandlerResult> {
    wrap_inline(handlers, element, "\"")
}

fn citation_handler(handlers: &dyn Handlers, element: Element<'_>) -> Option<HandlerResult> {
    wrap_inline(handlers, element, "*")
}

fn wrap_inline(
    handlers: &dyn Handlers,
    element: Element<'_>,
    marker: &str,
) -> Option<HandlerResult> {
    let content = handlers.walk_children(element.node).content;
    (!content.is_empty()).then(|| format!("{marker}{content}{marker}").into())
}

fn preserved_tag_handler(handlers: &dyn Handlers, element: Element<'_>) -> Option<HandlerResult> {
    let content = handlers.walk_children(element.node).content;
    let mut opening = format!("<{}", element.tag);
    for attribute in element.attrs {
        opening.push(' ');
        opening.push_str(&attribute.name.local);
        opening.push_str("=\"");
        push_escaped_attribute(&mut opening, &attribute.value);
        opening.push('"');
    }
    Some(format!("{opening}>{content}</{tag}>", tag = element.tag).into())
}

fn push_escaped_attribute(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '"' => output.push_str("&quot;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
        }
    }
}

fn iframe_handler(_handlers: &dyn Handlers, element: Element<'_>) -> Option<HandlerResult> {
    let source = element
        .attrs
        .iter()
        .find(|attribute| &attribute.name.local == "src")?
        .value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!source.is_empty()).then(|| format!("\n\nEmbedded content: {source}\n\n").into())
}

fn code_fence(content: &str) -> String {
    let mut longest_run = 0;
    let mut current_run = 0;
    for character in content.chars() {
        if character == '`' {
            current_run += 1;
            longest_run = longest_run.max(current_run);
        } else {
            current_run = 0;
        }
    }
    "`".repeat(3.max(longest_run + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preserves_utf8_in_orphan_list_item_inside_quote() {
        let markdown = to_markdown("<blockquote><li>中文</li></blockquote>".to_string())
            .await
            .unwrap();

        assert!(markdown.contains("中文"));
    }

    #[tokio::test]
    async fn preserves_emoji_in_orphan_list_item_inside_quote() {
        let markdown = to_markdown("<blockquote><li>😀</li></blockquote>".to_string())
            .await
            .unwrap();

        assert!(markdown.contains('😀'));
    }

    #[tokio::test]
    async fn handles_multibyte_ordered_list_escape() {
        let markdown = to_markdown("<p>2½. Long shot</p>".to_string())
            .await
            .unwrap();

        assert!(markdown.contains("2½"));
        assert!(markdown.contains("Long shot"));
    }

    #[test]
    fn renders_utf8_html_as_text() {
        let text = to_text("<p>你好 <strong>世界</strong></p>", 120).unwrap();

        assert!(text.contains("你好"));
        assert!(text.contains("世界"));
    }

    #[tokio::test]
    async fn preserves_legacy_markdown_semantics() {
        let markdown = to_markdown(
            "<pre>```shell\ncommand\n~~~</pre><p><s>old</s> <q>quote</q> <cite>source</cite> H<sub>2</sub>O</p><details open><summary>More</summary>Body</details><iframe src=\"https://example.com/embed\"></iframe>"
                .to_string(),
        )
        .await
        .unwrap();

        assert!(markdown.contains("````\n```shell\ncommand\n~~~\n````"));
        assert!(markdown.contains("~~old~~"));
        assert!(markdown.contains("\"quote\""));
        assert!(markdown.contains("*source*"));
        assert!(markdown.contains("H<sub>2</sub>O"));
        assert!(markdown.contains("<details open=\"\">"));
        assert!(markdown.contains("Embedded content: https://example.com/embed"));
    }

    #[tokio::test]
    async fn does_not_double_fence_preformatted_code() {
        let markdown =
            to_markdown("<pre><code class=\"language-rust\">fn main() {}</code></pre>".to_string())
                .await
                .unwrap();

        assert_eq!(markdown, "```rust\nfn main() {}\n```");
    }

    #[tokio::test]
    async fn rejects_backticks_in_code_language() {
        let markdown =
            to_markdown("<pre><code class=\"language-```\">x</code></pre><p>after</p>".to_string())
                .await
                .unwrap();

        assert_eq!(markdown, "```\nx\n```\n\nafter");
    }

    #[test]
    fn rejects_text_that_cannot_fit_the_requested_width() {
        let html = format!(
            "{}hello{}",
            "<blockquote>".repeat(80),
            "</blockquote>".repeat(80)
        );

        assert!(to_text(&html, 120).is_err());
    }

    #[tokio::test]
    async fn rejects_excessively_deep_markdown_input() {
        let html = format!("{}text{}", "<div>".repeat(140), "</div>".repeat(140));

        assert!(to_markdown(html).await.is_err());
    }

    #[tokio::test]
    async fn rejects_excessive_text_tags_before_rendering() {
        let html = "<br>".repeat(MAX_TEXT_TAGS + 1);

        assert!(to_text_async(html, 120).await.is_err());
    }
}
