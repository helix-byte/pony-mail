//! 标签系统的数据层集成测试。
//!
//! 覆盖 `commands` 里标签相关 command 依赖的不变量：
//! 账号隔离、名称唯一、打标幂等、删除级联、按标签筛选下推到 SQL。
//!
//! 运行：`cargo test --test tags`

use pony_mail_lib::store::Store;

/// 建一个临时库，两个账号各一封邮件，用于验证标签的账号隔离。
fn seeded_store() -> (Store, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let store = Store::new(dir.path().join("test.db")).expect("初始化 Store 失败");

    {
        let conn = store.raw_conn_for_test();
        for (acc, email) in [("acc", "a@b.com"), ("acc2", "c@d.com")] {
            conn.execute(
                "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, imap_tls, \
                 smtp_host, smtp_port, smtp_tls, username, created_at) \
                 VALUES (?1, ?2, 'N', 'imap.b.com', 993, 1, 'smtp.b.com', 465, 1, ?2, '')",
                rusqlite::params![acc, email],
            )
            .expect("插入账号失败");
        }
        conn.execute(
            "INSERT INTO folders (account_id, name, messages) VALUES ('acc', 'INBOX', 0)",
            [],
        )
        .expect("插入文件夹失败");
        for (id, acc, uid, subject) in [
            (1i64, "acc", 101u32, "第一封"),
            (2, "acc", 102, "第二封"),
            (3, "acc2", 201, "别的账号"),
        ] {
            conn.execute(
                "INSERT INTO messages (id, account_id, folder, uid, message_id, subject, \
                 from_name, from_addr, date_ts, seen, flagged, has_attachments, body_loaded) \
                 VALUES (?1, ?2, 'INBOX', ?3, ?4, ?5, 'X', 'x@y.com', 0, 0, 0, 0, 0)",
                rusqlite::params![id, acc, uid, format!("mid{id}"), subject],
            )
            .expect("插入邮件失败");
        }
    }

    (store, dir)
}

#[test]
fn create_tag_assigns_incrementing_order() {
    let (store, _dir) = seeded_store();

    let a = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    let b = store.create_tag("acc", "账单", "#f97316", "").unwrap();

    assert_eq!(a.sort_order, 0, "首个标签应排 0");
    assert_eq!(b.sort_order, 1, "后续标签应递增");

    let all = store.list_tags("acc").unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].name, "工作", "应按 sort_order 升序返回");
    assert!(all.iter().all(|t| t.message_count == 0), "新标签计数应为 0");
}

#[test]
fn create_tag_trims_name_and_rejects_blank() {
    let (store, _dir) = seeded_store();

    let t = store.create_tag("acc", "  工作  ", "#6366f1", "").unwrap();
    assert_eq!(t.name, "工作", "名称应去掉首尾空白");

    assert!(store.create_tag("acc", "   ", "#6366f1", "").is_err(), "空名应被拒绝");
}

#[test]
fn create_tag_rejects_duplicate_name_within_account() {
    let (store, _dir) = seeded_store();
    store.create_tag("acc", "工作", "#6366f1", "").unwrap();

    let err = store.create_tag("acc", "工作", "#f97316", "").unwrap_err();
    assert!(err.to_string().contains("已存在"), "错误文案应可读：{err}");
}

#[test]
fn tag_names_are_scoped_per_account() {
    let (store, _dir) = seeded_store();

    // 两个账号可以用同名标签，互不冲突。
    store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    store.create_tag("acc2", "工作", "#6366f1", "").unwrap();

    assert_eq!(store.list_tags("acc").unwrap().len(), 1);
    assert_eq!(store.list_tags("acc2").unwrap().len(), 1);
}

#[test]
fn attach_and_detach_are_idempotent() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();

    store.attach_tag(1, tag.id, "").unwrap();
    store.attach_tag(1, tag.id, "").unwrap(); // 重复打标不应报错
    assert_eq!(store.tags_of_message(1).unwrap(), vec![tag.id]);

    store.detach_tag(1, tag.id).unwrap();
    store.detach_tag(1, tag.id).unwrap(); // 重复撤销不应报错
    assert!(store.tags_of_message(1).unwrap().is_empty());
}

#[test]
fn list_tags_reports_message_count() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    let other = store.create_tag("acc", "账单", "#f97316", "").unwrap();

    store.attach_tag(1, tag.id, "").unwrap();
    store.attach_tag(2, tag.id, "").unwrap();
    store.attach_tag(1, other.id, "").unwrap();

    let all = store.list_tags("acc").unwrap();
    let work = all.iter().find(|t| t.id == tag.id).unwrap();
    let bill = all.iter().find(|t| t.id == other.id).unwrap();
    assert_eq!(work.message_count, 2);
    assert_eq!(bill.message_count, 1);
}

