use keyring::Entry;

use crate::error::AppResult;

/// 服务名，写入 Windows 凭据管理器时使用。
const SERVICE: &str = "pony-mail";

fn entry(account_id: &str) -> AppResult<Entry> {
    Ok(Entry::new(SERVICE, account_id)?)
}

/// 将账号密码写入系统凭据管理器（Windows Credential Manager）。
pub fn save_password(account_id: &str, password: &str) -> AppResult<()> {
    entry(account_id)?.set_password(password)?;
    Ok(())
}

/// 读取账号密码。账号未保存密码时返回 Err。
pub fn get_password(account_id: &str) -> AppResult<String> {
    Ok(entry(account_id)?.get_password()?)
}

/// 删除账号密码。凭据不存在时视为成功（幂等）。
pub fn delete_password(account_id: &str) -> AppResult<()> {
    match entry(account_id)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
