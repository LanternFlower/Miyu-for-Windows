//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/sessions.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl StateStore {
    pub fn platform_access_grants(&self, platform: &str) -> Result<Vec<PlatformAccessGrant>> {
        let _mutation = self.platform_access.mutations.lock().unwrap();
        self.conv_db.platform_access_grants(Some(platform))
    }

    pub fn add_platform_access_grant(
        &self,
        key: &PlatformAccessGrantKey,
        actor: &PlatformAccessActor,
    ) -> Result<bool> {
        let _mutation = self.platform_access.mutations.lock().unwrap();
        let inserted = self.conv_db.add_platform_access_grant(key, actor)?;
        if inserted {
            self.platform_access.index.write().unwrap().insert(key);
        }
        Ok(inserted)
    }

    pub fn remove_platform_access_grant(
        &self,
        key: &PlatformAccessGrantKey,
        actor: &PlatformAccessActor,
    ) -> Result<bool> {
        let _mutation = self.platform_access.mutations.lock().unwrap();
        let was_cached = self.platform_access.index.write().unwrap().remove(key);
        match self.conv_db.remove_platform_access_grant(key, actor) {
            Ok(deleted) => Ok(deleted),
            Err(error) => {
                if was_cached {
                    self.platform_access.index.write().unwrap().insert(key);
                }
                Err(error)
            }
        }
    }

    pub fn find_session_by_name(&self, persona: &str, name: &str) -> Result<Option<SessionRecord>> {
        self.conv_db.find_session_by_name(persona, name)
    }

    pub fn bind_platform_session(
        &self,
        key: &PlatformSessionBindingKey,
        session_id: &str,
    ) -> Result<()> {
        self.conv_db.bind_platform_session(key, session_id)
    }

    pub fn unbind_platform_session(&self, key: &PlatformSessionBindingKey) -> Result<bool> {
        self.conv_db.unbind_platform_session(key)
    }

    pub fn plugin_delete_scope(&self, scope: &PlatformPluginScopeKey) -> Result<usize> {
        self.conv_db.plugin_delete_scope(scope)
    }
}
