//! 标签组功能的数据层集成测试。
//!
//! 重点覆盖多对多关系的特有陷阱：
//! 同一标签属于多个组、组内邮件**去重**计数、删组的解绑语义。
//!
//! 运行：`cargo test --test tag_groups`

use pony_mail_lib::store::Store;

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
        for (id, uid) in [(1i64, 101u32), (2, 102), (3, 103)] {
            conn.execute(
                "INSERT INTO messages (id, account_id, folder, uid, message_id, subject, from_name, \
                 from_addr, date_ts, seen, flagged, has_attachments, body_loaded) \
                 VALUES (?1, 'acc', 'INBOX', ?2, ?3, 'S', 'X', 'x@y.com', 0, 0, 0, 0, 0)",
                rusqlite::params![id, uid, format!("mid{id}")],
            )
            .expect("插入邮件失败");
        }
    }

    (store, dir)
}

#[test]
fn create_group_assigns_incrementing_order() {
    let (store, _dir) = seeded_store();
    let a = store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();
    let b = store.create_tag_group("acc", "客户", "#a855f7", "").unwrap();

    assert_eq!(a.sort_order, 0);
    assert_eq!(b.sort_order, 1);
    assert!(!a.collapsed, "新组默认展开");

    let all = store.list_tag_groups("acc").unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].name, "本周");
}

#[test]
fn create_group_rejects_duplicate_and_blank_name() {
    let (store, _dir) = seeded_store();
    store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();

    let err = store.create_tag_group("acc", "本周", "#000000", "").unwrap_err();
    assert!(err.to_string().contains("已存在"), "重名应被拒绝：{err}");
    assert!(store.create_tag_group("acc", "  ", "#000000", "").is_err(), "空名应被拒绝");
}

#[test]
fn group_names_are_scoped_per_account() {
    let (store, _dir) = seeded_store();
    let conn = store.raw_conn_for_test();
    conn.execute(
        "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, imap_tls, \
         smtp_host, smtp_port, smtp_tls, username, created_at) \
         VALUES ('acc2', 'c@d.com', 'C', 'imap.d.com', 993, 1, 'smtp.d.com', 465, 1, 'c@d.com', '')",
        [],
    )
    .unwrap();
    drop(conn);

    store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();
    store.create_tag_group("acc2", "本周", "#8b5cf6", "").unwrap();
    assert_eq!(store.list_tag_groups("acc").unwrap().len(), 1);
    assert_eq!(store.list_tag_groups("acc2").unwrap().len(), 1);
}

#[test]
fn a_tag_can_belong_to_multiple_groups() {
    // 多对多的核心断言：同一个标签出现在两个组里，两边都应能查到它。
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    let g1 = store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();
    let g2 = store.create_tag_group("acc", "客户", "#a855f7", "").unwrap();

    store.attach_tag_to_group(g1.id, tag.id, "").unwrap();
    store.attach_tag_to_group(g2.id, tag.id, "").unwrap();

    let groups = store.list_tag_groups("acc").unwrap();
    let a = groups.iter().find(|g| g.id == g1.id).unwrap();
    let b = groups.iter().find(|g| g.id == g2.id).unwrap();
    assert_eq!(a.tag_ids, vec![tag.id], "组 1 应含该标签");
    assert_eq!(b.tag_ids, vec![tag.id], "组 2 也应含该标签");

    let memberships = store.groups_of_tag(tag.id).unwrap();
    assert_eq!(memberships, vec![g1.id, g2.id], "反查应得到两个组");
}

#[test]
fn attach_to_group_is_idempotent() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    let g = store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();

    store.attach_tag_to_group(g.id, tag.id, "").unwrap();
    store.attach_tag_to_group(g.id, tag.id, "").unwrap(); // 重复添加不应报错
    assert_eq!(store.group_tag_ids(g.id).unwrap(), vec![tag.id]);

    store.detach_tag_from_group(g.id, tag.id).unwrap();
    store.detach_tag_from_group(g.id, tag.id).unwrap();
    assert!(store.group_tag_ids(g.id).unwrap().is_empty());
}

