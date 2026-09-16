use miyu_base::config::OneBotConfig;
use miyu_core::state::{
    PlatformAccessAuthorization, PlatformAccessGrantKey, StateStore, GLOBAL_PLATFORM_ACCOUNT_SCOPE,
};

pub(crate) const ONEBOT_PLATFORM: &str = "onebot";
pub(crate) const USER_SUBJECT: &str = "user";
pub(crate) const GROUP_SUBJECT: &str = "group";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccessPermission {
    Administrator,
    PrivateWhitelist,
    GroupWhitelist,
}

impl AccessPermission {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "administrator" => Some(Self::Administrator),
            "private_whitelist" => Some(Self::PrivateWhitelist),
            "group_whitelist" => Some(Self::GroupWhitelist),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Administrator => "administrator",
            Self::PrivateWhitelist => "private_whitelist",
            Self::GroupWhitelist => "group_whitelist",
        }
    }

    pub(crate) fn subject_kind(self) -> &'static str {
        match self {
            Self::Administrator | Self::PrivateWhitelist => USER_SUBJECT,
            Self::GroupWhitelist => GROUP_SUBJECT,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Administrator => "管理员",
            Self::PrivateWhitelist => "私聊白名单",
            Self::GroupWhitelist => "群聊白名单",
        }
    }

    pub(crate) fn statically_contains(self, config: &OneBotConfig, target_id: i64) -> bool {
        match self {
            Self::Administrator => config.admin_users.contains(&target_id),
            Self::PrivateWhitelist => config.private_chats.whitelist.contains(&target_id),
            Self::GroupWhitelist => config.group_chats.whitelist.contains(&target_id),
        }
    }
}

pub(crate) fn has_dynamic_access(
    state: &StateStore,
    account_id: &str,
    permission: AccessPermission,
    target_id: &str,
) -> bool {
    state.has_platform_access_grant(
        ONEBOT_PLATFORM,
        account_id,
        permission.as_str(),
        permission.subject_kind(),
        target_id,
    )
}

pub(crate) fn is_effective_admin(
    config: &OneBotConfig,
    state: &StateStore,
    account_id: &str,
    user_id: &str,
) -> bool {
    user_id.parse::<i64>().ok().is_some_and(|numeric_user_id| {
        config.admin_users.contains(&numeric_user_id)
            || has_dynamic_access(state, account_id, AccessPermission::Administrator, user_id)
    })
}

pub(crate) fn administrator_authorization(
    config: &OneBotConfig,
    account_id: &str,
    user_id: &str,
) -> PlatformAccessAuthorization {
    PlatformAccessAuthorization {
        statically_authorized: user_id
            .parse::<i64>()
            .ok()
            .is_some_and(|user_id| config.admin_users.contains(&user_id)),
        dynamic_key: PlatformAccessGrantKey {
            platform: ONEBOT_PLATFORM.to_string(),
            account_scope: account_id.to_string(),
            permission: AccessPermission::Administrator.as_str().to_string(),
            subject_kind: AccessPermission::Administrator.subject_kind().to_string(),
            subject_id: user_id.to_string(),
        },
    }
}

pub(crate) fn global_grant_key(
    permission: AccessPermission,
    target_id: impl Into<String>,
) -> miyu_core::state::PlatformAccessGrantKey {
    miyu_core::state::PlatformAccessGrantKey {
        platform: ONEBOT_PLATFORM.to_string(),
        account_scope: GLOBAL_PLATFORM_ACCOUNT_SCOPE.to_string(),
        permission: permission.as_str().to_string(),
        subject_kind: permission.subject_kind().to_string(),
        subject_id: target_id.into(),
    }
}
