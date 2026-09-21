//! 鉴权、来源校验与密钥脱敏。

use crate::web::*;

#[test]
fn cookie_parser_matches_an_exact_cookie_name() {
    let mut headers = HeaderMap::new();
    headers.insert(
        COOKIE,
        HeaderValue::from_static("other=1; miyu_session=secret-token; suffix=2"),
    );
    assert_eq!(cookie_value(&headers, AUTH_COOKIE), Some("secret-token"));
    assert_eq!(cookie_value(&headers, "session"), None);
}

#[test]
fn origin_check_accepts_absent_or_current_host_origin() {
    let mut headers = HeaderMap::new();
    assert!(origin_is_allowed(&headers));
    headers.insert(HOST, HeaderValue::from_static("192.168.1.20:4096"));
    headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:4096"));
    assert!(!origin_is_allowed(&headers));
    headers.insert(ORIGIN, HeaderValue::from_static("http://192.168.1.20:4096"));
    assert!(origin_is_allowed(&headers));
    headers.append(ORIGIN, HeaderValue::from_static("http://192.168.1.20:4096"));
    assert!(!origin_is_allowed(&headers));
}

#[test]
fn config_response_never_serializes_secret_values() {
    let mut config = AppConfig::default();
    config.providers[0].api_key = Some("provider-secret".to_string());
    config.plugins.web.tavily_api_keys = vec!["tavily-secret".to_string()];
    config.plugins.exchange_rate.api_key = "exchange-secret".to_string();
    config.plugins.image_generation.api_keys = vec!["image-secret".to_string()];
    let paths = tempfile::tempdir().unwrap();
    let paths = MiyuPaths {
        root_dir: paths.path().to_path_buf(),
        config_dir: paths.path().join("config"),
        config_file: paths.path().join("config/config.jsonc"),
        skills_dir: paths.path().join("config/skills"),
        data_dir: paths.path().join("data"),
        cache_dir: paths.path().join("cache"),
        state_dir: paths.path().join("state"),
        pictures_dir: paths.path().join("pictures"),
        fish_hook_file: paths.path().join("fish"),
        bash_hook_file: paths.path().join("bash"),
        zsh_hook_file: paths.path().join("zsh"),
        scripts_dir: paths.path().join("scripts"),
        system_scripts_dir: paths.path().join("system-scripts"),
    };
    let response = config_response(
        &config,
        ContextSnapshot {
            tokens: 0,
            window: None,
            window_assumed: false,
            cumulative_tokens: 0,
            cumulative_prompt_tokens: 0,
            cumulative_cache_read_tokens: 0,
        },
        &paths,
    )
    .unwrap();
    let serialized = serde_json::to_string(&response).unwrap();
    assert!(!serialized.contains("provider-secret"));
    assert!(!serialized.contains("tavily-secret"));
    assert!(!serialized.contains("exchange-secret"));
    assert!(!serialized.contains("image-secret"));
    assert_eq!(response.secret_states["providers.0.api_key"], true);
    assert_eq!(response.secret_states["plugins.web.tavily_api_keys"], true);
    assert!(response.config.get("memory").is_some());
}

#[test]
fn omitted_provider_secret_does_not_follow_array_position_after_rename() {
    let mut current = AppConfig::default();
    current.providers[0].id = "first".to_string();
    current.providers[0].api_key = Some("first-secret".to_string());
    let mut candidate = current.clone();
    candidate.providers[0].id = "renamed".to_string();
    candidate.providers[0].api_key = None;
    restore_config_secrets(&mut candidate, &current, &HashMap::new()).unwrap();
    assert_eq!(candidate.providers[0].api_key, None);
}

#[test]
fn explicit_secret_clear_removes_a_provider_key() {
    let mut current = AppConfig::default();
    current.providers[0].api_key = Some("secret".to_string());
    let mut candidate = current.clone();
    candidate.providers[0].api_key = None;
    let mutations = HashMap::from([("providers.0.api_key".to_string(), SecretMutation::Clear)]);
    restore_config_secrets(&mut candidate, &current, &mutations).unwrap();
    assert_eq!(candidate.providers[0].api_key, None);
}
