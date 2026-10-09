use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::{AppError, AppResult};
use crate::models::*;

const SCHEMA: &str = r#"
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
"#;

/// SQLite 仓储层。每次操作独立开连接（SQLite 打开成本极低），
/// 避免跨线程共享 `Connection` 带来的锁竞争。
#[derive(Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: PathBuf) -> AppResult<Self> {
        let store = Self { path };
        {
            let conn = store.conn()?;
            conn.execute_batch(SCHEMA)?;
        }
        Ok(store)
    }

    fn conn(&self) -> AppResult<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }

    /// 仅供集成测试使用：拿一条裸连接，方便直接造数据 / 断言底层行状态。
    ///
    /// 生产代码不要调用它——所有读写都应走上面那些具名方法。
    #[doc(hidden)]
    pub fn raw_conn_for_test(&self) -> Connection {
        let conn = Connection::open(&self.path).expect("打开测试连接失败");
        conn.busy_timeout(Duration::from_secs(5)).expect("设置 busy_timeout 失败");
        conn
    }

    // ------------------------------------------------------------------
    // 账号
    // ------------------------------------------------------------------

    pub fn list_accounts(&self) -> AppResult<Vec<Account>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, email, display_name, imap_host, imap_port, imap_tls,
                    smtp_host, smtp_port, smtp_tls, username, created_at
             FROM accounts ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], row_to_account)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_account(&self, id: &str) -> AppResult<Account> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id, email, display_name, imap_host, imap_port, imap_tls,
                    smtp_host, smtp_port, smtp_tls, username, created_at
             FROM accounts WHERE id = ?1",
            params![id],
            row_to_account,
        )
        .optional()?
        .ok_or_else(|| AppError::AccountNotFound(id.to_string()))
    }

    pub fn insert_account(
        &self,
        id: &str,
        input: &AccountInput,
        created_at: &str,
    ) -> AppResult<Account> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO accounts
                (id, email, display_name, imap_host, imap_port, imap_tls,
                 smtp_host, smtp_port, smtp_tls, username, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                input.email.trim(),
                input.display_name.trim(),
                input.imap_host.trim(),
                input.imap_port,
                input.imap_tls as i64,
                input.smtp_host.trim(),
                input.smtp_port,
                input.smtp_tls as i64,
                input.username.trim(),
                created_at,
            ],
        )?;
        drop(conn);
        self.get_account(id)
    }

    pub fn delete_account(&self, id: &str) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM messages WHERE account_id = ?1", params![id])?;
        conn.execute("DELETE FROM folders WHERE account_id = ?1", params![id])?;
        conn.execute("DELETE FROM sync_state WHERE account_id = ?1", params![id])?;
        conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 文件夹 / 同步游标
    // ------------------------------------------------------------------

    pub fn list_folders(&self, account_id: &str) -> AppResult<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT name FROM folders WHERE account_id = ?1 ORDER BY name ASC")?;
        let rows = stmt.query_map(params![account_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn upsert_folder(&self, account_id: &str, name: &str) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO folders (account_id, name) VALUES (?1, ?2)
             ON CONFLICT (account_id, name) DO NOTHING",
            params![account_id, name],
        )?;
        Ok(())
    }

    pub fn last_uid(&self, account_id: &str, folder: &str) -> AppResult<u32> {
        let conn = self.conn()?;
        let v: Option<i64> = conn
            .query_row(
                "SELECT last_uid FROM sync_state WHERE account_id = ?1 AND folder = ?2",
                params![account_id, folder],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.unwrap_or(0).max(0) as u32)
    }

    pub fn set_last_uid(&self, account_id: &str, folder: &str, uid: u32, at: &str) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO sync_state (account_id, folder, last_uid, last_sync_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account_id, folder) DO UPDATE SET
                last_uid = MAX(last_uid, excluded.last_uid),
                last_sync_at = excluded.last_sync_at",
            params![account_id, folder, uid, at],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 邮件
    // ------------------------------------------------------------------

    /// 写入(或更新)邮件头信息，返回新增条数。
    pub fn upsert_headers(
        &self,
        account_id: &str,
        folder: &str,
        mails: &[ParsedMail],
        synced_at: &str,
    ) -> AppResult<usize> {
        if mails.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut inserted = 0usize;
        for m in mails {
            let changed = tx.execute(
                "INSERT INTO messages
                    (account_id, folder, uid, message_id, subject, from_name, from_addr,
                     to_addrs, cc_addrs, date_ts, seen, flagged, has_attachments, synced_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT (account_id, folder, uid) DO UPDATE SET
                    message_id      = excluded.message_id,
                    subject         = excluded.subject,
                    from_name       = excluded.from_name,
                    from_addr       = excluded.from_addr,
                    to_addrs        = excluded.to_addrs,
                    cc_addrs        = excluded.cc_addrs,
                    date_ts         = excluded.date_ts,
                    seen            = excluded.seen,
                    flagged         = excluded.flagged,
                    has_attachments = MAX(messages.has_attachments, excluded.has_attachments),
                    synced_at       = excluded.synced_at",
                params![
                    account_id,
                    folder,
                    m.uid as i64,
                    m.message_id,
                    m.subject,
                    m.from_name,
                    m.from_addr,
                    m.to_addrs,
                    m.cc_addrs,
                    m.date_ts,
                    m.seen as i64,
                    m.flagged as i64,
                    (!m.attachments.is_empty()) as i64,
                    synced_at,
                ],
            )?;
            inserted += changed;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// 写入邮件正文（按需拉取后调用），同时刷新摘要片段。
    pub fn save_body(
        &self,
        account_id: &str,
        folder: &str,
        mail: &ParsedMail,
        synced_at: &str,
    ) -> AppResult<()> {
        let attachments = serde_json::to_string(&mail.attachments)?;
        let snippet = make_snippet(&mail.body_text, &mail.subject);
        let conn = self.conn()?;
        conn.execute(
            "UPDATE messages SET
                body_text = ?1, body_html = ?2, attachments = ?3,
                snippet = ?4, has_attachments = ?5, body_loaded = 1,
                to_addrs = ?6, cc_addrs = ?7, message_id = ?8, synced_at = ?9
             WHERE account_id = ?10 AND folder = ?11 AND uid = ?12",
            params![
                mail.body_text,
                mail.body_html,
                attachments,
                snippet,
                (!mail.attachments.is_empty()) as i64,
                mail.to_addrs,
                mail.cc_addrs,
                mail.message_id,
                synced_at,
                account_id,
                folder,
                mail.uid as i64,
            ],
        )?;
        Ok(())
    }

    pub fn list_messages(
        &self,
        account_id: &str,
        folder: &str,
        limit: u32,
        offset: u32,
    ) -> AppResult<Vec<MessageHeader>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, uid, folder, subject, from_name, from_addr, date_ts,
                    seen, flagged, has_attachments, body_loaded, snippet
             FROM messages
             WHERE account_id = ?1 AND folder = ?2
             ORDER BY date_ts DESC, id DESC
             LIMIT ?3 OFFSET ?4",
        )?;
        let rows = stmt.query_map(
            params![account_id, folder, limit as i64, offset as i64],
            row_to_header,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_message(&self, id: i64) -> AppResult<MessageDetail> {
        let conn = self.conn()?;
        let detail = conn
            .query_row(
                "SELECT id, uid, folder, subject, from_name, from_addr, date_ts,
                        seen, flagged, has_attachments, body_loaded, snippet,
                        to_addrs, cc_addrs, message_id, body_text, body_html, attachments
                 FROM messages WHERE id = ?1",
                params![id],
                |row| {
                    let header = row_to_header(row)?;
                    let attachments_json: String = row.get("attachments")?;
                    Ok(MessageDetail {
                        header,
                        to_addrs: row.get("to_addrs")?,
                        cc_addrs: row.get("cc_addrs")?,
                        message_id: row.get("message_id")?,
                        body_text: row.get("body_text")?,
                        body_html: row.get("body_html")?,
                        attachments: serde_json::from_str(&attachments_json).unwrap_or_default(),
                    })
                },
            )
            .optional()?;
        detail.ok_or_else(|| AppError::Other("邮件不存在".to_string()))
    }

    /// 按 uid 查找已缓存邮件的 id。
    pub fn find_id_by_uid(
        &self,
        account_id: &str,
        folder: &str,
        uid: u32,
    ) -> AppResult<Option<i64>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT id FROM messages WHERE account_id = ?1 AND folder = ?2 AND uid = ?3",
                params![account_id, folder, uid as i64],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_seen(&self, id: i64, seen: bool) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE messages SET seen = ?1 WHERE id = ?2",
            params![seen as i64, id],
        )?;
        Ok(())
    }

    pub fn set_flagged(&self, id: i64, flagged: bool) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE messages SET flagged = ?1 WHERE id = ?2",
            params![flagged as i64, id],
        )?;
        Ok(())
    }

    /// 删除单封邮件的本地缓存，并清掉该邮件的 AI 摘要缓存。
    /// 返回是否确实删掉了一行（false 表示邮件本就不存在）。
    pub fn delete_message(&self, id: i64) -> AppResult<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let removed = tx.execute("DELETE FROM messages WHERE id = ?1", params![id])?;
        // 摘要缓存键形如 `summary:{id}:{message_id}:{model}`，按前缀清理即可。
        tx.execute(
            "DELETE FROM ai_cache WHERE cache_key LIKE ?1",
            params![format!("summary:{id}:%")],
        )?;
        tx.commit()?;
        Ok(removed > 0)
    }

    /// 批量删除本地缓存（按 id 列表），用于删除失败后的回滚清理。
    pub fn delete_messages(&self, ids: &[i64]) -> AppResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut removed = 0usize;
        for id in ids {
            removed += tx.execute("DELETE FROM messages WHERE id = ?1", params![id])?;
            tx.execute(
                "DELETE FROM ai_cache WHERE cache_key LIKE ?1",
                params![format!("summary:{id}:%")],
            )?;
        }
        tx.commit()?;
        Ok(removed)
    }

    /// 邮件所在文件夹的剩余邮件数，用于同步 `folders.messages` 计数。
    pub fn refresh_folder_count(&self, account_id: &str, folder: &str) -> AppResult<u32> {
        let count = self.count_messages(account_id, folder)?;
        let conn = self.conn()?;
        conn.execute(
            "UPDATE folders SET messages = ?1 WHERE account_id = ?2 AND name = ?3",
            params![count as i64, account_id, folder],
        )?;
        Ok(count)
    }

    /// 增量同步用的「当前库内最大 UID」。
    pub fn max_uid(&self, account_id: &str, folder: &str) -> AppResult<u32> {
        let conn = self.conn()?;
        let v: Option<i64> = conn.query_row(
            "SELECT MAX(uid) FROM messages WHERE account_id = ?1 AND folder = ?2",
            params![account_id, folder],
            |r| r.get(0),
        )?;
        Ok(v.unwrap_or(0).max(0) as u32)
    }

    pub fn count_messages(&self, account_id: &str, folder: &str) -> AppResult<u32> {
        let conn = self.conn()?;
        let v: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE account_id = ?1 AND folder = ?2",
            params![account_id, folder],
            |r| r.get(0),
        )?;
        Ok(v as u32)
    }

    // ------------------------------------------------------------------
    // AI 缓存
    // ------------------------------------------------------------------

    pub fn get_cache(&self, key: &str) -> AppResult<Option<(String, String)>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT content, model FROM ai_cache WHERE cache_key = ?1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn put_cache(&self, key: &str, kind: &str, model: &str, content: &str, at: &str) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO ai_cache (cache_key, kind, model, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (cache_key) DO UPDATE SET
                content = excluded.content, model = excluded.model, created_at = excluded.created_at",
            params![key, kind, model, content, at],
        )?;
        Ok(())
    }

    pub fn delete_cache_prefix(&self, prefix: &str) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM ai_cache WHERE cache_key LIKE ?1",
            params![format!("{prefix}%")],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 配置
    // ------------------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn ai_settings(&self) -> AppResult<AiSettings> {
        let raw = self.get_setting("ai")?;
        Ok(raw
            .and_then(|s| serde_json::from_str::<AiSettings>(&s).ok())
            .unwrap_or_default())
    }

    pub fn save_ai_settings(&self, s: &AiSettings) -> AppResult<()> {
        self.set_setting("ai", &serde_json::to_string(s)?)
    }

    // ------------------------------------------------------------------
    // 邮件路由
    // ------------------------------------------------------------------

    /// 返回邮件所属的 (account_id, folder, uid)，用于按需拉取正文与回写标记。
    pub fn message_route(&self, id: i64) -> AppResult<(String, String, u32)> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT account_id, folder, uid FROM messages WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u32)),
        )
        .optional()?
        .ok_or_else(|| AppError::Other("邮件不存在".to_string()))
    }
}

