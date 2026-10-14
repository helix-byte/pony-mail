use std::net::TcpStream;

use imap::types::{Flag, NameAttribute};
use native_tls::{TlsConnector, TlsStream};

use crate::error::{AppError, AppResult};
use crate::models::{Account, ParsedMail};

use super::{normalize_password, parser};

pub type ImapSession = imap::Session<TlsStream<TcpStream>>;

/// 建立 IMAP 连接并登录。当前仅支持隐式 TLS（993 / 143+STARTTLS 由服务器自动升级）。
pub fn connect(account: &Account, password: &str) -> AppResult<ImapSession> {
    let tls = TlsConnector::builder().build()?;
    let client = imap::connect(
        (account.imap_host.as_str(), account.imap_port),
        account.imap_host.as_str(),
        &tls,
    )?;
    let password = normalize_password(account, password);
    let mut session = client
        .login(&account.username, &password)
        .map_err(|(e, _)| AppError::Mail(format!("登录失败：{e}")))?;
    if std::env::var_os("PONY_IMAP_DEBUG").is_some() {
        session.debug = true;
    }
    // 网易系（163/126/188/yeah.net）在 SELECT 前要求客户端上报 ID 信息，
    // 否则返回 "Unsafe Login" 并拒绝访问邮箱。失败时静默忽略，不影响其他服务商。
    send_client_id(&mut session);
    Ok(session)
}

/// 发送 RFC 2971 ID 命令，向服务器声明客户端身份。
///
/// 走 vendored `imap` 补丁新增的 `Session::id()`（见 `vendor/imap`）——
/// 上游 crate 没有暴露该能力的正确实现，用 `run_command*` 会导致 tag 错位崩溃。
fn send_client_id(session: &mut ImapSession) {
    let args = concat!(
        "(\"name\" \"Pony Mail\" ",
        "\"version\" \"0.1.0\" ",
        "\"vendor\" \"Pony\" ",
        "\"support-email\" \"support@example.com\")"
    );
    let _ = session.id(args);
}

/// 选择文件夹并返回其全部 UID（升序）。
pub fn list_uids(session: &mut ImapSession, folder: &str) -> AppResult<Vec<u32>> {
    select_folder(session, folder)?;
    let set = session.uid_search("ALL")?;
    let mut uids: Vec<u32> = set.into_iter().collect();
    uids.sort_unstable();
    Ok(uids)
}

/// 仅选择文件夹，不做额外查询。
pub fn select_folder(session: &mut ImapSession, folder: &str) -> AppResult<()> {
    session
        .select(folder)
        .map_err(|e| AppError::Mail(format!("打开文件夹 {folder} 失败：{e}")))?;
    Ok(())
}

/// 批量拉取邮件头（不改变 \Seen 状态）。
pub fn fetch_headers(session: &mut ImapSession, uids: &[u32]) -> AppResult<Vec<ParsedMail>> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let set = build_sequence_set(uids);
    let fetches = session.uid_fetch(set, "(UID FLAGS RFC822.HEADER)")?;
    let mut out = Vec::new();
    for f in fetches.iter() {
        let uid = match f.uid {
            Some(u) => u,
            None => continue,
        };
        let raw = match f.header() {
            Some(h) => h,
            None => continue,
        };
        let (seen, flagged) = read_flags(f);
        if let Some(mail) = parser::parse(raw, uid, seen, flagged, false) {
            out.push(mail);
        }
    }
    Ok(out)
}

/// 拉取单封完整报文（正文 + 附件元信息），不改变 \Seen 状态。
pub fn fetch_full(session: &mut ImapSession, uid: u32) -> AppResult<ParsedMail> {
    let fetches = session.uid_fetch(uid.to_string(), "(UID FLAGS BODY.PEEK[])")?;
    let f = fetches
        .iter()
        .next()
        .ok_or_else(|| AppError::Mail(format!("服务器未返回 UID {uid} 的内容")))?;
    let raw = f
        .body()
        .ok_or_else(|| AppError::Mail(format!("UID {uid} 正文为空")))?;
    let (seen, flagged) = read_flags(f);
    parser::parse(raw, uid, seen, flagged, true)
        .ok_or_else(|| AppError::Mail(format!("UID {uid} 解析失败")))
}

