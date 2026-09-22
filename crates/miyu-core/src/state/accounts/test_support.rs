//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/accounts.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl StateStore {
    pub fn count_accounts(&self) -> Result<i64> {
        self.conv_db.count_accounts()
    }

    /// 引导:daemon 带着 `-p` 起来时,保证有一个管理员账号且密码就是它。
    /// 没有账号就建一个(用户名 = 家目录名,拿不到时 `admin`);已有管理员就
    /// 把密码重设成 `-p` 的值(它是属主这台机器上的机器级凭据,改了 `-p`
    /// 就该生效)。
    pub fn ensure_bootstrap_admin(&self, password: &str, username: &str) -> Result<Account> {
        let admins = self
            .conv_db
            .list_accounts()?
            .into_iter()
            .filter(|account| account.is_admin())
            .collect::<Vec<_>>();
        if let Some(admin) = admins.into_iter().next() {
            if !verify_password(&admin.password_hash, password) {
                self.conv_db
                    .set_account_password_hash(&admin.id, &hash_password(password))?;
            }
            return Ok(self
                .conv_db
                .account_by_id(&admin.id)?
                .expect("admin account exists"));
        }
        let username = if validate_username(username).is_ok() {
            username
        } else {
            BOOTSTRAP_ADMIN_USERNAME
        };
        self.create_account(username, "", password, ROLE_ADMIN)
    }
}