#[test]
fn batch_attach_reports_actual_changes() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();

    assert_eq!(store.attach_tag_to_messages(&[1, 2], tag.id, "").unwrap(), 2);
    // 再打一次同样的两封：都已在标签下，实际改动为 0。
    assert_eq!(store.attach_tag_to_messages(&[1, 2], tag.id, "").unwrap(), 0);
    assert_eq!(store.attach_tag_to_messages(&[], tag.id, "").unwrap(), 0);

    assert_eq!(store.detach_tag_from_messages(&[1, 2], tag.id).unwrap(), 2);
}

#[test]
fn list_messages_filtered_by_tag_pushes_filter_into_sql() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    store.attach_tag(1, tag.id, "").unwrap();

    let filtered = store
        .list_messages_filtered("acc", "INBOX", 200, 0, Some(tag.id))
        .unwrap();
    assert_eq!(filtered.len(), 1, "只应返回打过该标签的邮件");
    assert_eq!(filtered[0].id, 1);

    let all = store.list_messages_filtered("acc", "INBOX", 200, 0, None).unwrap();
    assert_eq!(all.len(), 2, "不筛选时返回全部");
}

#[test]
fn list_messages_embeds_tag_ids() {
    let (store, _dir) = seeded_store();
    let a = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    let b = store.create_tag("acc", "账单", "#f97316", "").unwrap();
    store.attach_tag(1, a.id, "").unwrap();
    store.attach_tag(1, b.id, "").unwrap();
    store.attach_tag(2, a.id, "").unwrap();

    let list = store.list_messages("acc", "INBOX", 200, 0).unwrap();
    let m1 = list.iter().find(|m| m.id == 1).unwrap();
    let m2 = list.iter().find(|m| m.id == 2).unwrap();
    assert_eq!(m1.tag_ids, vec![a.id, b.id], "一封邮件可带多个标签");
    assert_eq!(m2.tag_ids, vec![a.id]);
}

#[test]
fn deleting_tag_removes_attachments_but_keeps_messages() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    store.attach_tag(1, tag.id, "").unwrap();

    store.delete_tag(tag.id).unwrap();

    assert!(store.list_tags("acc").unwrap().is_empty(), "标签应被删除");
    assert!(store.tags_of_message(1).unwrap().is_empty(), "打标关系应级联清理");
    assert_eq!(store.count_messages("acc", "INBOX").unwrap(), 2, "邮件本身不应被删");
}

#[test]
fn deleting_message_removes_its_tag_links() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    store.attach_tag(1, tag.id, "").unwrap();
    store.attach_tag(2, tag.id, "").unwrap();

    store.delete_message(1).unwrap();

    assert!(store.tags_of_message(1).unwrap().is_empty(), "邮件删除后不应留孤儿打标行");
    // 标签计数随之下降，否则侧栏角标会虚高。
    let after = store.list_tags("acc").unwrap();
    assert_eq!(after[0].message_count, 1, "计数应同步为 1");
}

#[test]
fn deleting_account_removes_tags_and_links() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    let other = store.create_tag("acc2", "工作", "#6366f1", "").unwrap();
    store.attach_tag(1, tag.id, "").unwrap();
    store.attach_tag(3, other.id, "").unwrap();

    store.delete_account("acc").unwrap();

    assert!(store.list_tags("acc").unwrap().is_empty(), "账号的标签应被清理");
    assert!(store.tags_of_message(1).unwrap().is_empty(), "打标关系应被清理");
    // 另一个账号的标签不受影响。
    assert_eq!(store.list_tags("acc2").unwrap().len(), 1);
    assert_eq!(store.tags_of_message(3).unwrap(), vec![other.id]);
}

#[test]
fn update_tag_renames_and_recolors() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();

    store.update_tag(tag.id, Some("项目"), Some("#14b8a6")).unwrap();
    let all = store.list_tags("acc").unwrap();
    assert_eq!(all[0].name, "项目");
    assert_eq!(all[0].color, "#14b8a6");
}

#[test]
fn update_tag_rejects_conflicting_name() {
    let (store, _dir) = seeded_store();
    let a = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    let b = store.create_tag("acc", "账单", "#f97316", "").unwrap();

    let err = store.update_tag(b.id, Some("工作"), None).unwrap_err();
    assert!(err.to_string().contains("已存在"), "重名应被拒绝：{err}");

    // 改成自己原来的名字不算冲突。
    assert!(store.update_tag(a.id, Some("工作"), None).is_ok());
}

#[test]
fn update_tag_rejects_blank_name() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    assert!(store.update_tag(tag.id, Some("   "), None).is_err(), "空名应被拒绝");
}

#[test]
fn update_missing_tag_reports_readable_error() {
    let (store, _dir) = seeded_store();
    // 不带 name 时直接改色会命中 0 行；带 name 时先查出账号再判定冲突。
    let err = store.update_tag(999, Some("任意"), None).unwrap_err();
    assert!(err.to_string().contains("标签不存在"), "错误应可读：{err}");
}
