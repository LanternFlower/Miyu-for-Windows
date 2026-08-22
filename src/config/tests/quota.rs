//! API 配额账号。

use crate::config::*;

#[test]
fn api_quota_legacy_key_migrates_to_a_stable_default_account() {
    let mut config = AppConfig::default();
    config.plugins.api_quota.deepseek.accounts.clear();
    config.plugins.api_quota.deepseek.api_key = "legacy-key".to_string();
    config.normalize_api_quota_accounts();
    assert!(config.plugins.api_quota.deepseek.api_key.is_empty());
    assert_eq!(config.plugins.api_quota.deepseek.accounts.len(), 1);
    assert_eq!(
        config.plugins.api_quota.deepseek.accounts[0].id,
        "account-1"
    );
    assert_eq!(
        config.plugins.api_quota.deepseek.accounts[0].api_key,
        "legacy-key"
    );
}

#[test]
fn api_quota_mixed_config_preserves_both_keys() {
    let mut config = ApiQuotaProviderConfig::default();
    config.accounts[0].api_key = "new-key".to_string();
    config.api_key = "legacy-key".to_string();
    normalize_api_quota_provider(&mut config);
    assert!(config.api_key.is_empty());
    assert_eq!(config.accounts.len(), 2);
    assert_eq!(config.accounts[0].api_key, "new-key");
    assert_eq!(config.accounts[1].api_key, "legacy-key");
    assert_ne!(config.accounts[0].id, config.accounts[1].id);
}

#[test]
fn api_quota_account_names_must_be_unique() {
    let mut config = AppConfig::default();
    config.plugins.api_quota.deepseek.accounts = vec![
        ApiQuotaAccountConfig {
            id: "first".to_string(),
            name: "账号".to_string(),
            api_key: "first".to_string(),
        },
        ApiQuotaAccountConfig {
            id: "second".to_string(),
            name: "账号".to_string(),
            api_key: "second".to_string(),
        },
    ];
    assert!(config.validate().is_err());
}