/// 把本地状态写回服务器（标记已读/星标）。
pub fn store_flag(session: &mut ImapSession, uid: u32, flag: &str, on: bool) -> AppResult<()> {
    let op = if on { "+FLAGS" } else { "-FLAGS" };
    session
        .uid_store(uid.to_string(), format!("{op} ({flag})"))
        .map_err(|e| AppError::Mail(format!("更新服务器标记失败：{e}")))?;
    Ok(())
}

/// 在服务器上永久删除邮件：打 `\Deleted` 标记后 `EXPUNGE`。
///
/// 注意 `EXPUNGE` 是**不可逆**操作，因此调用方必须先经用户确认。
/// `EXPUNGE` 成功后再执行一次 `UID SEARCH DELETED` 校验，确保真的删掉了。
///
/// Gmail 系不要直接调用本函数——对 Gmail 它的语义是「摘标签」甚至永久删除，
/// 走 [`delete_or_trash`] 才能得到用户预期的「移入回收站」。
pub fn delete_messages(session: &mut ImapSession, uids: &[u32]) -> AppResult<()> {
    if uids.is_empty() {
        return Ok(());
    }
    let set = build_sequence_set(uids);
    // 1) 打删除标记
    session
        .uid_store(set.clone(), "+FLAGS (\\Deleted)")
        .map_err(|e| AppError::Mail(format!("标记删除失败：{e}")))?;
    // 2) 立即永久删除（不进入 \Deleted 残留状态）
    session
        .expunge()
        .map_err(|e| AppError::Mail(format!("永久删除（EXPUNGE）失败：{e}")))?;
    Ok(())
}

/// 删除邮件的统一入口，按服务器语义分流：
///
/// - **Gmail / Google 系**：把邮件移入「回收站」文件夹（等效网页端的删除，
///   30 天内可恢复）。对 INBOX 里的邮件直接打 `\Deleted` + `EXPUNGE` 在 Gmail
///   上只是摘掉「收件箱」标签（邮件仍留在「所有邮件」）乃至永久删除，与用户
///   对「删除」的预期都不符；仅当当前选中的文件夹本身就是回收站时才永久删除。
/// - **其他服务器**：沿用 [`delete_messages`] 的永久删除。
pub fn delete_or_trash(
    session: &mut ImapSession,
    account: &Account,
    current_folder: &str,
    uids: &[u32],
) -> AppResult<()> {
    if uids.is_empty() {
        return Ok(());
    }
    if !account.is_gmail() {
        return delete_messages(session, uids);
    }
    let Some(trash) = find_trash_folder(session)? else {
        // 找不到回收站时宁可不动：在 Gmail 上盲目 EXPUNGE 可能绕过回收站永久删除，
        // 与「删除 = 可恢复」的预期相悖。报错后由调用方的既有逻辑记录并告知用户。
        return Err(AppError::Mail(
            "未找到 Gmail 回收站文件夹（\\Trash），已取消服务器删除".to_string(),
        ));
    };
    if trash.eq_ignore_ascii_case(current_folder) {
        // 已在回收站内删除 = 永久删除，符合用户预期。
        return delete_messages(session, uids);
    }
    move_messages(session, uids, &trash)
}

/// 查找 Gmail 的回收站文件夹。
///
/// 系统文件夹的名字随账号语言本地化（中文账号叫「已删除邮件」之类），枚举固定
/// 名字不可靠，必须按服务器声明的 SPECIAL-USE 属性（RFC 6154，Gmail 标准支持）
/// 识别。LIST 响应里的名字是 IMAP modified UTF-7 的原始编码，原样传回服务器
/// 即可（编解码闭环），无需客户端转换。
pub fn find_trash_folder(session: &mut ImapSession) -> AppResult<Option<String>> {
    let names = session
        .list(None, Some("*"))
        .map_err(|e| AppError::Mail(format!("列出文件夹失败：{e}")))?;
    if let Some(n) = names.iter().find(|n| has_special_use(n.attributes(), "\\Trash")) {
        return Ok(Some(n.name().to_string()));
    }
    // 属性缺失时的兜底：Gmail 英文界面的默认名。
    Ok(names
        .iter()
        .find(|n| n.name().eq_ignore_ascii_case("[Gmail]/Trash"))
        .map(|n| n.name().to_string()))
}