#[test]
fn set_group_tags_overwrites_membership() {
    let (store, _dir) = seeded_store();
    let a = store.create_tag("acc", "甲", "#6366f1", "").unwrap();
    let b = store.create_tag("acc", "乙", "#f97316", "").unwrap();
    let c = store.create_tag("acc", "丙", "#14b8a6", "").unwrap();
    let g = store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();

    store.set_group_tags(g.id, &[a.id, b.id], "").unwrap();
    assert_eq!(store.group_tag_ids(g.id).unwrap(), vec![a.id, b.id]);

    // 覆盖式写入：换成 b + c，a 应被摘掉。
    store.set_group_tags(g.id, &[b.id, c.id], "").unwrap();
    assert_eq!(store.group_tag_ids(g.id).unwrap(), vec![b.id, c.id]);

    // 设为空清空成员。
    store.set_group_tags(g.id, &[], "").unwrap();
    assert!(store.group_tag_ids(g.id).unwrap().is_empty());
}

#[test]
fn group_message_count_is_deduplicated() {
    // 关键断言：一封邮件同时带组内两个标签时，只能计一次。
    let (store, _dir) = seeded_store();
    let a = store.create_tag("acc", "甲", "#6366f1", "").unwrap();
    let b = store.create_tag("acc", "乙", "#f97316", "").unwrap();
    let g = store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();
    store.set_group_tags(g.id, &[a.id, b.id], "").unwrap();

    // 邮件 1 同时打 a 和 b；邮件 2 只打 a。
    store.attach_tag(1, a.id, "").unwrap();
    store.attach_tag(1, b.id, "").unwrap();
    store.attach_tag(2, a.id, "").unwrap();

    let groups = store.list_tag_groups("acc").unwrap();
    let grp = groups.iter().find(|x| x.id == g.id).unwrap();
    assert_eq!(grp.message_count, 2, "去重后应为 2（邮件 1 只算一次）");
    assert_eq!(grp.raw_message_count, 3, "未去重的原始和为 3");

    // 与直接调用计数方法保持一致。
    assert_eq!(store.group_message_count(g.id).unwrap(), 2);
}

#[test]
fn resolve_group_tags_returns_union() {
    let (store, _dir) = seeded_store();
    let a = store.create_tag("acc", "甲", "#6366f1", "").unwrap();
    let b = store.create_tag("acc", "乙", "#f97316", "").unwrap();
    let c = store.create_tag("acc", "丙", "#14b8a6", "").unwrap();
    let g1 = store.create_tag_group("acc", "组一", "#8b5cf6", "").unwrap();
    let g2 = store.create_tag_group("acc", "组二", "#a855f7", "").unwrap();

    store.set_group_tags(g1.id, &[a.id, b.id], "").unwrap();
    store.set_group_tags(g2.id, &[b.id, c.id], "").unwrap();

    // 两组取并集，b 是重叠项，去重后只出现一次。
    let union = store.groups_tag_ids(&[g1.id, g2.id]).unwrap();
    assert_eq!(union, vec![a.id, b.id, c.id], "应为去重后的并集");

    // 单组解析等价于该组成员。
    assert_eq!(store.groups_tag_ids(&[g1.id]).unwrap(), vec![a.id, b.id]);
    assert!(store.groups_tag_ids(&[]).unwrap().is_empty());
}

#[test]
fn list_messages_multi_uses_or_semantics() {
    let (store, _dir) = seeded_store();
    let a = store.create_tag("acc", "甲", "#6366f1", "").unwrap();
    let b = store.create_tag("acc", "乙", "#f97316", "").unwrap();
    store.attach_tag(1, a.id, "").unwrap();
    store.attach_tag(2, b.id, "").unwrap();
    // 邮件 3 不打任何标签。

    let both = store
        .list_messages_multi("acc", "INBOX", 200, 0, &[a.id, b.id])
        .unwrap();
    assert_eq!(both.len(), 2, "OR 语义：命中任一标签即入选");

    let only_a = store
        .list_messages_multi("acc", "INBOX", 200, 0, &[a.id])
        .unwrap();
    assert_eq!(only_a.len(), 1);
    assert_eq!(only_a[0].id, 1);

    let none = store.list_messages_multi("acc", "INBOX", 200, 0, &[]).unwrap();
    assert_eq!(none.len(), 3, "空集合表示不筛选，返回全部");

    // 单个标签的旧接口等价于单元素多选。
    let legacy = store
        .list_messages_filtered("acc", "INBOX", 200, 0, Some(a.id))
        .unwrap();
    assert_eq!(legacy.len(), 1);
}