fn row_to_account(row: &Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get("id")?,
        email: row.get("email")?,
        display_name: row.get("display_name")?,
        imap_host: row.get("imap_host")?,
        imap_port: row.get::<_, i64>("imap_port")? as u16,
        imap_tls: row.get::<_, i64>("imap_tls")? != 0,
        smtp_host: row.get("smtp_host")?,
        smtp_port: row.get::<_, i64>("smtp_port")? as u16,
        smtp_tls: row.get::<_, i64>("smtp_tls")? != 0,
        username: row.get("username")?,
        created_at: row.get("created_at")?,
    })
}

fn row_to_header(row: &Row) -> rusqlite::Result<MessageHeader> {
    Ok(MessageHeader {
        id: row.get("id")?,
        uid: row.get::<_, i64>("uid")? as u32,
        folder: row.get("folder")?,
        subject: row.get("subject")?,
        from_name: row.get("from_name")?,
        from_addr: row.get("from_addr")?,
        date_ts: row.get("date_ts")?,
        seen: row.get::<_, i64>("seen")? != 0,
        flagged: row.get::<_, i64>("flagged")? != 0,
        has_attachments: row.get::<_, i64>("has_attachments")? != 0,
        body_loaded: row.get::<_, i64>("body_loaded")? != 0,
        snippet: row.get("snippet")?,
    })
}

/// 生成列表摘要片段：优先用纯文本正文，没有正文时退回主题。
fn make_snippet(body_text: &str, subject: &str) -> String {
    let cleaned = body_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('>'))
        .collect::<Vec<_>>()
        .join(" ");
    let src = if cleaned.is_empty() {
        subject.to_string()
    } else {
        cleaned
    };
    src.chars().take(140).collect()
}