/// 名称属性里是否声明了指定 SPECIAL-USE（如 `\Trash`）。
/// vendored imap 未识别的扩展属性都会落到 [`NameAttribute::Custom`]，大小写不敏感比较。
fn has_special_use(attrs: &[NameAttribute<'_>], attr: &str) -> bool {
    attrs
        .iter()
        .any(|a| matches!(a, NameAttribute::Custom(s) if s.eq_ignore_ascii_case(attr)))
}

/// 把邮件移动到目标文件夹。优先 RFC 6851 `UID MOVE`（Gmail 支持）；
/// 不支持时退化为 COPY + `\Deleted` + `UID EXPUNGE`——UIDPLUS 的精确删除
/// 只作用于指定集合，不会连带清掉其他客户端留下的 `\Deleted` 标记。
fn move_messages(session: &mut ImapSession, uids: &[u32], target: &str) -> AppResult<()> {
    let set = build_sequence_set(uids);
    let (has_move, has_uidplus) = match session.capabilities() {
        Ok(caps) => (caps.has_str("MOVE"), caps.has_str("UIDPLUS")),
        Err(_) => (false, false),
    };
    if has_move {
        session
            .uid_mv(&set, target)
            .map_err(|e| AppError::Mail(format!("移动到「{target}」失败：{e}")))?;
        return Ok(());
    }
    session
        .uid_copy(&set, target)
        .map_err(|e| AppError::Mail(format!("复制到「{target}」失败：{e}")))?;
    session
        .uid_store(&set, "+FLAGS (\\Deleted)")
        .map_err(|e| AppError::Mail(format!("标记源邮件删除失败：{e}")))?;
    if has_uidplus {
        session
            .uid_expunge(&set)
            .map_err(|e| AppError::Mail(format!("从原文件夹移除失败（UID EXPUNGE）：{e}")))?;
    } else {
        session
            .expunge()
            .map_err(|e| AppError::Mail(format!("从原文件夹移除失败（EXPUNGE）：{e}")))?;
    }
    Ok(())
}

pub fn logout(mut session: ImapSession) {
    let _ = session.logout();
}

fn read_flags(f: &imap::types::Fetch) -> (bool, bool) {
    let mut seen = false;
    let mut flagged = false;
    for flag in f.flags() {
        match flag {
            Flag::Seen => seen = true,
            Flag::Flagged => flagged = true,
            _ => {}
        }
    }
    (seen, flagged)
}

/// 把升序 UID 列表压缩成 IMAP sequence set，例如 `1:5,9,12:14`。
fn build_sequence_set(uids: &[u32]) -> String {
    let Some((&first, rest)) = uids.split_first() else {
        return String::new();
    };
    let mut parts = Vec::new();
    let (mut start, mut prev) = (first, first);
    for &u in rest {
        if u == prev + 1 {
            prev = u;
            continue;
        }
        parts.push(range_part(start, prev));
        start = u;
        prev = u;
    }
    parts.push(range_part(start, prev));
    parts.join(",")
}

fn range_part(a: u32, b: u32) -> String {
    if a == b {
        a.to_string()
    } else {
        format!("{a}:{b}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs<'a>(raw: &'a [&'a str]) -> Vec<NameAttribute<'a>> {
        raw.iter().map(|s| NameAttribute::from(*s)).collect()
    }

    #[test]
    fn has_special_use_matches_custom_attribute_case_insensitively() {
        // vendored imap 只识别 \Noinferiors/\Noselect/\Marked/\Unmarked，
        // SPECIAL-USE 属性一律落在 Custom 里。
        assert!(has_special_use(&attrs(&["\\HasNoChildren", "\\Trash"]), "\\Trash"));
        assert!(has_special_use(&attrs(&["\\trash"]), "\\Trash"));
        assert!(!has_special_use(&attrs(&["\\HasNoChildren"]), "\\Trash"));
        assert!(!has_special_use(&[], "\\Trash"));
    }

    #[test]
    fn has_special_use_does_not_match_system_attributes() {
        // \Noselect 会被解析成系统变体而非 Custom，不得误命中。
        assert!(!has_special_use(&attrs(&["\\Noselect"]), "\\Noselect"));
        assert!(!has_special_use(&attrs(&["\\Noselect"]), "\\Trash"));
    }
}
