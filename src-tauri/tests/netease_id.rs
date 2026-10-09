//! 针对网易系邮箱 IMAP `Unsafe Login` 修复的集成测试。
//!
//! 运行（需要真实账号）：
//! ```bash
//! PONY_TEST_EMAIL=you@163.com \
//! PONY_TEST_PASS=授权码 \
//! PONY_IMAP_DEBUG=1 \
//!   cargo test --test netease_id -- --nocapture --ignored
//! ```
//!
//! 验证目标：`LOGIN` 后发送 RFC 2971 `ID`，再 `SELECT INBOX` 必须成功；
//! 且 `ID` 的响应不会污染后续命令（这是上游 imap 2.4.1 会 panic 的地方）。

use pony_mail_lib::mail::imap_client;
use pony_mail_lib::models::Account;

fn account() -> Account {
    let email = std::env::var("PONY_TEST_EMAIL").expect("缺少 PONY_TEST_EMAIL");
    Account {
        id: "test".into(),
        email: email.clone(),
        display_name: "test".into(),
        imap_host: std::env::var("PONY_TEST_IMAP_HOST").unwrap_or_else(|_| "imap.163.com".into()),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.163.com".into(),
        smtp_port: 465,
        smtp_tls: true,
        username: email,
        created_at: String::new(),
    }
}

#[test]
#[ignore]
fn netease_login_id_select_succeeds() {
    let password = std::env::var("PONY_TEST_PASS").expect("缺少 PONY_TEST_PASS");
    let acct = account();

    let mut session = imap_client::connect(&acct, &password).expect("登录失败（预期修复后应成功）");
    eprintln!("[1/3] IMAP 登录成功");

    // 关键：ID 的响应必须已被完整消费，下面这条 SELECT 不能 panic。
    let uids = imap_client::list_uids(&mut session, "INBOX")
        .expect("SELECT INBOX 失败（预期修复后应成功）");
    eprintln!("[2/3] SELECT INBOX 成功，共 {} 封邮件", uids.len());

    // 再发一条命令，确认连接未被 ID 的残留响应污染。
    let uids2 = imap_client::list_uids(&mut session, "INBOX").expect("第二次 SELECT 失败，连接已被污染");
    assert_eq!(uids.len(), uids2.len(), "两次 SELECT 结果数量不一致");
    eprintln!("[3/3] 第二次 SELECT 成功，连接状态干净");

    imap_client::logout(session);
}
