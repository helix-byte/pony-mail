//! 删除邮件功能的数据层集成测试。
//!
//! 直接驱动 `Store` 的删除原语，验证 `commands::delete_message` 所依赖的
//! 四条不变量：邮件行被删除、摘要缓存被级联清理、文件夹计数被同步、
//! 打标关系随邮件清理（标签角标是实时聚合，漏清理会虚高）。
//!
//! 运行：`cargo test --test delete_message`

use pony_mail_lib::store::Store;

/// 建一个临时库，塞入一封邮件 + 对应的摘要缓存。
fn seeded_store() -> (Store, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let store = Store::new(dir.path().join("test.db")).expect("初始化 Store 失败");

    {
        let conn = store.raw_conn_for_test();
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, imap_tls, \
             smtp_host, smtp_port, smtp_tls, username, created_at) \
             VALUES ('acc', 'a@b.com', 'A', 'imap.b.com', 993, 1, 'smtp.b.com', 465, 1, 'a@b.com', '')",
            [],
        )
        .expect("插入账号失败");
        conn.execute(
            "INSERT INTO folders (account_id, name, messages) VALUES ('acc', 'INBOX', 0)",
            [],
        )
        .expect("插入文件夹失败");
        for (id, uid, subject) in [(1i64, 101u32, "第一封"), (2, 102, "第二封")] {
            conn.execute(
                "INSERT INTO messages (id, account_id, folder, uid, message_id, subject, from_name, \
                 from_addr, date_ts, seen, flagged, has_attachments, body_loaded) \
                 VALUES (?1, 'acc', 'INBOX', ?2, ?3, ?4, 'X', 'x@y.com', 0, 0, 0, 0, 0)",
                rusqlite::params![id, uid, format!("mid{id}"), subject],
            )
            .expect("插入邮件失败");
            // 每封邮件配一个摘要缓存，键格式与 commands 中一致：summary:{id}:{mid}:{model}
            conn.execute(
                "INSERT INTO ai_cache (cache_key, kind, model, content, created_at) \
                 VALUES (?1, 'summary', 'm', '摘要', '')",
                rusqlite::params![format!("summary:{id}:mid{id}:m")],
            )
            .expect("插入缓存失败");
        }
    }

    (store, dir)
}

#[test]
fn delete_message_removes_row_and_summary_cache() {
    let (store, _dir) = seeded_store();

    assert_eq!(store.count_messages("acc", "INBOX").unwrap(), 2);
    let cache_before = store
        .raw_conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM ai_cache WHERE cache_key LIKE 'summary:1:%'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(cache_before, 1, "前置条件：邮件 1 应有一条摘要缓存");

    let removed = store.delete_message(1).unwrap();
    assert!(removed, "删除应返回 true");

    // 邮件行没了
    assert_eq!(store.count_messages("acc", "INBOX").unwrap(), 1);
    // 缓存被级联清理，且没有误伤别的邮件
    let cache_after = store
        .raw_conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM ai_cache WHERE cache_key LIKE 'summary:1:%'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(cache_after, 0, "摘要缓存应随邮件一起删除");
    let other_cache = store
        .raw_conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM ai_cache WHERE cache_key LIKE 'summary:2:%'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(other_cache, 1, "不应误删其他邮件的缓存");
}

#[test]
fn delete_message_on_missing_id_returns_false() {
    let (store, _dir) = seeded_store();
    assert!(!store.delete_message(999).unwrap(), "删不存在的邮件应返回 false");
}

#[test]
fn delete_message_clears_tag_links_and_decrements_counts() {
    let (store, _dir) = seeded_store();

    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    store.attach_tag(1, tag.id, "").unwrap();
    store.attach_tag(2, tag.id, "").unwrap();
    assert_eq!(
        store.list_tags("acc").unwrap()[0].message_count,
        2,
        "前置条件：两封邮件都应挂在标签下"
    );

    store.delete_message(1).unwrap();

    // 打标关系随邮件清理，标签计数（实时聚合）随之下降。
    let links_left = store
        .raw_conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM message_tags WHERE message_id = 1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(links_left, 0, "打标关系应随邮件一起删除");
    assert_eq!(
        store.list_tags("acc").unwrap()[0].message_count,
        1,
        "标签计数应随删除下降"
    );

    // 删掉最后一封挂着该标签的邮件 → 计数必须归零（不得留下虚幻角标）。
    store.delete_message(2).unwrap();
    assert_eq!(
        store.list_tags("acc").unwrap()[0].message_count,
        0,
        "标签下没有邮件时计数应为 0"
    );
}

#[test]
fn refresh_folder_count_syncs_drifted_counter() {
    let (store, _dir) = seeded_store();

    // 模拟真实库里的漂移：folders.messages 是 0，但实际有 2 封。
    let before = store
        .raw_conn_for_test()
        .query_row("SELECT messages FROM folders WHERE name='INBOX'", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(before, 0, "前置条件：计数应为漂移状态");

    let count = store.refresh_folder_count("acc", "INBOX").unwrap();
    assert_eq!(count, 2, "应按实际邮件数回填");

    let after = store
        .raw_conn_for_test()
        .query_row("SELECT messages FROM folders WHERE name='INBOX'", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(after, 2, "folders.messages 应被同步");

    // 删一封后再次同步，计数应降为 1。
    store.delete_message(1).unwrap();
    assert_eq!(store.refresh_folder_count("acc", "INBOX").unwrap(), 1);
}

#[test]
fn delete_messages_batch_removes_all_and_reports_count() {
    let (store, _dir) = seeded_store();

    // 两封都打上标签，批量删除后打标关系与标签计数都要归零。
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    store.attach_tag(1, tag.id, "").unwrap();
    store.attach_tag(2, tag.id, "").unwrap();

    let removed = store.delete_messages(&[1, 2]).unwrap();
    assert_eq!(removed, 2, "批量删除应返回实际删除行数");
    assert_eq!(store.count_messages("acc", "INBOX").unwrap(), 0);

    let cache_left = store
        .raw_conn_for_test()
        .query_row("SELECT COUNT(*) FROM ai_cache", [], |r| r.get::<_, i64>(0))
        .unwrap();
    assert_eq!(cache_left, 0, "两封邮件的摘要缓存都应被清理");

    assert_eq!(
        store.list_tags("acc").unwrap()[0].message_count,
        0,
        "批量删除后标签计数应归零"
    );
}

#[test]
fn message_route_survives_until_delete() {
    let (store, _dir) = seeded_store();

    // 删除前能拿到路由（commands 依赖它决定去哪个服务器文件夹删）。
    let (account_id, folder, uid) = store.message_route(1).unwrap();
    assert_eq!(account_id, "acc");
    assert_eq!(folder, "INBOX");
    assert_eq!(uid, 101);

    store.delete_message(1).unwrap();
    // 删除后路由不可得，且报错信息明确（而非 panic）。
    let err = store.message_route(1).unwrap_err();
    assert!(err.to_string().contains("邮件不存在"), "错误信息应可读：{err}");
}
