//! Gmail 「删除 = 移入回收站」语义的集成测试（真实账号）。
//!
//! 运行（需要开启两步验证的 Gmail 账号 + 应用专用密码）：
//! ```bash
//! PONY_TEST_EMAIL=you@gmail.com \
//! PONY_TEST_PASS="abcd efgh ijkl mnop" \
//!   cargo test --test gmail_trash -- --ignored --nocapture
//! ```
//! 应用专用密码可带空格（粘贴形态），走 `imap_client::connect` 会自动去掉空白。
//!
//! 验证目标（对应 `imap_client::delete_or_trash` 的 Gmail 分支）：
//!   1. 自寄一封测试邮件，确认到达 INBOX；
//!   2. `delete_or_trash` 后邮件从 INBOX 消失、出现在 \Trash 回收站（可恢复）；
//!   3. 在回收站中再次删除 → 永久删除（复用用户点「删除」的同一路径）。
//!
//! 为什么必须用真实服务器验证：回收站文件夹名随账号语言本地化，
//! 只能靠 SPECIAL-USE 属性（RFC 6154）定位，这是纯逻辑单测覆盖不到的协议行为。

use pony_mail_lib::mail::{imap_client, smtp_client};
use pony_mail_lib::models::{Account, SendPayload};
use std::time::{Duration, Instant};

fn account() -> Account {
    let email = std::env::var("PONY_TEST_EMAIL").expect("缺少 PONY_TEST_EMAIL");
    Account {
        id: "test".into(),
        email: email.clone(),
        display_name: "test".into(),
        imap_host: "imap.gmail.com".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.gmail.com".into(),
        smtp_port: 465,
        smtp_tls: true,
        username: email,
        created_at: String::new(),
    }
}

/// 在指定文件夹里按主题搜索，返回命中的 UID 列表。
fn search_subject(session: &mut imap_client::ImapSession, subject: &str) -> Vec<u32> {
    let mut hits: Vec<u32> = session
        .uid_search(format!("SUBJECT \"{subject}\""))
        .expect("SEARCH 失败")
        .into_iter()
        .collect();
    hits.sort_unstable();
    hits
}

#[test]
#[ignore]
fn gmail_delete_moves_to_trash_and_trash_delete_is_permanent() {
    let password = std::env::var("PONY_TEST_PASS").expect("缺少 PONY_TEST_PASS");
    let acct = account();
    assert!(acct.is_gmail(), "本测试只针对 Gmail 主机");

    let subject = format!(
        "Pony gmail trash probe {}",
        chrono::Utc::now().timestamp_millis()
    );

    // ---- 1) 自寄测试邮件并等它到达 INBOX ----
    let payload = SendPayload {
        account_id: acct.id.clone(),
        to: acct.email.clone(),
        cc: String::new(),
        subject: subject.clone(),
        body: "gmail trash probe".to_string(),
        html: false,
    };
    smtp_client::send(&acct, &password, &payload).expect("自寄测试邮件发送失败");
    eprintln!("[1/5] 测试邮件已发出：{subject}");

    let mut session = imap_client::connect(&acct, &password).expect("IMAP 登录失败");
    imap_client::select_folder(&mut session, "INBOX").expect("SELECT INBOX 失败");

    let deadline = Instant::now() + Duration::from_secs(90);
    let inbox_uid = loop {
        let hits = search_subject(&mut session, &subject);
        if let Some(&uid) = hits.first() {
            break uid;
        }
        assert!(
            Instant::now() < deadline,
            "90 秒内测试邮件未出现在 INBOX（检查账号过滤规则或投递延迟）"
        );
        std::thread::sleep(Duration::from_secs(3));
    };
    eprintln!("[2/5] 测试邮件已在 INBOX，UID = {inbox_uid}");

    // ---- 2) delete_or_trash：应从 INBOX 消失并进入回收站 ----
    imap_client::delete_or_trash(&mut session, &acct, "INBOX", &[inbox_uid]).expect("移入回收站失败");

    let inbox_left = search_subject(&mut session, &subject);
    assert!(
        inbox_left.is_empty(),
        "从 INBOX 删除后仍能搜到测试邮件：{inbox_left:?}"
    );
    eprintln!("[3/5] 已从 INBOX 移除");

    let trash = imap_client::find_trash_folder(&mut session)
        .expect("LIST 失败")
        .expect("未找到 \\Trash 回收站文件夹");
    eprintln!("[4/5] 回收站文件夹：{trash}");

    imap_client::select_folder(&mut session, &trash).expect("SELECT 回收站失败");
    let in_trash = search_subject(&mut session, &subject);
    assert!(
        !in_trash.is_empty(),
        "回收站中搜不到刚删除的测试邮件（删除没有走「移入回收站」语义）"
    );

    // ---- 3) 在回收站内再删 = 永久删除（与用户路径一致），顺手清理测试邮件 ----
    imap_client::delete_or_trash(&mut session, &acct, &trash, &in_trash).expect("永久删除失败");
    let trash_left = search_subject(&mut session, &subject);
    assert!(
        trash_left.is_empty(),
        "回收站内删除后仍能搜到测试邮件：{trash_left:?}"
    );

    imap_client::logout(session);
    eprintln!("[5/5] 回收站内永久删除验证通过，实时删除语义测试全部通过");
}
