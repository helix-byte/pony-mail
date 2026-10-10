//! 基于 `PRAGMA user_version` 的 schema 版本化迁移。
//!
//! 背景：最初的建表方式是一整块 `CREATE TABLE IF NOT EXISTS`，它只能建缺失的表、
//! **加不了列**——存量库上改建表语句会被静默跳过，直到查询报 `no such column`
//! 才暴露。因此引入版本化迁移：新变更一律追加更高版本，`Store::new` 时自动补齐。
//!
//! 三条铁律：
//! 1. **已发布的迁移不可再改**——改了也不会在存量库上重跑，只会让新旧库形态分叉；
//!    实要变更就追加更高版本。
//! 2. 每个迁移与其 `user_version` 写入**同一个事务**，失败整体回滚、版本不前进。
//! 3. 新增非幂等迁移（如 `ALTER ... ADD COLUMN`）时，任何「建新库再把版本号压回旧值」
//!    的测试夹具都会撞 `duplicate column`——必须同时把结构退回上一版
//!    （见 `tests/migrations.rs` 的 `revert_to_*_shape`）。

use rusqlite::Connection;

use crate::error::AppResult;

/// 当前 schema 目标版本。必须等于 `MIGRATIONS` 中最后一个版本号（有测试盯住）。
pub const SCHEMA_VERSION: i64 = 4;

/// v1 基线：建表全集。清一色 `IF NOT EXISTS`，幂等——
/// 存量库「表齐全但 user_version = 0」时执行它是空操作，只补版本号、不动数据。
const V1_BASELINE: &str = r#"
CREATE TABLE IF NOT EXISTS accounts (
    id            TEXT PRIMARY KEY,
    email         TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL DEFAULT '',
    imap_host     TEXT NOT NULL,
    imap_port     INTEGER NOT NULL,
    imap_tls      INTEGER NOT NULL DEFAULT 1,
    smtp_host     TEXT NOT NULL,
    smtp_port     INTEGER NOT NULL,
    smtp_tls      INTEGER NOT NULL DEFAULT 1,
    username      TEXT NOT NULL,
    created_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS folders (
    account_id  TEXT NOT NULL,
    name        TEXT NOT NULL,
    messages    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, name)
);

CREATE TABLE IF NOT EXISTS messages (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id       TEXT NOT NULL,
    folder           TEXT NOT NULL,
    uid              INTEGER NOT NULL,
    message_id       TEXT NOT NULL DEFAULT '',
    subject          TEXT NOT NULL DEFAULT '',
    from_name        TEXT NOT NULL DEFAULT '',
    from_addr        TEXT NOT NULL DEFAULT '',
    to_addrs         TEXT NOT NULL DEFAULT '',
    cc_addrs         TEXT NOT NULL DEFAULT '',
    date_ts          INTEGER NOT NULL DEFAULT 0,
    seen             INTEGER NOT NULL DEFAULT 0,
    flagged          INTEGER NOT NULL DEFAULT 0,
    has_attachments  INTEGER NOT NULL DEFAULT 0,
    body_loaded      INTEGER NOT NULL DEFAULT 0,
    snippet          TEXT NOT NULL DEFAULT '',
    body_text        TEXT NOT NULL DEFAULT '',
    body_html        TEXT NOT NULL DEFAULT '',
    attachments      TEXT NOT NULL DEFAULT '[]',
    synced_at        TEXT NOT NULL DEFAULT '',
    UNIQUE (account_id, folder, uid)
);

CREATE INDEX IF NOT EXISTS idx_messages_list
    ON messages (account_id, folder, date_ts DESC);

CREATE TABLE IF NOT EXISTS sync_state (
    account_id   TEXT NOT NULL,
    folder       TEXT NOT NULL,
    last_uid     INTEGER NOT NULL DEFAULT 0,
    last_sync_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, folder)
);