#[test]
fn deleting_group_unbinds_tags_but_keeps_them() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    let g = store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();
    store.set_group_tags(g.id, &[tag.id], "").unwrap();
    store.attach_tag(1, tag.id, "").unwrap();

    store.delete_tag_group(g.id).unwrap();

    assert!(store.list_tag_groups("acc").unwrap().is_empty(), "组应被删除");
    assert_eq!(store.list_tags("acc").unwrap().len(), 1, "标签本身必须保留");
    assert_eq!(store.tags_of_message(1).unwrap(), vec![tag.id], "打标关系不受影响");
}

#[test]
fn deleting_tag_unbinds_it_from_groups_but_keeps_groups() {
    let (store, _dir) = seeded_store();
    let a = store.create_tag("acc", "甲", "#6366f1", "").unwrap();
    let b = store.create_tag("acc", "乙", "#f97316", "").unwrap();
    let g = store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();
    store.set_group_tags(g.id, &[a.id, b.id], "").unwrap();

    store.delete_tag(a.id).unwrap();

    let groups = store.list_tag_groups("acc").unwrap();
    assert_eq!(groups.len(), 1, "组应保留");
    assert_eq!(groups[0].tag_ids, vec![b.id], "被删标签应从组内摘掉");
}

#[test]
fn update_group_renames_recolors_and_toggles_collapse() {
    let (store, _dir) = seeded_store();
    let g = store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();

    store.update_tag_group(g.id, Some("本月"), Some("#0d9488"), Some(true)).unwrap();
    let all = store.list_tag_groups("acc").unwrap();
    assert_eq!(all[0].name, "本月");
    assert_eq!(all[0].color, "#0d9488");
    assert!(all[0].collapsed, "折叠状态应持久化");

    store.update_tag_group(g.id, None, None, Some(false)).unwrap();
    assert!(!store.list_tag_groups("acc").unwrap()[0].collapsed);
}

#[test]
fn update_group_rejects_conflicting_name() {
    let (store, _dir) = seeded_store();
    let a = store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();
    let b = store.create_tag_group("acc", "客户", "#a855f7", "").unwrap();

    let err = store.update_tag_group(b.id, Some("本周"), None, None).unwrap_err();
    assert!(err.to_string().contains("已存在"), "重名应被拒绝：{err}");
    // 改成自己原来的名字不算冲突。
    assert!(store.update_tag_group(a.id, Some("本周"), None, None).is_ok());
}

#[test]
fn update_missing_group_reports_readable_error() {
    let (store, _dir) = seeded_store();
    let err = store.update_tag_group(999, Some("任意"), None, None).unwrap_err();
    assert!(err.to_string().contains("标签组不存在"), "错误应可读：{err}");
}

#[test]
fn deleting_account_clears_groups_and_memberships() {
    let (store, _dir) = seeded_store();
    let tag = store.create_tag("acc", "工作", "#6366f1", "").unwrap();
    let g = store.create_tag_group("acc", "本周", "#8b5cf6", "").unwrap();
    store.set_group_tags(g.id, &[tag.id], "").unwrap();

    store.delete_account("acc").unwrap();

    assert!(store.list_tag_groups("acc").unwrap().is_empty(), "账号的组应被清理");
    assert!(store.list_tags("acc").unwrap().is_empty(), "账号的标签应被清理");
    let left: i64 = store
        .raw_conn_for_test()
        .query_row("SELECT COUNT(*) FROM tag_group_members", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0, "组员关系不应留孤儿行");
}

#[test]
fn empty_group_has_zero_count_not_error() {
    let (store, _dir) = seeded_store();
    let g = store.create_tag_group("acc", "空组", "#8b5cf6", "").unwrap();

    let all = store.list_tag_groups("acc").unwrap();
    let grp = all.iter().find(|x| x.id == g.id).unwrap();
    assert!(grp.tag_ids.is_empty());
    assert_eq!(grp.message_count, 0);
    assert_eq!(grp.raw_message_count, 0);
    assert_eq!(store.group_message_count(g.id).unwrap(), 0);
}
