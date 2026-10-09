//! 智能整理写回语义的集成测试。
//!
//! 覆盖多对多组模型下「组内单选」的几条关键不变量：
//! 用户手动标签的保护（整组跳过 / 不降级）、受限替换（跨组共享标签不互踩）、
//! 新标签的复用与去重、`ai_tagged_at` 可重试标记、发件人统计只计已整理邮件。
//!
//! 运行：`cargo test --test ai_classify`

use pony_mail_lib::classify::Pick;
use pony_mail_lib::store::{GroupAssignment, Store};

/// 建一个含账号与两封同发件人邮件的临时库。
fn seeded_store() -> (Store, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let store = Store::new(dir.path().join("test.db")).expect("初始化 Store 失败");
    {
        let conn = store.raw_conn_for_test();
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, imap_host, imap_port, imap_tls, \
             smtp_host, smtp_port, smtp_tls, username, created_at) \
             VALUES ('acc', 'a@b.com', 'N', 'imap.b.com', 993, 1, 'smtp.b.com', 465, 1, 'a@b.com', '')",
            [],
        )
        .expect("插入账号失败");
        for (id, uid) in [(1i64, 101u32), (2, 102)] {
            conn.execute(
                "INSERT INTO messages (id, account_id, folder, uid, message_id, subject, \
                 from_name, from_addr, date_ts, seen, flagged, has_attachments, body_loaded) \
                 VALUES (?1, 'acc', 'INBOX', ?2, ?3, '测试', 'X', 'x@y.com', 0, 0, 0, 0, 0)",
                rusqlite::params![id, uid, format!("mid{id}")],
            )
            .expect("插入邮件失败");
        }
    }
    (store, dir)
}

fn make_tag(store: &Store, name: &str) -> i64 {
    store.create_tag("acc", name, "#6366f1", "").expect("建标签失败").id
}

fn make_group(store: &Store, name: &str, ai_mode: &str, tag_ids: &[i64]) -> i64 {
    let g = store.create_tag_group("acc", name, "#8b5cf6", "").expect("建组失败");
    store
        .update_tag_group(g.id, None, None, None, Some(ai_mode))
        .expect("设置 ai_mode 失败");
    if !tag_ids.is_empty() {
        store.set_group_tags(g.id, tag_ids, "").expect("设组员失败");
    }
    g.id
}

