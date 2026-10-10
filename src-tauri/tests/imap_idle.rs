//! 针对实时收信 IDLE 链路的集成测试。
//!
//! 运行（需要真实账号，参考 tests/netease_id.rs 的用法）：
//! ```bash
//! PONY_TEST_EMAIL=you@163.com \
//! PONY_TEST_PASS=授权码 \
//!   cargo test --test imap_idle -- --ignored --nocapture
//! ```
//!
//! 验证目标：能力探测 → 进入 IDLE → 等待结束（DONE 收尾）后，会话仍可继续
//! 收发命令；以及新邮件到达时服务器能立刻推送唤醒（实时收信的根基）。
//! 这是 vendor/imap `idle.rs` 补丁的回归点：
//! 上游等待超时把 DONE 收尾留给 Drop（读无超时，断网时会永久阻塞监听线程），
//! 且建立阶段遇到未预期的响应会 `unreachable!()` 直接崩溃（release panic=abort）。

use std::time::{Duration, Instant};

use pony_mail_lib::mail::{imap_client, smtp_client};
use pony_mail_lib::models::{Account, SendPayload};

fn account() -> Account {
    let email = std::env::var("PONY_TEST_EMAIL").expect("缺少 PONY_TEST_EMAIL");
    Account {
        id: "test".into(),
        email: email.clone(),
        display_name: "test".into(),
        imap_host: std::env::var("PONY_TEST_IMAP_HOST").unwrap_or_else(|_| "imap.163.com".into()),
        imap_port: 993,
        imap_tls: true,
        smtp_host: std::env::var("PONY_TEST_SMTP_HOST").unwrap_or_else(|_| "smtp.163.com".into()),
        smtp_port: std::env::var("PONY_TEST_SMTP_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(465),
        smtp_tls: true,
        username: email,
        created_at: String::new(),
    }
}

#[test]
#[ignore]
fn idle_round_trip_keeps_session_usable() {
    let password = std::env::var("PONY_TEST_PASS").expect("缺少 PONY_TEST_PASS");
    let acct = account();

    let mut session = imap_client::connect(&acct, &password).expect("登录失败");
    imap_client::select_folder(&mut session, "INBOX").expect("SELECT INBOX 失败");

    let idle_supported = session
        .capabilities()
        .map(|caps| caps.has_str("IDLE"))
        .unwrap_or(false);
    eprintln!("[1/4] 登录成功，服务器 IDLE 能力：{idle_supported}");
    if !idle_supported {
        eprintln!("服务器不支持 IDLE，跳过监听路径验证（运行时将退化为轮询）");
        imap_client::logout(session);
        return;
    }

    // 两轮 IDLE：第一轮验证进入与超时退出；第二轮验证 DONE 收尾后会话未被污染，
    // 还能再次进入 IDLE（监听线程正是这样循环的）。
    for round in 1..=2 {
        let handle = session.idle().expect("进入 IDLE 失败");
        let outcome = handle
            .wait_with_timeout(Duration::from_secs(4))
            .expect("IDLE 等待出错");
        eprintln!("[2/4] 第 {round} 轮 IDLE 正常结束：{outcome:?}");
    }

    // DONE 收尾之后会话必须仍可正常收发命令。
    let uids = imap_client::list_uids(&mut session, "INBOX").expect("IDLE 之后的 SEARCH 失败");
    eprintln!("[3/4] IDLE 后 SEARCH 成功，共 {} 封", uids.len());

    imap_client::logout(session);
    eprintln!("[4/4] 登出完成");
}

/// 端到端验证「实时收信」的核心承诺：IDLE 期间新邮件到达会立刻唤醒等待。
///
/// 做法：IDLE 期间从另一线程给自己发一封测试邮件，等待必须远早于超时返回，
/// 且唤醒后能在收件箱搜到新邮件。
#[test]
#[ignore]
fn idle_wakes_on_new_mail() {
    let password = std::env::var("PONY_TEST_PASS").expect("缺少 PONY_TEST_PASS");
    let acct = account();

    let mut session = imap_client::connect(&acct, &password).expect("登录失败");
    imap_client::select_folder(&mut session, "INBOX").expect("SELECT INBOX 失败");
    let idle_supported = session
        .capabilities()
        .map(|caps| caps.has_str("IDLE"))
        .unwrap_or(false);
    if !idle_supported {
        eprintln!("服务器不支持 IDLE，跳过（运行时将退化为轮询）");
        imap_client::logout(session);
        return;
    }

    let before = imap_client::list_uids(&mut session, "INBOX")
        .expect("SEARCH 失败")
        .len();

    // 3 秒后给自己发一封测试邮件（独立线程，避免阻塞主线程的 IDLE 读）。
    let send_acct = account();
    let send_pass = password.clone();
    let subject = format!("Pony idle probe {}", std::process::id());
    let send_subject = subject.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        let payload = SendPayload {
            account_id: send_acct.id.clone(),
            to: send_acct.email.clone(),
            cc: String::new(),
            subject: send_subject,
            body: "idle wakeup probe".to_string(),
            html: false,
        };
        match smtp_client::send(&send_acct, &send_pass, &payload) {
            Ok(()) => eprintln!("[send] 自寄测试邮件已发出：{subject}"),
            Err(e) => eprintln!("[send] 发送失败：{e}"),
        }
    });

    let timeout = Duration::from_secs(45);
    let started = Instant::now();
    let handle = session.idle().expect("进入 IDLE 失败");
    let outcome = handle.wait_with_timeout(timeout).expect("IDLE 等待出错");
    let waited = started.elapsed();
    eprintln!("[1/3] IDLE 结束：{outcome:?}，用时 {:.1}s", waited.as_secs_f32());

    let _ = sender.join();
    assert_eq!(
        outcome,
        imap::extensions::idle::WaitOutcome::MailboxChanged,
        "新邮件到达后 IDLE 未被唤醒（等到超时才返回）"
    );

    let after = imap_client::list_uids(&mut session, "INBOX").expect("唤醒后 SEARCH 失败");
    eprintln!("[2/3] 唤醒后收件箱 {} 封（此前 {before} 封）", after.len());
    assert!(
        after.len() > before,
        "IDLE 被唤醒但收件箱没有新邮件（邮件可能投递到了别的文件夹）"
    );

    imap_client::logout(session);
    eprintln!("[3/3] 登出完成，实时收信链路验证通过");
}