CREATE TABLE IF NOT EXISTS ai_cache (
    cache_key   TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    model       TEXT NOT NULL DEFAULT '',
    content     TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tags (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  TEXT NOT NULL,
    name        TEXT NOT NULL,
    color       TEXT NOT NULL DEFAULT '#6366f1',
    sort_order  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT '',
    UNIQUE (account_id, name)
);

CREATE TABLE IF NOT EXISTS message_tags (
    message_id  INTEGER NOT NULL,
    tag_id      INTEGER NOT NULL,
    created_at  TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (message_id, tag_id)
);

-- 按标签筛邮件时走这个索引；反过来查「某封邮件的标签」走主键前缀。
CREATE INDEX IF NOT EXISTS idx_message_tags_tag ON message_tags (tag_id, message_id);

-- 标签组：可点击筛选的标签合集，组内标签是 OR 关系。
CREATE TABLE IF NOT EXISTS tag_groups (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  TEXT NOT NULL,
    name        TEXT NOT NULL,
    color       TEXT NOT NULL DEFAULT '#8b5cf6',
    sort_order  INTEGER NOT NULL DEFAULT 0,
    collapsed   INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT '',
    UNIQUE (account_id, name)
);

-- 组与标签**多对多**：同一标签可同时归入多个组（如「工作」既在「本周」也在「客户」组）。
CREATE TABLE IF NOT EXISTS tag_group_members (
    group_id    INTEGER NOT NULL,
    tag_id      INTEGER NOT NULL,
    created_at  TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (group_id, tag_id)
);

-- 按组查其成员标签（渲染侧栏折叠区）时走这个索引。
CREATE INDEX IF NOT EXISTS idx_tag_group_members_tag ON tag_group_members (tag_id, group_id);
"#;

/// v2：给打标关系加来源标记（'manual' / 'ai'）。
/// 注意：这条迁移曾随一版已回退的实现落到本机真实库（user_version = 3），
/// **不可修改**——存量库不会重跑它，改了只会让新旧库形态分叉。
const V2_MESSAGE_TAG_SOURCE: &str =
    "ALTER TABLE message_tags ADD COLUMN source TEXT NOT NULL DEFAULT 'manual'";

/// v3：每封邮件的「已整理」标记（NULL = 未整理，待整理队列的依据）。
/// 同上，真实库已应用，不可修改。
const V3_AI_TAGGED_AT: &str = "ALTER TABLE messages ADD COLUMN ai_tagged_at TEXT";

/// v4：AI 智能整理的本期全部变更。
/// - `tag_groups.ai_mode`：该组是否参与 AI 整理（'off' | 'select' | 'create'）。
/// - `tags.created_by`：标签由谁创建（'user' | 'ai'），AI 创建的可在界面区分、用户可删。
/// - 未整理队列与发件人快速通道的查询索引。
const V4_AI_CLASSIFY: &str = r#"
ALTER TABLE tag_groups ADD COLUMN ai_mode TEXT NOT NULL DEFAULT 'off';
ALTER TABLE tags ADD COLUMN created_by TEXT NOT NULL DEFAULT 'user';
CREATE INDEX IF NOT EXISTS idx_messages_ai_untagged ON messages (account_id) WHERE ai_tagged_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_messages_from_addr ON messages (account_id, from_addr);
"#;

/// 全部迁移，(版本号, SQL) 升序排列。
pub const MIGRATIONS: &[(i64, &str)] = &[
    (1, V1_BASELINE),
    (2, V2_MESSAGE_TAG_SOURCE),
    (3, V3_AI_TAGGED_AT),
    (4, V4_AI_CLASSIFY),
];

/// 读取 `user_version`，按序执行所有尚未应用的迁移，返回最终版本。
pub fn apply(conn: &Connection) -> AppResult<i64> {
    apply_list(conn, MIGRATIONS, SCHEMA_VERSION)
}

/// 执行 `migrations` 中 `version > 当前 user_version` 且 `<= upto` 的迁移。
///
/// 独立出来是为了让测试能塞入合成迁移验证机制本身（回滚、幂等、版本推进），
/// 生产代码请走 [`apply`]。
#[doc(hidden)]
pub fn apply_list(conn: &Connection, migrations: &[(i64, &str)], upto: i64) -> AppResult<i64> {
    let mut current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (version, sql) in migrations {
        if *version <= current || *version > upto {
            continue;
        }
        // 迁移语句与其版本号写在同一事务：失败整体回滚、版本不前进，
        // 下次启动会原样重试，不会留下「结构半应用」的中间态。
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        current = *version;
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_matches_last_migration() {
        // 防漂移：加迁移忘了改 SCHEMA_VERSION（或反之）时立刻失败。
        let last = MIGRATIONS.last().expect("迁移列表不应为空").0;
        assert_eq!(last, SCHEMA_VERSION);
    }

    #[test]
    fn migrations_are_ordered_and_unique() {
        let mut prev = 0;
        for (version, _) in MIGRATIONS {
            assert!(*version > prev, "迁移版本必须严格升序：{version}");
            prev = *version;
        }
    }
}
