//! API 配额账号的管理。
//!
//! 账号 ID 与显示名都要自动生成不重复的（`next_api_quota_account_id` /
//! `_name`），因为用户通常只想「再加一个」，不想给每个账号起名字。

use crate::config_tui::*;

pub(in crate::config_tui) fn edit_api_quota(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let options = [
            format!(
                "DeepSeek ({})",
                configured_count(&config.plugins.api_quota.deepseek)
            ),
            format!(
                "OpenRouter ({})",
                configured_count(&config.plugins.api_quota.openrouter)
            ),
        ];
        draw_menu(
            stdout,
            t(" LLM API QUOTA ", " 大模型额度查询 "),
            &options,
            selected,
            t("[Enter]configure [q]back", "[Enter]配置 [q]返回"),
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(1),
            KeyCode::Enter | KeyCode::Char('i') => {
                if selected == 0 {
                    edit_api_quota_accounts(
                        stdout,
                        "DeepSeek",
                        &mut config.plugins.api_quota.deepseek,
                    )?;
                } else {
                    edit_api_quota_accounts(
                        stdout,
                        "OpenRouter",
                        &mut config.plugins.api_quota.openrouter,
                    )?;
                }
            }
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn edit_api_quota_accounts(
    stdout: &mut io::Stdout,
    name: &str,
    config: &mut ApiQuotaProviderConfig,
) -> Result<()> {
    if config.accounts.is_empty() {
        config.accounts.push(ApiQuotaAccountConfig {
            id: "account-1".to_string(),
            name: "默认账号".to_string(),
            api_key: std::mem::take(&mut config.api_key),
        });
    }
    let mut selected = 0usize;
    loop {
        let mut options = config
            .accounts
            .iter()
            .map(|account| {
                format!(
                    "{} ({})",
                    account.name,
                    if account.api_key.trim().is_empty() {
                        t("not configured", "未配置")
                    } else {
                        t("configured", "已配置")
                    }
                )
            })
            .collect::<Vec<_>>();
        options.push(t("New account", "新建账号").to_string());
        selected = selected.min(options.len().saturating_sub(1));
        draw_menu(
            stdout,
            &format!(" {name} "),
            &options,
            selected,
            if name == "DeepSeek" {
                t(
                    "[Enter]edit [n]new [d]delete",
                    "[Enter]编辑 [n]新建 [d]删除",
                )
            } else {
                t(
                    "[Enter]edit [n]new [d]delete [q]back",
                    "[Enter]编辑 [n]新建 [d]删除 [q]返回",
                )
            },
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(options.len().saturating_sub(1))
            }
            KeyCode::Char('n') => {
                if config.accounts.len() < 32 && add_api_quota_account(stdout, config, name)? {
                    selected = config.accounts.len().saturating_sub(1);
                }
            }
            KeyCode::Char('d') if selected < config.accounts.len() => {
                if confirm_api_quota_delete(stdout, &config.accounts[selected].name)? {
                    if config.accounts.len() == 1 {
                        config.accounts[0].name = "默认账号".to_string();
                        config.accounts[0].api_key.clear();
                    } else {
                        config.accounts.remove(selected);
                        selected = selected.min(config.accounts.len() - 1);
                    }
                }
            }
            KeyCode::Enter | KeyCode::Char('i') if selected < config.accounts.len() => {
                let _ = edit_api_quota_account(stdout, name, &mut config.accounts[selected])?;
            }
            KeyCode::Enter | KeyCode::Char('i') => {
                if config.accounts.len() < 32 && add_api_quota_account(stdout, config, name)? {
                    selected = config.accounts.len().saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn confirm_api_quota_delete(
    stdout: &mut io::Stdout,
    account: &str,
) -> Result<bool> {
    let options = [
        t("Cancel", "取消").to_string(),
        format!("{}: {account}", t("Delete", "删除")),
    ];
    let mut selected = 0usize;
    loop {
        draw_menu(
            stdout,
            t(" DELETE ACCOUNT ", " 删除账号 "),
            &options,
            selected,
            t("[Enter]confirm [q]cancel", "[Enter]确认 [q]取消"),
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(false),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(1),
            KeyCode::Enter => return Ok(selected == 1),
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn edit_api_quota_account(
    stdout: &mut io::Stdout,
    provider: &str,
    account: &mut ApiQuotaAccountConfig,
) -> Result<bool> {
    let mut fields = vec![
        Field::new(t("Account name", "账号名称"), account.name.clone()),
        Field::new("API Key", account.api_key.clone()).sensitive(),
    ];
    if run_form(stdout, &format!(" {provider} "), &mut fields)? {
        account.name = fields[0].value.trim().to_string();
        if account.name.is_empty() {
            account.name = "默认账号".to_string();
        }
        account.api_key = fields[1].value.trim().to_string();
        return Ok(true);
    }
    Ok(false)
}

pub(in crate::config_tui) fn add_api_quota_account(
    stdout: &mut io::Stdout,
    config: &mut ApiQuotaProviderConfig,
    provider: &str,
) -> Result<bool> {
    let name = next_api_quota_account_name(config);
    let id = next_api_quota_account_id(config);
    config.accounts.push(ApiQuotaAccountConfig {
        id,
        name,
        api_key: String::new(),
    });
    let index = config.accounts.len() - 1;
    if edit_api_quota_account(stdout, provider, &mut config.accounts[index])? {
        Ok(true)
    } else {
        config.accounts.pop();
        Ok(false)
    }
}

pub(in crate::config_tui) fn next_api_quota_account_id(_config: &ApiQuotaProviderConfig) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("account-{nanos}-{sequence}")
}

pub(in crate::config_tui) fn next_api_quota_account_name(
    config: &ApiQuotaProviderConfig,
) -> String {
    let mut number = 2usize;
    loop {
        let candidate = format!("账号 {number}");
        if config
            .accounts
            .iter()
            .all(|account| account.name != candidate)
        {
            return candidate;
        }
        number += 1;
    }
}

pub(in crate::config_tui) fn configured_count(config: &ApiQuotaProviderConfig) -> String {
    let count = if config.accounts.is_empty() {
        usize::from(!config.api_key.trim().is_empty())
    } else {
        config
            .accounts
            .iter()
            .filter(|account| !account.api_key.trim().is_empty())
            .count()
    };
    if is_zh() {
        format!("{count} 个已配置")
    } else {
        format!("{count} configured")
    }
}
