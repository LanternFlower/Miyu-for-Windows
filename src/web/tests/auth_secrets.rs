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
    config.plugins.api_quota.deepseek.api_key = "deepseek-secret".to_string();
    config.plugins.api_quota.openrouter.api_key = "openrouter-secret".to_string();
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
    assert!(!serialized.contains("deepseek-secret"));
    assert!(!serialized.contains("openrouter-secret"));
    assert_eq!(response.secret_states["providers.0.api_key"], true);
    assert_eq!(response.secret_states["plugins.web.tavily_api_keys"], true);
    assert_eq!(
        response.secret_states["plugins.api_quota.deepseek.accounts.0.api_key"],
        true
    );
    assert_eq!(
        response.secret_states["plugins.api_quota.openrouter.accounts.0.api_key"],
        true
    );
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

#[test]
fn api_quota_secrets_are_preserved_set_and_cleared() {
    let mut current = AppConfig::default();
    current.plugins.api_quota.deepseek.api_key = "deepseek-old".to_string();
    current.plugins.api_quota.openrouter.api_key = "openrouter-old".to_string();
    let mut candidate = current.clone();
    candidate.plugins.api_quota.deepseek.accounts = vec![crate::config::ApiQuotaAccountConfig {
        id: "account-1".to_string(),
        name: "默认账号".to_string(),
        api_key: String::new(),
    }];
    candidate.plugins.api_quota.openrouter.accounts = vec![crate::config::ApiQuotaAccountConfig {
        id: "account-1".to_string(),
        name: "默认账号".to_string(),
        api_key: String::new(),
    }];
    candidate.plugins.api_quota.deepseek.api_key.clear();
    candidate.plugins.api_quota.openrouter.api_key.clear();

    restore_config_secrets(&mut candidate, &current, &HashMap::new()).unwrap();
    assert_eq!(
        candidate.plugins.api_quota.deepseek.accounts[0].api_key,
        "deepseek-old"
    );
    assert_eq!(
        candidate.plugins.api_quota.openrouter.accounts[0].api_key,
        "openrouter-old"
    );

    let mutations = HashMap::from([
        (
            "plugins.api_quota.deepseek.accounts.0.api_key".to_string(),
            SecretMutation::Set("deepseek-new".to_string()),
        ),
        (
            "plugins.api_quota.openrouter.accounts.0.api_key".to_string(),
            SecretMutation::Clear,
        ),
    ]);
    restore_config_secrets(&mut candidate, &current, &mutations).unwrap();
    assert_eq!(
        candidate.plugins.api_quota.deepseek.accounts[0].api_key,
        "deepseek-new"
    );
    assert!(candidate.plugins.api_quota.openrouter.accounts[0]
        .api_key
        .is_empty());
}

#[test]
fn api_quota_account_ids_prevent_deleted_key_reuse() {
    let mut current = AppConfig::default();
    current.plugins.api_quota.deepseek.accounts[0] = crate::config::ApiQuotaAccountConfig {
        id: "old-id".to_string(),
        name: "账号 2".to_string(),
        api_key: "old-secret".to_string(),
    };
    let mut candidate = current.clone();
    candidate.plugins.api_quota.deepseek.accounts[0] = crate::config::ApiQuotaAccountConfig {
        id: "new-id".to_string(),
        name: "账号 2".to_string(),
        api_key: String::new(),
    };

    restore_config_secrets(&mut candidate, &current, &HashMap::new()).unwrap();
    assert!(candidate.plugins.api_quota.deepseek.accounts[0]
        .api_key
        .is_empty());

    candidate.plugins.api_quota.deepseek.accounts[0].id = "old-id".to_string();
    candidate.plugins.api_quota.deepseek.accounts[0].name = "重命名账号".to_string();
    restore_config_secrets(&mut candidate, &current, &HashMap::new()).unwrap();
    assert_eq!(
        candidate.plugins.api_quota.deepseek.accounts[0].api_key,
        "old-secret"
    );
}
