//! schema 版本化迁移机制的集成测试。
//!
//! 覆盖三类存量形态的采纳路径与失败回滚：
//! - 全新库：v1 基线 → v4 全量；
//! - 真实库形态（v3：有 `source` / `ai_tagged_at`，无 `ai_mode` / `created_by`，`user_version = 3`）——
//!   这两个列是一版已回退的实现曾实际落到本机库里的；
//! - 老库形态（v0：表齐全但无任何新列，`user_version = 0`）。
//!
//! 注意：一旦存在非幂等迁移（`ALTER ... ADD COLUMN`），「新结构 + 旧版本号」是
//! 真实世界到不了的状态（结构与版本号同事务），所以造老库不能只压版本号——
//! 必须同时把结构退回上一版（见 `revert_to_*_shape`），否则重放会撞 `duplicate column`。
//!
//! 运行：`cargo test --test migrations`

use pony_mail_lib::schema::{apply_list, SCHEMA_VERSION};
use pony_mail_lib::store::Store;
use rusqlite::Connection;

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("准备 table_info 失败");
    let mut rows = stmt.query([]).expect("查询 table_info 失败");
    while let Some(row) = rows.next().expect("读取 table_info 失败") {
        let name: String = row.get(1).expect("读取列名失败");
        if name == column {
            return true;
        }
    }
    false
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("读 user_version 失败")
}

/// 把库退回真实库当前的形态：v3（有 v2/v3 两列，无 v4 的两列与索引）。
fn revert_to_v3_shape(conn: &Connection) {
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_messages_ai_untagged;
         DROP INDEX IF EXISTS idx_messages_from_addr;
         ALTER TABLE tag_groups DROP COLUMN ai_mode;
         ALTER TABLE tags DROP COLUMN created_by;",
    )
    .expect("退回 v3 形态失败");
    conn.pragma_update(None, "user_version", 3)
        .expect("设置版本号失败");
}

/// 再退回 v0：连 v2/v3 的两列也删掉（迁移机制引入之前的形态）。
fn revert_to_v0_shape(conn: &Connection) {
    revert_to_v3_shape(conn);
    conn.execute_batch(
        "ALTER TABLE message_tags DROP COLUMN source;
         ALTER TABLE messages DROP COLUMN ai_tagged_at;",
    )
    .expect("退回 v0 形态失败");
    conn.pragma_update(None, "user_version", 0)
        .expect("设置版本号失败");
}

/// 造一个带数据的全量库（v4 形态）。数据用于验证迁移不动存量行。
fn seeded_db() -> tempfile::TempDir {
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
        conn.execute(
            "INSERT INTO messages (id, account_id, folder, uid, message_id, subject, from_name, \
             from_addr, date_ts, seen, flagged, has_attachments, body_loaded) \
             VALUES (1, 'acc', 'INBOX', 101, 'mid1', '测试', 'X', 'x@y.com', 0, 0, 0, 0, 0)",
            [],
        )
        .expect("插入邮件失败");
        conn.execute(
            "INSERT INTO tags (id, account_id, name, color, sort_order, created_at, created_by) \
             VALUES (1, 'acc', '促销', '#6366f1', 0, '', 'user')",
            [],
        )
        .expect("插入标签失败");
        conn.execute(
            "INSERT INTO tag_groups (id, account_id, name, color, sort_order, collapsed, \
             created_at, ai_mode) \
             VALUES (1, 'acc', '内容', '#8b5cf6', 0, 0, '', 'select')",
            [],
        )
        .expect("插入标签组失败");
        conn.execute(
            "INSERT INTO message_tags (message_id, tag_id, created_at, source) \
             VALUES (1, 1, '', 'manual')",
            [],
        )
        .expect("插入打标行失败");
    }
    dir
}

/// 全新库一次到 v4，四列齐全。
#[test]
fn fresh_db_reaches_latest_version() {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let store = Store::new(dir.path().join("fresh.db")).expect("初始化 Store 失败");
    let conn = store.raw_conn_for_test();

    assert_eq!(user_version(&conn), SCHEMA_VERSION);
    for (table, column) in [
        ("message_tags", "source"),
        ("messages", "ai_tagged_at"),
        ("tag_groups", "ai_mode"),
        ("tags", "created_by"),
    ] {
        assert!(column_exists(&conn, table, column), "{table}.{column} 应存在");
    }
}

