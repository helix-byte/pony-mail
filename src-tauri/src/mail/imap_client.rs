use std::net::TcpStream;

use imap::types::Flag;
use native_tls::{TlsConnector, TlsStream};

use crate::error::{AppError, AppResult};
use crate::models::{Account, ParsedMail};

use super::parser;

pub type ImapSession = imap::Session<TlsStream<TcpStream>>;

/// 建立 IMAP 连接并登录。当前仅支持隐式 TLS（993 / 143+STARTTLS 由服务器自动升级）。
pub fn connect(account: &Account, password: &str) -> AppResult<ImapSession> {
    let tls = TlsConnector::builder().build()?;
    let client = imap::connect(
        (account.imap_host.as_str(), account.imap_port),
        account.imap_host.as_str(),
        &tls,
    )?;
    let mut session = client
        .login(&account.username, password)
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
