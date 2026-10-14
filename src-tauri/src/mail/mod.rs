pub mod imap_client;
pub mod parser;
pub mod sanitize;
pub mod smtp_client;

use crate::models::Account;

/// 按服务商规范登录凭据。IMAP 登录与 SMTP 认证都必须走这里，保持一致。
///
/// Gmail 的「应用专用密码」在 Google 页面上以 `xxxx xxxx xxxx xxxx` 形式成组展示，
/// 用户复制粘贴时常带空格，直接送出会被服务器以 Invalid credentials 拒绝；
/// 而该密码本身不含空白字符，去掉全部空白是无损的。其他服务商原样透传，
/// 不假设其密码中不允许空格。
pub fn normalize_password(account: &Account, password: &str) -> String {
    if account.is_gmail() {
        password.chars().filter(|c| !c.is_whitespace()).collect()
    } else {
        password.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(imap_host: &str) -> Account {
        Account {
            id: "t".into(),
            email: "t@example.com".into(),
            display_name: String::new(),
            imap_host: imap_host.into(),
            imap_port: 993,
            imap_tls: true,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_tls: true,
            username: String::new(),
            created_at: String::new(),
        }
    }

    #[test]
    fn normalize_password_strips_whitespace_for_gmail() {
        let acct = account("imap.gmail.com");
        assert_eq!(
            normalize_password(&acct, "abcd efgh ijkl mnop"),
            "abcdefghijklmnop"
        );
        assert_eq!(normalize_password(&acct, "abcd\tefgh\nijkl mnop"), "abcdefghijklmnop");
        // 已不带空白的原样返回。
        assert_eq!(normalize_password(&acct, "abcdefghijklmnop"), "abcdefghijklmnop");
    }

    #[test]
    fn normalize_password_keeps_other_providers_untouched() {
        let acct = account("imap.163.com");
        assert_eq!(normalize_password(&acct, "pass word"), "pass word");
        assert_eq!(normalize_password(&acct, " pass "), " pass ");
    }
}
