//! 托盘未读总数（`Store::unread_total`）的数据层集成测试。
//!
//! 托盘 tooltip 的「N 封未读」口径与列表未读语义一致：**全账号、仅 INBOX、
//! seen=0**。会改变该数值的写点有：新邮件落库（watcher / 手动同步）、
//! 标记已读（set_seen）、删除邮件、删除账号。这里验证数据层的账目正确，
//! 各写点对 `tray::refresh` 的调用属命令层编排，由手动验收覆盖。
//!
//! 运行：`cargo test --test tray_unread`

use pony_mail_lib::store::Store;

/// 建一个临时库：两个账号的 INBOX 若干邮件 + 一封非 INBOX 邮件，混合已读状态。
/// 前置账目：acc1 两封未读 + 一封已读；acc2 一封未读（另有一封未读在 Sent，不计）。
fn seeded_store() -> (Store, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let store = Store::new(dir.path().join("test.db")).expect("初始化 Store 失败");

    {
        let conn = store.raw_conn_for_test();
        for (acct, email) in [("acc1", "a@b.com"), ("acc2", "c@d.com")] {
            conn.execute(
                "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, imap_tls, \
                 smtp_host, smtp_port, smtp_tls, username, created_at) \
                 VALUES (?1, ?2, 'X', 'imap.b.com', 993, 1, 'smtp.b.com', 465, 1, ?2, '')",
                rusqlite::params![acct, email],
            )
            .expect("插入账号失败");
        }
        let rows: [(i64, &str, &str, u32, i64); 5] = [
            (1, "acc1", "INBOX", 101, 0),
            (2, "acc1", "INBOX", 102, 0),
            (3, "acc1", "INBOX", 103, 1),
            (4, "acc2", "INBOX", 201, 0),
            (5, "acc2", "Sent", 301, 0),
        ];
        for (id, acct, folder, uid, seen) in rows {
            conn.execute(
                "INSERT INTO messages (id, account_id, folder, uid, message_id, subject, from_name, \
                 from_addr, date_ts, seen, flagged, has_attachments, body_loaded) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 's', 'X', 'x@y.com', 0, ?6, 0, 0, 0)",
                rusqlite::params![id, acct, folder, uid, format!("mid{id}"), seen],
            )
            .expect("插入邮件失败");
        }
    }

    (store, dir)
}

#[test]
fn unread_total_counts_all_accounts_inbox_unseen_only() {
    let (store, _dir) = seeded_store();
    // acc1 两封未读 + acc2 一封未读；已读件与非 INBOX 文件夹都不计。
    assert_eq!(store.unread_total().unwrap(), 3);
}

#[test]
fn unread_total_tracks_mark_seen_both_directions() {
    let (store, _dir) = seeded_store();

    store.set_seen(1, true).unwrap();
    assert_eq!(store.unread_total().unwrap(), 2, "标记已读应减一");

    // 幂等：重复标记同一封不改账目。
    store.set_seen(1, true).unwrap();
    assert_eq!(store.unread_total().unwrap(), 2);

    // 反向：把已读标记回未读应加一（右键「标记为未读」的路径）。
    store.set_seen(3, false).unwrap();
    assert_eq!(store.unread_total().unwrap(), 3, "标回未读应加一");
}

#[test]
fn unread_total_drops_after_delete_unread_message() {
    let (store, _dir) = seeded_store();

    assert!(store.delete_message(2).unwrap(), "删一封未读应返回 true");
    assert_eq!(store.unread_total().unwrap(), 2, "删未读应减一");

    store.delete_message(3).unwrap();
    assert_eq!(store.unread_total().unwrap(), 2, "删已读不影响未读总数");
}

#[test]
fn unread_total_drops_after_delete_account() {
    let (store, _dir) = seeded_store();

    // acc2 名下一封 INBOX 未读（Sent 那封本就不计），级联删除后总数减一。
    store.delete_account("acc2").unwrap();
    assert_eq!(store.unread_total().unwrap(), 2);
}