/// `(tag_id, source)` 行列表。
fn tag_rows(store: &Store, message_id: i64) -> Vec<(i64, String)> {
    let conn = store.raw_conn_for_test();
    let mut stmt = conn
        .prepare("SELECT tag_id, source FROM message_tags WHERE message_id = ?1 ORDER BY tag_id")
        .expect("准备查询失败");
    let rows = stmt
        .query_map([message_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("查询失败");
    rows.map(|r| r.expect("读取失败")).collect()
}

fn tagged_at(store: &Store, message_id: i64) -> Option<String> {
    let conn = store.raw_conn_for_test();
    conn.query_row(
        "SELECT ai_tagged_at FROM messages WHERE id = ?1",
        [message_id],
        |r| r.get(0),
    )
    .expect("查询标记失败")
}

fn assigns(pairs: &[(i64, Option<Pick>)]) -> Vec<GroupAssignment> {
    pairs
        .iter()
        .map(|(gid, pick)| GroupAssignment {
            group_id: *gid,
            pick: pick.clone(),
        })
        .collect()
}

#[test]
fn ai_tag_written_with_source_and_marker() {
    let (store, _dir) = seeded_store();
    let t1 = make_tag(&store, "Microsoft");
    let g1 = make_group(&store, "发件方", "select", &[t1]);

    let out = store
        .save_classify_result(
            "acc",
            1,
            &assigns(&[(g1, Some(Pick::Existing(t1)))]),
            "2026-10-09T22:00:00Z",
        )
        .expect("写回应成功");

    assert_eq!(out, vec![t1], "返回权威标签集合");
    assert_eq!(tag_rows(&store, 1), vec![(t1, "ai".to_string())]);
    assert!(tagged_at(&store, 1).is_some(), "成功后应写已整理标记");
}

/// 组内单选：同一组两次写回不同标签，只留后者；其他组的 AI 标签不受影响。
#[test]
fn group_single_select_replaces_previous_ai_tag() {
    let (store, _dir) = seeded_store();
    let ta = make_tag(&store, "Microsoft");
    let tb = make_tag(&store, "Steam");
    let t_other = make_tag(&store, "促销");
    let g1 = make_group(&store, "发件方", "select", &[ta, tb]);
    let g2 = make_group(&store, "内容", "select", &[t_other]);

    store
        .save_classify_result(
            "acc",
            1,
            &assigns(&[(g1, Some(Pick::Existing(ta))), (g2, Some(Pick::Existing(t_other)))]),
            "t1",
        )
        .unwrap();
    assert_eq!(tag_rows(&store, 1), vec![(ta, "ai".into()), (t_other, "ai".into())]);

    // 第二次：g1 改选 tb → ta 被替换，g2 不动。
    store
        .save_classify_result(
            "acc",
            1,
            &assigns(&[(g1, Some(Pick::Existing(tb))), (g2, Some(Pick::Existing(t_other)))]),
            "t2",
        )
        .unwrap();
    assert_eq!(tag_rows(&store, 1), vec![(tb, "ai".into()), (t_other, "ai".into())]);
}

/// 用户保护：组内有手动标签 → 整组跳过（不删不写）；其他组照常。
#[test]
fn manual_tag_protects_whole_group() {
    let (store, _dir) = seeded_store();
    let t_manual = make_tag(&store, "Microsoft");
    let t_ai = make_tag(&store, "Steam");
    let t_other = make_tag(&store, "促销");
    let g1 = make_group(&store, "发件方", "select", &[t_manual, t_ai]);
    let g2 = make_group(&store, "内容", "select", &[t_other]);

    // 用户手动把 Microsoft 挂到邮件 1。
    store.attach_tag(1, t_manual, "t0").unwrap();

    store
        .save_classify_result(
            "acc",
            1,
            &assigns(&[(g1, Some(Pick::Existing(t_ai))), (g2, Some(Pick::Existing(t_other)))]),
            "t1",
        )
        .unwrap();

    let rows = tag_rows(&store, 1);
    assert!(rows.contains(&(t_manual, "manual".to_string())), "手动标签应保留：{rows:?}");
    assert!(!rows.contains(&(t_ai, "ai".to_string())), "被保护的组不得写入 AI 标签：{rows:?}");
    assert!(rows.contains(&(t_other, "ai".to_string())), "未保护的组照常写：{rows:?}");
}

/// AI 选中的标签命中一条已存在的 manual 行 → 行保持 manual（不降级、不误删）。
///
/// 构造：Steam 被用户手动挂上（不在任何启用组内，不触发整组保护），
/// AI 在 create 组输出大小写变体 "steam" → 解析复用同一标签 → 冲突时 DO NOTHING。
#[test]
fn ai_reselect_keeps_manual_source() {
    let (store, _dir) = seeded_store();
    let steam = make_tag(&store, "Steam");
    store.attach_tag(1, steam, "t0").unwrap();

    let t_other = make_tag(&store, "促销");
    let g1 = make_group(&store, "内容", "select", &[t_other]);
    let g2 = make_group(&store, "发件方", "create", &[]);

    store
        .save_classify_result(
            "acc",
            1,
            &assigns(&[
                (g1, Some(Pick::Existing(t_other))),
                (g2, Some(Pick::New("steam".to_string()))),
            ]),
            "t1",
        )
        .unwrap();

    let rows = tag_rows(&store, 1);
    assert!(
        rows.contains(&(steam, "manual".to_string())),
        "命中的已有行必须保持 manual：{rows:?}"
    );
    // 且没有产生第二个 Steam 变体标签。
    let tags = store.list_tags("acc").unwrap();
    assert_eq!(tags.iter().filter(|t| t.name.eq_ignore_ascii_case("steam")).count(), 1);
}

/// 跨组共享标签：只要被任一启用组选中就保留；都不选才删除。
#[test]
fn tag_shared_by_two_active_groups_survives() {
    let (store, _dir) = seeded_store();
    let t_shared = make_tag(&store, "Steam");
    let t_a = make_tag(&store, "Microsoft");
    let g1 = make_group(&store, "发件方", "select", &[t_shared, t_a]);
    let g2 = make_group(&store, "购物", "select", &[t_shared]);

    // 仅 g2 选中 t_shared → t_shared 保留（删除阶段取全选中集合的并集）。
    store
        .save_classify_result(
            "acc",
            1,
            &assigns(&[(g1, None), (g2, Some(Pick::Existing(t_shared)))]),
            "t1",
        )
        .unwrap();
    assert_eq!(tag_rows(&store, 1), vec![(t_shared, "ai".into())]);

    // g1 改选 t_a、g2 仍选 t_shared → 两个都在（按 tag_id 升序，t_shared 先建 id 更小）。
    store
        .save_classify_result(
            "acc",
            1,
            &assigns(&[
                (g1, Some(Pick::Existing(t_a))),
                (g2, Some(Pick::Existing(t_shared))),
            ]),
            "t2",
        )
        .unwrap();
    assert_eq!(
        tag_rows(&store, 1),
        vec![(t_shared, "ai".into()), (t_a, "ai".into())]
    );

    // g2 不再选 t_shared → 它属于启用组且未被选中 → 删除。
    store
        .save_classify_result(
            "acc",
            1,
            &assigns(&[(g1, Some(Pick::Existing(t_a))), (g2, None)]),
            "t3",
        )
        .unwrap();
    assert_eq!(tag_rows(&store, 1), vec![(t_a, "ai".into())]);
}

/// create 模式：全新品牌 → 新建标签（created_by='ai'、自动入组、确定性配色）。
#[test]
fn create_new_tag_joins_group_with_ai_source() {
    let (store, _dir) = seeded_store();
    let g1 = make_group(&store, "发件方", "create", &[]);

    store
        .save_classify_result(
            "acc",
            1,
            &assigns(&[(g1, Some(Pick::New("GitHub".to_string())))]),
            "t1",
        )
        .unwrap();

    let tags = store.list_tags("acc").unwrap();
    let gh = tags.iter().find(|t| t.name == "GitHub").expect("应创建 GitHub");
    assert_eq!(gh.created_by, "ai");
    assert_eq!(gh.color, pony_mail_lib::classify::palette_color("GitHub"));
    assert!(store.group_tag_ids(g1).unwrap().contains(&gh.id), "新标签应自动入组");
    assert_eq!(tag_rows(&store, 1), vec![(gh.id, "ai".to_string())]);
}

/// 指派到一个已被删除的组 → 跳过该维度，不报错。
#[test]
fn missing_group_is_skipped() {
    let (store, _dir) = seeded_store();
    let t1 = make_tag(&store, "Microsoft");
    let g1 = make_group(&store, "发件方", "select", &[t1]);

    store
        .save_classify_result(
            "acc",
            1,
            &assigns(&[(999, Some(Pick::Existing(t1))), (g1, Some(Pick::Existing(t1)))]),
            "t1",
        )
        .expect("不存在的组应被静默跳过");
    assert_eq!(tag_rows(&store, 1), vec![(t1, "ai".to_string())]);
}

/// 全部维度不指派（低置信情景）→ 无标签写入，但仍写已整理标记。
#[test]
fn empty_assignments_still_marks_classified() {
    let (store, _dir) = seeded_store();
    let t1 = make_tag(&store, "促销");
    let g1 = make_group(&store, "内容", "select", &[t1]);

    store
        .save_classify_result("acc", 1, &assigns(&[(g1, None)]), "t1")
        .unwrap();
    assert!(tag_rows(&store, 1).is_empty());
    assert!(tagged_at(&store, 1).is_some(), "判定过就要写标记，避免无限重试");
}

/// 待整理队列与发件人统计：只计入已整理邮件。
#[test]
fn queue_and_sender_stats_only_count_classified() {
    let (store, _dir) = seeded_store();
    let t1 = make_tag(&store, "Microsoft");
    let g1 = make_group(&store, "发件方", "select", &[t1]);

    // 初始两封都未整理。
    assert_eq!(store.count_unclassified("acc").unwrap(), 2);
    let ids = store.unclassified_ids("acc", 10).unwrap();
    assert_eq!(ids, vec![2, 1], "新邮件优先（date_ts 平手时 id 降序）");

    // 整理邮件 1；发件人统计仍应为 0（只计已整理，且排除自身）。
    store
        .save_classify_result("acc", 1, &assigns(&[(g1, Some(Pick::Existing(t1)))]), "t1")
        .unwrap();
    assert_eq!(store.count_unclassified("acc").unwrap(), 1);
    assert_eq!(
        store.sender_classified_count("acc", "x@y.com", 1).unwrap(),
        0,
        "邮件 1 自身被排除，邮件 2 未整理"
    );

    // 再整理邮件 2 后，统计邮件 1 时能看到邮件 2 的历史。
    store
        .save_classify_result("acc", 2, &assigns(&[(g1, Some(Pick::Existing(t1)))]), "t2")
        .unwrap();
    assert_eq!(store.sender_classified_count("acc", "x@y.com", 1).unwrap(), 1);
    let stats = store.sender_group_stats("acc", "x@y.com", 1, g1).unwrap();
    assert_eq!(stats, vec![(t1, "Microsoft".to_string(), 1)]);
    assert_eq!(store.count_unclassified("acc").unwrap(), 0);
}

/// 维度组快照：只读取启用的组，未知模式视为不参与。
#[test]
fn ai_dimension_groups_reads_enabled_only() {
    let (store, _dir) = seeded_store();
    let t1 = make_tag(&store, "Microsoft");
    let g_on = make_group(&store, "发件方", "select", &[t1]);
    let _g_off = make_group(&store, "别的组", "off", &[t1]);

    let groups = store.ai_dimension_groups("acc").unwrap();
    assert_eq!(groups.len(), 1, "只应读出启用组");
    assert_eq!(groups[0].id, g_on);
    assert_eq!(groups[0].candidates, vec![(t1, "Microsoft".to_string())]);
}
