use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::classify::{ClassifyInput, DimensionGroup, GroupAiMode, Pick};
use crate::error::{AppError, AppResult};
use crate::models::*;
use crate::schema;


/// 单封邮件对某个启用维度组的指派（[`Store::save_classify_result`] 的入参）。
#[derive(Debug, Clone)]
pub struct GroupAssignment {
    pub group_id: i64,
    /// `None` = 该维度不指派（低置信 / 模型判空 / 快速通道无共识）。
    pub pick: Option<Pick>,
}

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
            // 建表与版本化迁移统一由 schema 模块负责（含存量库的版本补齐）。
            schema::apply(&conn)?;
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
        // 先清打标关系（依赖 tags 表尚未删除），再清组员关系，最后删各自主表。
        conn.execute(
            "DELETE FROM message_tags WHERE tag_id IN (SELECT id FROM tags WHERE account_id = ?1)",
            params![id],
        )?;
        conn.execute(
            "DELETE FROM tag_group_members
             WHERE group_id IN (SELECT id FROM tag_groups WHERE account_id = ?1)
                OR tag_id IN (SELECT id FROM tags WHERE account_id = ?1)",
            params![id],
        )?;
        conn.execute("DELETE FROM tags WHERE account_id = ?1", params![id])?;
        conn.execute("DELETE FROM tag_groups WHERE account_id = ?1", params![id])?;
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
        self.list_messages_filtered(account_id, folder, limit, offset, None)
    }

    /// 列邮件，可附带「限定某一标签」。`tag_id` 为 `None` 表示不筛选。
    ///
    /// 标签筛选**下推到 SQL**（EXISTS 子查询），而不是捞出全表后在应用层过滤——
    /// 后者会让「按标签看邮件」在大收件箱下先加载几千条再丢掉绝大多数。
    pub fn list_messages_filtered(
        &self,
        account_id: &str,
        folder: &str,
        limit: u32,
        offset: u32,
        tag_id: Option<i64>,
    ) -> AppResult<Vec<MessageHeader>> {
        self.list_messages_multi(account_id, folder, limit, offset, &tag_id.into_iter().collect::<Vec<_>>())
    }

    /// 多选标签筛选：命中**任一**标签即入选（OR）。
    ///
    /// `tag_ids` 为空 = 不筛选。多选语义选定为 OR 而非 AND，是因为侧栏的
    /// 「组内多选 / 组间多选」直觉上是「把这几类都看一下」，AND 会让多选
    /// 迅速收敛到空集（邮件很少同时带多个标签）。
    pub fn list_messages_multi(
        &self,
        account_id: &str,
        folder: &str,
        limit: u32,
        offset: u32,
        tag_ids: &[i64],
    ) -> AppResult<Vec<MessageHeader>> {
        let conn = self.conn()?;
        // 无筛选时走最简路径，避免为「全部邮件」这个高频场景拼动态 SQL。
        if tag_ids.is_empty() {
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
            drop(stmt);
            self.attach_tags(&conn, account_id, &mut out)?;
            return Ok(out);
        }

        // 动态标签用普通 `?`，绑定顺序严格等于 SQL 中出现顺序。
        //
        // 这里刻意不写 `?1..?4`：rusqlite 的 query_map 是**按位置**绑定，
        // `?1` 指的是「第一个绑定值」而不是「下标 1 的值」。混用编号占位符和
        // 追加在末尾的标签值，会让编号槽错位（实测报 InvalidParameterCount）。
        let placeholders = vec!["?"; tag_ids.len()].join(",");
        let sql = format!(
            "SELECT id, uid, folder, subject, from_name, from_addr, date_ts,
                    seen, flagged, has_attachments, body_loaded, snippet
             FROM messages
             WHERE account_id = ? AND folder = ?
               AND EXISTS (
                     SELECT 1 FROM message_tags mt
                     WHERE mt.message_id = messages.id AND mt.tag_id IN ({placeholders}))
             ORDER BY date_ts DESC, id DESC
             LIMIT ? OFFSET ?"
        );
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(tag_ids.len() + 4);
        values.push(Box::new(account_id.to_string()));
        values.push(Box::new(folder.to_string()));
        for id in tag_ids {
            values.push(Box::new(*id));
        }
        values.push(Box::new(limit as i64));
        values.push(Box::new(offset as i64));
        let refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(refs.as_slice(), row_to_header)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        drop(stmt);
        self.attach_tags(&conn, account_id, &mut out)?;
        Ok(out)
    }

    /// 某组下全部邮件的去重计数。
    ///
    /// 必须用 `COUNT(DISTINCT message_id)`——一封邮件同时带组内两个标签时，
    /// 直接 COUNT 会把它算两次，侧栏角标就虚高。
    pub fn group_message_count(&self, group_id: i64) -> AppResult<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(DISTINCT mt.message_id)
             FROM tag_group_members gm
             JOIN message_tags mt ON mt.tag_id = gm.tag_id
             WHERE gm.group_id = ?1",
            params![group_id],
            |r| r.get(0),
        )?)
    }

    /// 给一批邮件头补上各自的标签 id 集合。
    ///
    /// 用一次 `IN (...)` 查询取代「每封邮件查一次」——列表默认 200 条，
    /// 逐封查就是 200 次往返，是典型的 N+1。
    fn attach_tags(
        &self,
        conn: &Connection,
        account_id: &str,
        headers: &mut [MessageHeader],
    ) -> AppResult<()> {
        if headers.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = headers.iter().map(|h| h.id).collect();
        // 参数占位符按实际长度拼接；值本身仍走绑定，不做字符串拼接。
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT mt.message_id, mt.tag_id
             FROM message_tags mt
             JOIN tags t ON t.id = mt.tag_id
             WHERE t.account_id = ? AND mt.message_id IN ({placeholders})"
        );
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(ids.len() + 1);
        values.push(Box::new(account_id.to_string()));
        for id in &ids {
            values.push(Box::new(*id));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(refs.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut pairs: Vec<(i64, i64)> = Vec::new();
        for r in rows {
            pairs.push(r?);
        }
        drop(stmt);

        for h in headers.iter_mut() {
            h.tag_ids = pairs
                .iter()
                .filter(|(mid, _)| *mid == h.id)
                .map(|(_, tid)| *tid)
                .collect();
        }
        Ok(())
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
        // 打标关系一并清掉，避免标签角标统计到已删除的邮件。
        tx.execute(
            "DELETE FROM message_tags WHERE message_id = ?1",
            params![id],
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
            tx.execute("DELETE FROM message_tags WHERE message_id = ?1", params![id])?;
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
    // 标签
    // ------------------------------------------------------------------

    /// 列出某账号下的全部标签，附带各自的邮件数。
    ///
    /// 计数用 LEFT JOIN 聚合而不是对每个标签发一条 COUNT——
    /// 侧栏渲染要显示每个标签的角标，N 个标签就是 N 次查询。
    pub fn list_tags(&self, account_id: &str) -> AppResult<Vec<Tag>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT t.id, t.account_id, t.name, t.color, t.sort_order, t.created_by,
                    COUNT(mt.message_id) AS message_count
             FROM tags t
             LEFT JOIN message_tags mt ON mt.tag_id = t.id
             WHERE t.account_id = ?1
             GROUP BY t.id
             ORDER BY t.sort_order ASC, t.id ASC",
        )?;
        let rows = stmt.query_map(params![account_id], |row| {
            Ok(Tag {
                id: row.get("id")?,
                account_id: row.get("account_id")?,
                name: row.get("name")?,
                color: row.get("color")?,
                sort_order: row.get("sort_order")?,
                message_count: row.get("message_count")?,
                created_by: row.get("created_by")?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 新建标签。同名时返回 `AppError::Other`（由 UNIQUE 约束兜底，先查一次给出可读文案）。
    pub fn create_tag(
        &self,
        account_id: &str,
        name: &str,
        color: &str,
        created_at: &str,
    ) -> AppResult<Tag> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::Other("标签名不能为空".to_string()));
        }
        if name.chars().count() > 24 {
            return Err(AppError::Other("标签名不宜超过 24 个字符".to_string()));
        }
        let conn = self.conn()?;
        let exists: Option<i64> = conn
            .query_row(
                "SELECT id FROM tags WHERE account_id = ?1 AND name = ?2",
                params![account_id, name],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_some() {
            return Err(AppError::Other(format!("标签「{name}」已存在")));
        }

        // 新标签排在末尾：取当前最大 sort_order + 1。
        let next_order: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM tags WHERE account_id = ?1",
                params![account_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        conn.execute(
            "INSERT INTO tags (account_id, name, color, sort_order, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![account_id, name, color, next_order, created_at],
        )?;
        let id = conn.last_insert_rowid();
        Ok(Tag {
            id,
            account_id: account_id.to_string(),
            name: name.to_string(),
            color: color.to_string(),
            sort_order: next_order,
            message_count: 0,
            // 手动创建的标签一律归 user；AI 整理创建走独立入口（classify 写回事务）。
            created_by: "user".to_string(),
        })
    }

    /// 重命名 / 改色。名称冲突时给出可读错误。
    pub fn update_tag(&self, id: i64, name: Option<&str>, color: Option<&str>) -> AppResult<()> {
        let conn = self.conn()?;
        if let Some(raw) = name {
            let name = raw.trim();
            if name.is_empty() {
                return Err(AppError::Other("标签名不能为空".to_string()));
            }
            if name.chars().count() > 24 {
                return Err(AppError::Other("标签名不宜超过 24 个字符".to_string()));
            }
            let account_id: String = conn
                .query_row("SELECT account_id FROM tags WHERE id = ?1", params![id], |r| {
                    r.get(0)
                })
                .optional()?
                .ok_or_else(|| AppError::Other("标签不存在".to_string()))?;
            let clash: Option<i64> = conn
                .query_row(
                    "SELECT id FROM tags WHERE account_id = ?1 AND name = ?2 AND id <> ?3",
                    params![account_id, name, id],
                    |r| r.get(0),
                )
                .optional()?;
            if clash.is_some() {
                return Err(AppError::Other(format!("标签「{name}」已存在")));
            }
            conn.execute("UPDATE tags SET name = ?1 WHERE id = ?2", params![name, id])?;
        }
        if let Some(color) = color {
            conn.execute("UPDATE tags SET color = ?1 WHERE id = ?2", params![color, id])?;
        }
        Ok(())
    }

    /// 删除标签，同时清掉所有打标关系与组员关系（组本身保留）。
    pub fn delete_tag(&self, id: i64) -> AppResult<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM message_tags WHERE tag_id = ?1", params![id])?;
        tx.execute("DELETE FROM tag_group_members WHERE tag_id = ?1", params![id])?;
        tx.execute("DELETE FROM tags WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// 给一封邮件打标。已打过则静默忽略（幂等），便于前端做开关式调用。
    pub fn attach_tag(&self, message_id: i64, tag_id: i64, created_at: &str) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO message_tags (message_id, tag_id, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (message_id, tag_id) DO NOTHING",
            params![message_id, tag_id, created_at],
        )?;
        Ok(())
    }

    pub fn detach_tag(&self, message_id: i64, tag_id: i64) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM message_tags WHERE message_id = ?1 AND tag_id = ?2",
            params![message_id, tag_id],
        )?;
        Ok(())
    }

    /// 一次给多封邮件打同一个标签（供列表多选使用）。
    pub fn attach_tag_to_messages(
        &self,
        message_ids: &[i64],
        tag_id: i64,
        created_at: &str,
    ) -> AppResult<usize> {
        if message_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut changed = 0usize;
        for mid in message_ids {
            changed += tx.execute(
                "INSERT INTO message_tags (message_id, tag_id, created_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (message_id, tag_id) DO NOTHING",
                params![mid, tag_id, created_at],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn detach_tag_from_messages(&self, message_ids: &[i64], tag_id: i64) -> AppResult<usize> {
        if message_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut changed = 0usize;
        for mid in message_ids {
            changed += tx.execute(
                "DELETE FROM message_tags WHERE message_id = ?1 AND tag_id = ?2",
                params![mid, tag_id],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// 单封邮件的标签 id 集合。
    pub fn tags_of_message(&self, message_id: i64) -> AppResult<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT tag_id FROM message_tags WHERE message_id = ?1 ORDER BY tag_id ASC",
        )?;
        let rows = stmt.query_map(params![message_id], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 删除账号时连带清理其标签与打标关系。
    pub fn delete_account_tags(&self, account_id: &str) -> AppResult<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM message_tags WHERE tag_id IN (SELECT id FROM tags WHERE account_id = ?1)",
            params![account_id],
        )?;
        tx.execute(
            "DELETE FROM tag_group_members
             WHERE group_id IN (SELECT id FROM tag_groups WHERE account_id = ?1)",
            params![account_id],
        )?;
        tx.execute("DELETE FROM tags WHERE account_id = ?1", params![account_id])?;
        tx.execute("DELETE FROM tag_groups WHERE account_id = ?1", params![account_id])?;
        tx.commit()?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 标签组
    // ------------------------------------------------------------------

    /// 列出某账号下的全部标签组，附带成员标签与去重后的邮件数。
    pub fn list_tag_groups(&self, account_id: &str) -> AppResult<Vec<TagGroup>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, account_id, name, color, sort_order, collapsed, ai_mode
             FROM tag_groups
             WHERE account_id = ?1
             ORDER BY sort_order ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![account_id], |row| {
            Ok(TagGroup {
                id: row.get("id")?,
                account_id: row.get("account_id")?,
                name: row.get("name")?,
                color: row.get("color")?,
                sort_order: row.get("sort_order")?,
                collapsed: row.get::<_, i64>("collapsed")? != 0,
                tag_ids: Vec::new(),
                message_count: 0,
                raw_message_count: 0,
                ai_mode: row.get("ai_mode")?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        drop(stmt);
        self.attach_group_members(&conn, account_id, &mut out)?;
        Ok(out)
    }

    /// 回填每个组的成员标签 id 与邮件计数。
    ///
    /// 与 `attach_tags` 同理：一次性查完全部组的成员，避免「N 个组查 N 次」。
    fn attach_group_members(
        &self,
        conn: &Connection,
        account_id: &str,
        groups: &mut [TagGroup],
    ) -> AppResult<()> {
        if groups.is_empty() {
            return Ok(());
        }

        // ---- 1) 成员标签 id ----
        let mut stmt = conn.prepare(
            "SELECT gm.group_id, gm.tag_id
             FROM tag_group_members gm
             JOIN tag_groups g ON g.id = gm.group_id
             WHERE g.account_id = ?1
             ORDER BY gm.group_id ASC, gm.tag_id ASC",
        )?;
        let rows = stmt.query_map(params![account_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut pairs: Vec<(i64, i64)> = Vec::new();
        for r in rows {
            pairs.push(r?);
        }
        drop(stmt);

        // ---- 2) 每个标签的邮件数（供 raw 计数用）----
        let mut stmt = conn.prepare(
            "SELECT t.id, COUNT(mt.message_id)
             FROM tags t
             LEFT JOIN message_tags mt ON mt.tag_id = t.id
             WHERE t.account_id = ?1
             GROUP BY t.id",
        )?;
        let rows = stmt.query_map(params![account_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut tag_counts: Vec<(i64, i64)> = Vec::new();
        for r in rows {
            tag_counts.push(r?);
        }
        drop(stmt);

        // ---- 3) 每组的去重邮件数 ----
        let mut stmt = conn.prepare(
            "SELECT g.id, COUNT(DISTINCT mt.message_id)
             FROM tag_groups g
             LEFT JOIN tag_group_members gm ON gm.group_id = g.id
             LEFT JOIN message_tags mt ON mt.tag_id = gm.tag_id
             WHERE g.account_id = ?1
             GROUP BY g.id",
        )?;
        let rows = stmt.query_map(params![account_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut dedup: Vec<(i64, i64)> = Vec::new();
        for r in rows {
            dedup.push(r?);
        }
        drop(stmt);

        for g in groups.iter_mut() {
            g.tag_ids = pairs
                .iter()
                .filter(|(gid, _)| *gid == g.id)
                .map(|(_, tid)| *tid)
                .collect();
            g.raw_message_count = g
                .tag_ids
                .iter()
                .filter_map(|tid| tag_counts.iter().find(|(id, _)| id == tid).map(|(_, c)| *c))
                .sum();
            g.message_count = dedup
                .iter()
                .find(|(id, _)| *id == g.id)
                .map(|(_, c)| *c)
                .unwrap_or(0);
        }
        Ok(())
    }

    pub fn create_tag_group(
        &self,
        account_id: &str,
        name: &str,
        color: &str,
        created_at: &str,
    ) -> AppResult<TagGroup> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::Other("组名不能为空".to_string()));
        }
        if name.chars().count() > 24 {
            return Err(AppError::Other("组名不宜超过 24 个字符".to_string()));
        }
        let conn = self.conn()?;
        let exists: Option<i64> = conn
            .query_row(
                "SELECT id FROM tag_groups WHERE account_id = ?1 AND name = ?2",
                params![account_id, name],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_some() {
            return Err(AppError::Other(format!("标签组「{name}」已存在")));
        }

        let next_order: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM tag_groups WHERE account_id = ?1",
                params![account_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        conn.execute(
            "INSERT INTO tag_groups (account_id, name, color, sort_order, collapsed, created_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5)",
            params![account_id, name, color, next_order, created_at],
        )?;
        let id = conn.last_insert_rowid();
        Ok(TagGroup {
            id,
            account_id: account_id.to_string(),
            name: name.to_string(),
            color: color.to_string(),
            sort_order: next_order,
            collapsed: false,
            tag_ids: Vec::new(),
            message_count: 0,
            raw_message_count: 0,
            // 新组默认不参与 AI 整理，由用户在组编辑器里显式开启。
            ai_mode: "off".to_string(),
        })
    }

    pub fn update_tag_group(
        &self,
        id: i64,
        name: Option<&str>,
        color: Option<&str>,
        collapsed: Option<bool>,
        ai_mode: Option<&str>,
    ) -> AppResult<()> {
        let conn = self.conn()?;
        if let Some(raw) = ai_mode {
            if !matches!(raw, "off" | "select" | "create") {
                return Err(AppError::Other(format!("无效的 AI 整理模式：{raw}")));
            }
        }
        if let Some(raw) = name {
            let name = raw.trim();
            if name.is_empty() {
                return Err(AppError::Other("组名不能为空".to_string()));
            }
            if name.chars().count() > 24 {
                return Err(AppError::Other("组名不宜超过 24 个字符".to_string()));
            }
            let account_id: String = conn
                .query_row(
                    "SELECT account_id FROM tag_groups WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| AppError::Other("标签组不存在".to_string()))?;
            let clash: Option<i64> = conn
                .query_row(
                    "SELECT id FROM tag_groups WHERE account_id = ?1 AND name = ?2 AND id <> ?3",
                    params![account_id, name, id],
                    |r| r.get(0),
                )
                .optional()?;
            if clash.is_some() {
                return Err(AppError::Other(format!("标签组「{name}」已存在")));
            }
            conn.execute("UPDATE tag_groups SET name = ?1 WHERE id = ?2", params![name, id])?;
        }
        if let Some(color) = color {
            conn.execute("UPDATE tag_groups SET color = ?1 WHERE id = ?2", params![color, id])?;
        }
        if let Some(collapsed) = collapsed {
            conn.execute(
                "UPDATE tag_groups SET collapsed = ?1 WHERE id = ?2",
                params![collapsed as i64, id],
            )?;
        }
        if let Some(ai_mode) = ai_mode {
            conn.execute(
                "UPDATE tag_groups SET ai_mode = ?1 WHERE id = ?2",
                params![ai_mode, id],
            )?;
        }
        Ok(())
    }

    /// 删除组。**只解绑成员关系，不删标签本身，也不动邮件打标。**
    pub fn delete_tag_group(&self, id: i64) -> AppResult<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM tag_group_members WHERE group_id = ?1", params![id])?;
        tx.execute("DELETE FROM tag_groups WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// 给组添加标签成员（幂等）。
    pub fn attach_tag_to_group(&self, group_id: i64, tag_id: i64, created_at: &str) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO tag_group_members (group_id, tag_id, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (group_id, tag_id) DO NOTHING",
            params![group_id, tag_id, created_at],
        )?;
        Ok(())
    }

    pub fn detach_tag_from_group(&self, group_id: i64, tag_id: i64) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM tag_group_members WHERE group_id = ?1 AND tag_id = ?2",
            params![group_id, tag_id],
        )?;
        Ok(())
    }

    /// 整组覆盖式设置成员标签（前端编辑器保存时用）。
    ///
    /// 用「先清后插」而非逐条 diff：成员数量级很小（个位数），
    /// 覆盖写入更简单且不会漏掉删除项。整体在事务里，不会出现中间态。
    pub fn set_group_tags(&self, group_id: i64, tag_ids: &[i64], created_at: &str) -> AppResult<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM tag_group_members WHERE group_id = ?1", params![group_id])?;
        for tag_id in tag_ids {
            tx.execute(
                "INSERT INTO tag_group_members (group_id, tag_id, created_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (group_id, tag_id) DO NOTHING",
                params![group_id, tag_id, created_at],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 某组包含的标签 id（用于「选择组本身」时展开为标签集合）。
    pub fn group_tag_ids(&self, group_id: i64) -> AppResult<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT tag_id FROM tag_group_members WHERE group_id = ?1 ORDER BY tag_id ASC",
        )?;
        let rows = stmt.query_map(params![group_id], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 多个组展开后的标签并集（去重），供「选中多个组」时筛选。
    pub fn groups_tag_ids(&self, group_ids: &[i64]) -> AppResult<Vec<i64>> {
        if group_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn()?;
        let placeholders = vec!["?"; group_ids.len()].join(",");
        let sql = format!(
            "SELECT DISTINCT tag_id FROM tag_group_members
             WHERE group_id IN ({placeholders}) ORDER BY tag_id ASC"
        );
        let refs: Vec<&dyn rusqlite::ToSql> = group_ids
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(refs.as_slice(), |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 某标签所属的全部组 id（多对多下侧栏渲染与「归属」提示用）。
    pub fn groups_of_tag(&self, tag_id: i64) -> AppResult<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT group_id FROM tag_group_members WHERE tag_id = ?1 ORDER BY group_id ASC",
        )?;
        let rows = stmt.query_map(params![tag_id], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 删除标签时同步摘掉它在所有组里的成员关系（组本身保留）。
    pub fn detach_tag_from_all_groups(&self, tag_id: i64) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM tag_group_members WHERE tag_id = ?1",
            params![tag_id],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 智能整理（AI 分类）
    // ------------------------------------------------------------------

    /// 读出该账号全部「参与 AI 整理」的维度组（含候选标签，按展示顺序）。
    ///
    /// 组数量个位数，逐组查候选即可，无需刻意合并成一条 SQL。
    pub fn ai_dimension_groups(&self, account_id: &str) -> AppResult<Vec<DimensionGroup>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, ai_mode FROM tag_groups
             WHERE account_id = ?1 AND ai_mode != 'off'
             ORDER BY sort_order ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![account_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut raw: Vec<(i64, String, String)> = Vec::new();
        for r in rows {
            raw.push(r?);
        }
        drop(stmt);

        let mut out = Vec::with_capacity(raw.len());
        for (id, name, mode) in raw {
            let Some(mode) = GroupAiMode::from_db(&mode) else {
                continue; // 未知模式视为不参与
            };
            let mut stmt = conn.prepare(
                "SELECT t.id, t.name FROM tags t
                 JOIN tag_group_members gm ON gm.tag_id = t.id
                 WHERE gm.group_id = ?1
                 ORDER BY t.sort_order ASC, t.id ASC",
            )?;
            let rows = stmt.query_map(params![id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            let mut candidates = Vec::new();
            for r in rows {
                candidates.push(r?);
            }
            out.push(DimensionGroup {
                id,
                name,
                mode,
                candidates,
            });
        }
        Ok(out)
    }

    /// 待整理队列：尚未整理（`ai_tagged_at IS NULL`）的邮件 id，最新优先。
    ///
    /// Pony 目前只在收件箱工作，队列按账号级取（与 Hony 的账户级一致）。
    pub fn unclassified_ids(&self, account_id: &str, limit: i64) -> AppResult<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM messages
             WHERE account_id = ?1 AND ai_tagged_at IS NULL
             ORDER BY date_ts DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![account_id, limit], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 还剩多少封未整理（`Done` 事件里的「剩余」）。
    pub fn count_unclassified(&self, account_id: &str) -> AppResult<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE account_id = ?1 AND ai_tagged_at IS NULL",
            params![account_id],
            |r| r.get(0),
        )?)
    }

    /// 组装分类输入：正文**不预取**——只有已缓存的正文才带上，
    /// 让整理能覆盖从未打开过的邮件。
    pub fn classify_input(&self, message_id: i64) -> AppResult<ClassifyInput> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT from_name, from_addr, subject, date_ts, snippet, body_loaded, body_text
             FROM messages WHERE id = ?1",
            params![message_id],
            |row| {
                let body_loaded: i64 = row.get("body_loaded")?;
                let body_text: String = row.get("body_text")?;
                Ok(ClassifyInput {
                    from_name: row.get("from_name")?,
                    from_addr: row.get("from_addr")?,
                    subject: row.get("subject")?,
                    date_ts: row.get("date_ts")?,
                    snippet: row.get("snippet")?,
                    body_text: (body_loaded != 0 && !body_text.trim().is_empty())
                        .then_some(body_text),
                })
            },
        )
        .optional()?
        .ok_or_else(|| AppError::Other("邮件不存在".to_string()))
    }

    /// 发件人历史样本数（只统计**已整理**的邮件——快速通道与提示的依据）。
    pub fn sender_classified_count(
        &self,
        account_id: &str,
        from_addr: &str,
        exclude_id: i64,
    ) -> AppResult<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM messages
             WHERE account_id = ?1 AND from_addr = ?2 AND id != ?3 AND ai_tagged_at IS NOT NULL",
            params![account_id, from_addr, exclude_id],
            |r| r.get(0),
        )?)
    }

    /// 该发件人已整理邮件在指定组内各标签的分布（按出现次数降序）。
    pub fn sender_group_stats(
        &self,
        account_id: &str,
        from_addr: &str,
        exclude_id: i64,
        group_id: i64,
    ) -> AppResult<Vec<(i64, String, i64)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT t.id, t.name, COUNT(*) AS n
             FROM messages m
             JOIN message_tags mt ON mt.message_id = m.id
             JOIN tags t ON t.id = mt.tag_id
             JOIN tag_group_members gm ON gm.tag_id = t.id
             WHERE m.account_id = ?1 AND m.from_addr = ?2 AND m.id != ?3
               AND m.ai_tagged_at IS NOT NULL AND gm.group_id = ?4
             GROUP BY t.id
             ORDER BY n DESC, t.id ASC",
        )?;
        let rows = stmt.query_map(
            params![account_id, from_addr, exclude_id, group_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 写回单封邮件的整理结果——整件事在**一个事务**里原子完成。
    ///
    /// 语义（多对多组模型下「组内单选」的定义）：
    /// 1. **用户保护**：邮件在某启用组内已有 `source='manual'` 行 → 整组跳过；
    /// 2. 解析指派（候选命中 / 创建新标签）；
    /// 3. **受限替换**：删除「属于启用组、来源为 AI、且不在本轮全部选中集合」的行——
    ///    集合级判定与顺序无关，同一标签同属多个启用组时不会互相踩踏；
    /// 4. 插入 `source='ai'`（`ON CONFLICT DO NOTHING`，manual 行永不降级）；
    /// 5. 同事务写 `ai_tagged_at`——只有提交成功才算「已整理」，失败下次自动重试。
    ///
    /// 返回写回后的权威标签集合（供事件回推前端）。
    pub fn save_classify_result(
        &self,
        account_id: &str,
        message_id: i64,
        assignments: &[GroupAssignment],
        now: &str,
    ) -> AppResult<Vec<i64>> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;

        let mut active_groups: Vec<i64> = Vec::new();
        let mut picked: Vec<i64> = Vec::new();

        for a in assignments {
            // 组可能在批处理途中被删（这几张表没有外键，必须重查防孤儿）。
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM tag_groups WHERE id = ?1)",
                params![a.group_id],
                |r| r.get(0),
            )?;
            if !exists {
                continue;
            }
            // 用户保护：组内已有手动标签 → 整组跳过（不删不写）。
            let has_manual: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM message_tags mt
                     JOIN tag_group_members gm ON gm.tag_id = mt.tag_id
                     WHERE mt.message_id = ?1 AND gm.group_id = ?2 AND mt.source = 'manual')",
                params![message_id, a.group_id],
                |r| r.get(0),
            )?;
            if has_manual {
                continue;
            }
            active_groups.push(a.group_id);
            match &a.pick {
                Some(Pick::Existing(id)) => picked.push(*id),
                Some(Pick::New(name)) => {
                    picked.push(resolve_or_create_tag(&tx, account_id, a.group_id, name, now)?);
                }
                None => {}
            }
        }

        picked.sort_unstable();
        picked.dedup();

        // 删除阶段（受限替换）。
        if !active_groups.is_empty() {
            let group_ph = vec!["?"; active_groups.len()].join(",");
            let mut sql = format!(
                "DELETE FROM message_tags
                 WHERE message_id = ?1 AND source = 'ai'
                   AND tag_id IN (SELECT tag_id FROM tag_group_members
                                  WHERE group_id IN ({group_ph}))"
            );
            let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(message_id)];
            for g in &active_groups {
                args.push(Box::new(*g));
            }
            if !picked.is_empty() {
                let pick_ph = vec!["?"; picked.len()].join(",");
                sql.push_str(&format!(" AND tag_id NOT IN ({pick_ph})"));
                for p in &picked {
                    args.push(Box::new(*p));
                }
            }
            let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
            tx.execute(&sql, refs.as_slice())?;
        }

        // 插入阶段：manual 行遇同 (message, tag) 冲突时保持 manual。
        for tag_id in &picked {
            tx.execute(
                "INSERT INTO message_tags (message_id, tag_id, source, created_at)
                 VALUES (?1, ?2, 'ai', ?3)
                 ON CONFLICT (message_id, tag_id) DO NOTHING",
                params![message_id, tag_id, now],
            )?;
        }

        tx.execute(
            "UPDATE messages SET ai_tagged_at = ?1 WHERE id = ?2",
            params![now, message_id],
        )?;
        tx.commit()?;

        self.tags_of_message(message_id)
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

/// 解析 AI 给出的新标签名：账号内已有（精确 / ASCII 大小写不敏感）→ 复用并加入本组；
/// 否则创建新标签（`created_by='ai'`、确定性配色）并自动入组。
///
/// 「命中即复用并入组」而非「冲突即放弃」：完全可逆（用户可删标签、可移出组），
/// 且能从源头压住 `steam` / `Steam` 这类同义标签漂移。
fn resolve_or_create_tag(
    tx: &rusqlite::Transaction<'_>,
    account_id: &str,
    group_id: i64,
    raw_name: &str,
    now: &str,
) -> AppResult<i64> {
    let name = raw_name.trim();
    let exact: Option<i64> = tx
        .query_row(
            "SELECT id FROM tags WHERE account_id = ?1 AND name = ?2",
            params![account_id, name],
            |r| r.get(0),
        )
        .optional()?;
    let existing = match exact {
        Some(id) => Some(id),
        // SQLite 的 lower() 只折叠 ASCII——正好覆盖「steam / Steam」这类场景。
        None => tx
            .query_row(
                "SELECT id FROM tags WHERE account_id = ?1 AND lower(name) = lower(?2)",
                params![account_id, name],
                |r| r.get(0),
            )
            .optional()?,
    };

    let tag_id = match existing {
        Some(id) => id,
        None => {
            let next_order: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM tags WHERE account_id = ?1",
                    params![account_id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            tx.execute(
                "INSERT INTO tags (account_id, name, color, sort_order, created_at, created_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'ai')",
                params![
                    account_id,
                    name,
                    crate::classify::palette_color(name),
                    next_order,
                    now
                ],
            )?;
            tx.last_insert_rowid()
        }
    };

    // 无论复用还是新建，都确保该标签属于该组（复用场景下它可能原本在别的组）。
    tx.execute(
        "INSERT INTO tag_group_members (group_id, tag_id, created_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (group_id, tag_id) DO NOTHING",
        params![group_id, tag_id, now],
    )?;
    Ok(tag_id)
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
        // 标签由 attach_tags 统一回填：查询该行的 SQL 不 JOIN 标签表，
        // 避免一封邮件带多个标签时把结果集撑成笛卡尔积。
        tag_ids: Vec::new(),
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