/// 真实库形态（v3）首次运行新版：只补 v4，数据零变动，新列取默认值。
#[test]
fn realistic_v3_db_adopts_v4_cleanly() {
    let dir = seeded_db();
    {
        let conn = Connection::open(dir.path().join("test.db")).expect("打开库失败");
        revert_to_v3_shape(&conn);
        assert_eq!(user_version(&conn), 3);
    }

    let store = Store::new(dir.path().join("test.db")).expect("重开 Store 失败");
    let conn = store.raw_conn_for_test();
    assert_eq!(user_version(&conn), 4, "重开后应补齐 v4");
    assert!(column_exists(&conn, "tags", "created_by"));
    assert!(column_exists(&conn, "tag_groups", "ai_mode"));

    // 存量行零变动；新列取默认值（created_by='user'、ai_mode='off'）。
    let tags = store.list_tags("acc").expect("列出标签失败");
    assert_eq!(tags.len(), 1, "标签行数不应变化");
    assert_eq!(tags[0].name, "促销");
    assert_eq!(tags[0].created_by, "user", "存量标签应视为用户创建");
    let groups = store.list_tag_groups("acc").expect("列出组失败");
    assert_eq!(groups.len(), 1, "组行数不应变化");
    assert_eq!(groups[0].ai_mode, "off", "存量组默认不参与 AI 整理");

    // v2 的 source 列不受影响：打标行仍为 manual。
    let src: String = conn
        .query_row("SELECT source FROM message_tags WHERE message_id = 1", [], |r| r.get(0))
        .expect("读打标来源失败");
    assert_eq!(src, "manual");
}

/// 老库形态（v0）一次补齐 v1→v4，数据保留、新列默认值正确。
#[test]
fn v0_legacy_db_adopts_all_migrations() {
    let dir = seeded_db();
    {
        let conn = Connection::open(dir.path().join("test.db")).expect("打开库失败");
        revert_to_v0_shape(&conn);
        assert_eq!(user_version(&conn), 0);
        assert!(!column_exists(&conn, "message_tags", "source"));
    }

    let store = Store::new(dir.path().join("test.db")).expect("重开 Store 失败");
    let conn = store.raw_conn_for_test();
    assert_eq!(user_version(&conn), 4);
    assert!(column_exists(&conn, "message_tags", "source"));
    assert!(column_exists(&conn, "messages", "ai_tagged_at"));

    // 存量数据保留；重加的 source 列默认 'manual'。
    assert_eq!(store.list_tags("acc").expect("列出标签失败").len(), 1);
    let src: String = conn
        .query_row("SELECT source FROM message_tags WHERE message_id = 1", [], |r| r.get(0))
        .expect("读打标来源失败");
    assert_eq!(src, "manual");
}

/// 迁移中途失败：同事务整体回滚，版本不前进，下次可原样重试。
#[test]
fn migration_failure_rolls_back_and_keeps_version() {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let conn = Connection::open(dir.path().join("rollback.db")).expect("打开库失败");

    let migrations: &[(i64, &str)] = &[
        (1, "CREATE TABLE good (id INTEGER);"),
        (2, "CREATE TABLE good2 (id INTEGER); NOT VALID SQL;"),
    ];
    assert!(apply_list(&conn, migrations, 2).is_err(), "坏迁移应报错");
    assert_eq!(user_version(&conn), 1, "失败迁移的版本不得前进");

    let table_count = |name: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |r| r.get(0),
        )
        .expect("查询 sqlite_master 失败")
    };
    assert_eq!(table_count("good2"), 0, "失败迁移内先建的表也应一并回滚");
    assert_eq!(table_count("good"), 1, "此前成功的迁移保持不变");
}

/// 重复打开（迁移已全部应用）应是空操作，不报 duplicate column。
#[test]
fn reopen_is_noop() {
    let dir = seeded_db();
    for _ in 0..3 {
        let store = Store::new(dir.path().join("test.db")).expect("重开 Store 失败");
        assert_eq!(user_version(&store.raw_conn_for_test()), 4);
    }
}
