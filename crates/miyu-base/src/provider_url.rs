//! Pure URL normalization shared by provider discovery paths.

pub(crate) fn models_url(base_url: &str) -> String {
    let mut url = base_url.trim().trim_end_matches('/').to_string();
    if url.ends_with("/chat/completions") {
        url.truncate(url.len() - "/chat/completions".len());
    }
    if url.ends_with("/v1") {
        format!("{url}/models")
    } else {
        format!("{url}/v1/models")
    }
}

#[cfg(test)]
mod tests {
    use super::models_url;

    #[test]
    fn normalizes_openai_compatible_endpoints() {
        assert_eq!(
            models_url("https://example.test/v1"),
            "https://example.test/v1/models"
        );
        assert_eq!(
            models_url("https://example.test"),
            "https://example.test/v1/models"
        );
    }
}
