//! 实时收信：为每个账号常驻一个 IMAP 监听线程。
//!
//! 策略：登录后进入 IDLE（RFC 2177），服务器有新变化（典型是新邮件到达）立刻唤醒；
//! 不支持 IDLE 的服务器（如网易 163）退化为 [`POLL_INTERVAL`] 定时轮询，
//! 效果接近但有延迟上限。
//! 发现新邮件立即落库，并向前端推送 [`EVENT_NEW_MAIL`] 事件、按设置弹系统通知。
//!
//! 线程生命周期与账号绑定：`start` 幂等（重复调用会替换旧线程），
//! `stop` 置位停止标志，线程在下一个检查点退出。断线自动退避重连，
//! 账号被删除时线程自行结束。
//!
//! 诊断：线程会把时间线（连接、取信、通知、重连）追加写入应用数据目录的
//! `pony-watch.log`，排查「新邮件到达慢」先看它。

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use chrono::Utc;
use imap::extensions::idle::WaitOutcome;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::credentials;
use crate::error::AppResult;
use crate::mail::imap_client;
use crate::models::{Account, ParsedMail};
use crate::store::Store;
use crate::AppState;

/// 监听的目标文件夹（当前产品只做收件箱）。
const FOLDER: &str = "INBOX";

/// 新邮件到达事件。前端据此刷新列表、弹应用内提示。
pub const EVENT_NEW_MAIL: &str = "mail:new";
/// 监听状态事件（`online` / `reconnecting` / `stopped`）。
pub const EVENT_WATCH_STATE: &str = "mail:watch-state";

/// IDLE 单次等待时长。超时不是错误——只是退出 IDLE 重新确认一次状态，
/// 顺便给停止标志一个生效机会；真实新邮件由服务器推送唤醒，不受此值影响。
/// 同时它也是「服务器推送丢失/延迟」时的兜底轮询间隔，不宜过长。
const IDLE_WAIT: Duration = Duration::from_secs(30);
/// 不支持 IDLE 的服务器（能力探测未命中；网易 163 实测直接拒绝 IDLE 命令）的轮询间隔。
///
/// 这是这类账号实时性的直接上限：显示延迟 ≈ 服务器把邮件放进收件箱的固有延迟
/// （163 实测 SMTP 发出后约 7.5 秒才在 IMAP 可见）+ 轮询相位（0 到该值之间）。
/// 取 15 秒是「足够快」与「不给服务器压力、避免触发风控」之间的折中；
/// 若服务器开始报错或限流，优先调大此值。
const POLL_INTERVAL: Duration = Duration::from_secs(15);
/// 单轮最多拉取的新邮件数。积压超过此数时只拉最新一批（更早的积压不再补齐，
/// 与手动同步的 limit 语义一致），把单次同步的开销限制在可控范围内。
const FETCH_LIMIT: usize = 50;
/// 首次接入（本地毫无缓存）时的静默补齐数量，与手动同步的默认值一致。
const INITIAL_FETCH_LIMIT: usize = 40;
/// 首次连接就失败（登录失败 / 网络不可达）时的重试起点。
/// 起值放长是对账号的保护：密码错误等场景下不至于高频冲击登录接口。
const BACKOFF_MIN: Duration = Duration::from_secs(10);
/// 连接曾经建立、随后中断（唤醒处理出错 / 网络抖动）时的重试起点。
/// 这类故障多为瞬时，取更短的值让实时性尽快恢复。
const BACKOFF_AFTER_DROP: Duration = Duration::from_secs(3);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// 事件中携带的新邮件摘要条数上限（通知展示用，不全量传）。
const EVENT_MAIL_SAMPLE: usize = 3;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMailItem {
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
}

/// 新邮件事件载荷。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMailEvent {
    pub account_id: String,
    pub folder: String,
    /// 本轮真正新到（本地此前不存在）的邮件数。
    pub count: usize,
    /// 最新的若干封（按 UID 倒序），供应用内提示与系统通知展示。
    pub messages: Vec<NewMailItem>,
}

/// 监听状态快照。`state` 取值：`online`（已连接）/ `reconnecting`（退避重连中）
/// / `stopped`（已停止）。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchStateEvent {
    pub account_id: String,
    pub state: String,
    /// 重连中时的原因（在线/停止时为空串）。
    pub detail: String,
}

/// 启动（或重启）某账号的监听线程。幂等：已有线程会先被停掉。
pub fn start(app: &AppHandle, account_id: String) {
    stop(app, &account_id);

    let flag = Arc::new(AtomicBool::new(false));
    {
        let state = app.state::<AppState>();
        state
            .watchers
            .lock()
            .expect("watchers 锁中毒")
            .insert(account_id.clone(), flag.clone());
    }

    let app_for_thread = app.clone();
    let id_for_thread = account_id.clone();
    let flag_for_thread = flag.clone();
    let spawned = thread::Builder::new()
        .name(format!("pony-watch-{account_id}"))
        .spawn(move || {
            run(app_for_thread.clone(), id_for_thread.clone(), flag_for_thread.clone());
            withdraw(&app_for_thread, &id_for_thread, &flag_for_thread);
        });

    if let Err(e) = spawned {
        // 线程起不来（极端资源情况）：撤下注册，如实提示；手动同步不受影响。
        let state = app.state::<AppState>();
        state
            .watchers
            .lock()
            .expect("watchers 锁中毒")
            .remove(&account_id);
        set_state(app, &account_id, "stopped", &format!("监听线程启动失败：{e}"));
    }
}

/// 停止某账号的监听。幂等；线程最多在 [`IDLE_WAIT`] 后退出。
pub fn stop(app: &AppHandle, account_id: &str) {
    let removed = {
        let state = app.state::<AppState>();
        let removed = state
            .watchers
            .lock()
            .expect("watchers 锁中毒")
            .remove(account_id);
        removed
    };
    if let Some(flag) = removed {
        flag.store(true, Ordering::Relaxed);
        set_state(app, account_id, "stopped", "");
    }
}

/// 为当前已登记的全部账号启动监听（应用启动时调用）。
pub fn start_all(app: &AppHandle) {
    let accounts = {
        let state = app.state::<AppState>();
        state.store.list_accounts().unwrap_or_default()
    };
    for account in accounts {
        start(app, account.id);
    }
}

/// 线程退出时的注册回收：只有当注册表里的标志仍是我们自己的（未被新线程替换）
/// 时才广播 stopped，避免「重启监听」时旧线程的迟到退出覆盖新线程的状态。
fn withdraw(app: &AppHandle, account_id: &str, mine: &Arc<AtomicBool>) {
    let still_mine = {
        let state = app.state::<AppState>();
        let mut map = state.watchers.lock().expect("watchers 锁中毒");
        match map.get(account_id) {
            Some(f) if Arc::ptr_eq(f, mine) => {
                map.remove(account_id);
                true
            }
            _ => false,
        }
    };
    if still_mine {
        set_state(app, account_id, "stopped", "");
    }
}

/// 更新监听状态：写入快照表（供 `watch_states` 命令查询）并向前端广播。
fn set_state(app: &AppHandle, account_id: &str, state: &str, detail: &str) {
    let event = WatchStateEvent {
        account_id: account_id.to_string(),
        state: state.to_string(),
        detail: detail.to_string(),
    };
    {
        let st = app.state::<AppState>();
        st.watch_states
            .lock()
            .expect("watch_states 锁中毒")
            .insert(account_id.to_string(), event.clone());
    }
    let _ = app.emit(EVENT_WATCH_STATE, event);
}

/// 监听线程主体：取账号凭据 → 建立会话 → 循环监听，全程退避重连。
fn run(app: AppHandle, account_id: String, stop: Arc<AtomicBool>) {
    let store = {
        let state = app.state::<AppState>();
        state.store.clone()
    };
    let mut backoff = BACKOFF_MIN;
    watch_log(&app, &format!("监听线程启动（账号 {account_id}）"));

    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        // 账号已被删除 → 线程结束（注销由 withdraw 负责）。
        let Ok(account) = store.get_account(&account_id) else {
            return;
        };
        let password = match credentials::get_password(&account_id) {
            Ok(p) => p,
            Err(e) => {
                watch_log(&app, &format!("读取密码失败：{e}；{backoff:?} 后重试"));
                set_state(&app, &account_id, "reconnecting", &format!("读取密码失败：{e}"));
                if sleep_or_stop(&stop, backoff) {
                    return;
                }
                backoff = grow(backoff);
                continue;
            }
        };

        match watch_once(&app, &store, &account, &password, &stop, &mut backoff) {
            // 收到停止请求，正常退出。
            Ok(()) => return,
            Err(e) => {
                // 错误可能正是「停止」引发的（如账号刚被删除导致写入失败）：
                // 此时不再广播 reconnecting，避免覆盖刚发出的 stopped。
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                watch_log(&app, &format!("监听中断：{e}；{backoff:?} 后重连"));
                set_state(&app, &account_id, "reconnecting", &e.to_string());
                if sleep_or_stop(&stop, backoff) {
                    return;
                }
                backoff = grow(backoff);
            }
        }
    }
}

/// 建立一次连接并监听，直到出错或收到停止请求。
///
/// 返回 `Ok(())` 表示收到停止请求（正常退出）；`Err` 表示连接层失败，
/// 由调用方退避后重连。
fn watch_once(
    app: &AppHandle,
    store: &Store,
    account: &Account,
    password: &str,
    stop: &AtomicBool,
    backoff: &mut Duration,
) -> AppResult<()> {
    let mut session = imap_client::connect(account, password)?;
    imap_client::select_folder(&mut session, FOLDER)?;
    // 能力探测失败不致命：保守按「不支持 IDLE」处理，走轮询。
    let idle_supported = session
        .capabilities()
        .map(|caps| caps.has_str("IDLE"))
        .unwrap_or(false);
    watch_log(app, &format!("已连接（IDLE={idle_supported}）"));
    set_state(app, &account.id, "online", "");

    // 首次接入（本地没有任何缓存）做一次静默补齐：只拉最近一批，不当「新邮件」
    // 通知——否则刚添加账号就会被几十条历史邮件的通知吓一跳。
    let initial =
        store.last_uid(&account.id, FOLDER)? == 0 && store.count_messages(&account.id, FOLDER)? == 0;
    let limit = if initial { INITIAL_FETCH_LIMIT } else { FETCH_LIMIT };
    let t0 = std::time::Instant::now();
    let fresh = poll_new(store, &mut session, &account.id, limit)?;
    watch_log(
        app,
        &format!("首轮检查完成（{:?}）：{} 封新邮件", t0.elapsed(), fresh.len()),
    );
    // 首轮取信成功才算「健康连接」：退避回到短档。若失败发生在更早的建立阶段
    // （反复登录失败等），退避继续按首次连接的长档增长，避免高频冲击登录接口。
    *backoff = BACKOFF_AFTER_DROP;
    if !initial && !fresh.is_empty() {
        deliver(app, store, account, fresh);
    }

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if idle_supported {
            // 进入 IDLE 等待服务器推送变化；超时或收到变化都会返回。
            // Handle 在 wait_with_timeout 内部完成 DONE 收尾，随后可继续用本会话。
            match session.idle() {
                Ok(handle) => {
                    let t = std::time::Instant::now();
                    let outcome = handle.wait_with_timeout(IDLE_WAIT)?;
                    if outcome == WaitOutcome::MailboxChanged {
                        watch_log(
                            app,
                            &format!("IDLE 被服务器推送唤醒（等待 {:.1}s）", t.elapsed().as_secs_f32()),
                        );
                    }
                }
                Err(e) => {
                    // 能力探测与真实行为不符（如服务器拒绝 IDLE）：本会话退回轮询，
                    // 而不是让整条连接作废——连接若其实已坏，下一轮检查自会报错重连。
                    watch_log(app, &format!("进入 IDLE 失败，本会话退回轮询：{e}"));
                    if sleep_or_stop(stop, POLL_INTERVAL) {
                        break;
                    }
                }
            }
        } else if sleep_or_stop(stop, POLL_INTERVAL) {
            break;
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let t0 = std::time::Instant::now();
        let fresh = poll_new(store, &mut session, &account.id, FETCH_LIMIT)?;
        if !fresh.is_empty() {
            watch_log(
                app,
                &format!(
                    "检查到 {} 封新邮件（SEARCH+FETCH 用时 {:?}）",
                    fresh.len(),
                    t0.elapsed()
                ),
            );
            deliver(app, store, account, fresh);
        }
    }

    imap_client::logout(session);
    Ok(())
}

/// 拉取同步游标之后的新邮件并落库；返回其中**本地此前不存在**的部分
/// （通知只认这些——手动同步与监听可能并发处理同一批 UID，upsert 幂等，
/// 但通知不能重复弹）。
fn poll_new(
    store: &Store,
    session: &mut imap_client::ImapSession,
    account_id: &str,
    limit: usize,
) -> AppResult<Vec<ParsedMail>> {
    let since = store.last_uid(account_id, FOLDER)?;
    // 服务端过滤：只让服务器返回游标之后的 UID，避免大邮箱全量传输。
    let hits = session.uid_search(format!("UID {}:*", since.saturating_add(1)))?;
    let uids = select_targets(hits.into_iter().collect(), since, limit);
    if uids.is_empty() {
        return Ok(Vec::new());
    }

    let mails = imap_client::fetch_headers(session, &uids)?;
    let now = Utc::now().to_rfc3339();

    // 落库前先判定「本地是否已有」——这决定哪些邮件算真正新到（用于通知）。
    let mut fresh = Vec::new();
    for m in &mails {
        if store.find_id_by_uid(account_id, FOLDER, m.uid)?.is_none() {
            fresh.push(m.clone());
        }
    }

    store.upsert_folder(account_id, FOLDER)?;
    store.upsert_headers(account_id, FOLDER, &mails, &now)?;
    // 游标只推进到「实际解析成功」的最大 UID，而不是请求的最大 UID：
    // 某些服务器对新到邮件存在短暂不可取（SEARCH 已可见、FETCH 暂拿不到
    // header）。若按请求推进，这封邮件会被游标永久跳过；不推进则下一轮
    // 自动重试直到取到（若邮件已不存在，RFC 3501 的 `n:*` 包含最后一封的
    // 特性由上方 `uid > since` 过滤兜底，不会造成空转取信）。
    if let Some(max) = mails.iter().map(|m| m.uid).max() {
        store.set_last_uid(account_id, FOLDER, max, &now)?;
    }
    // 同步文件夹计数（失败不影响主流程，与手动同步的处理一致）。
    let _ = store.refresh_folder_count(account_id, FOLDER);

    fresh.sort_by_key(|m| m.uid);
    Ok(fresh)
}

/// 从 `UID SEARCH UID n:*` 的搜索结果里选出要拉取的 UID：
/// 剔除不高于游标的（RFC 3501 规定 `n:*` 在 n 超过最大 UID 时仍包含最后一封），
/// 再取最新的 `limit` 个，保持升序。
fn select_targets(mut uids: Vec<u32>, since: u32, limit: usize) -> Vec<u32> {
    uids.retain(|u| *u > since);
    uids.sort_unstable();
    if uids.len() > limit {
        uids = uids.split_off(uids.len() - limit);
    }
    uids
}

/// 新邮件到达的对外动作：推事件给前端 + 按设置弹系统通知。
fn deliver(app: &AppHandle, store: &Store, account: &Account, fresh: Vec<ParsedMail>) {
    let messages = fresh
        .iter()
        .rev()
        .take(EVENT_MAIL_SAMPLE)
        .map(|m| NewMailItem {
            subject: m.subject.clone(),
            from_name: m.from_name.clone(),
            from_addr: m.from_addr.clone(),
        })
        .collect();
    let _ = app.emit(
        EVENT_NEW_MAIL,
        NewMailEvent {
            account_id: account.id.clone(),
            folder: FOLDER.to_string(),
            count: fresh.len(),
            messages,
        },
    );
    let newest_desc = fresh
        .iter()
        .max_by_key(|m| m.uid)
        .map(|m| {
            let s = m.subject.trim();
            if s.is_empty() {
                "（无主题）".to_string()
            } else {
                s.to_string()
            }
        })
        .unwrap_or_default();
    watch_log(
        app,
        &format!("新邮件事件已推送：{} 封（最新：{newest_desc}）", fresh.len()),
    );

    // 系统通知开关即时生效：每次新邮件都重新读设置。
    let enabled = store
        .mail_settings()
        .map(|s| s.notify_new_mail)
        .unwrap_or(true);
    if !enabled {
        watch_log(app, "系统通知已按设置关闭，仅应用内提示");
        return;
    }
    let Some(newest) = fresh.iter().max_by_key(|m| m.uid) else {
        return;
    };
    let (title, body) = notification_text(&account.email, newest, fresh.len());
    // 通知失败（如系统未允许）不视为错误：应用内提示仍然可见。
    match app.notification().builder().title(title).body(body).show() {
        Ok(()) => watch_log(app, "系统通知已弹出"),
        Err(e) => watch_log(app, &format!("系统通知弹出失败：{e}")),
    }
}

/// 组装系统通知文案（纯函数，便于单测）。
fn notification_text(account_email: &str, newest: &ParsedMail, count: usize) -> (String, String) {
    let from = if newest.from_name.trim().is_empty() {
        newest.from_addr.as_str()
    } else {
        newest.from_name.trim()
    };
    let subject = if newest.subject.trim().is_empty() {
        "（无主题）"
    } else {
        newest.subject.trim()
    };
    if count == 1 {
        (subject.to_string(), format!("{from} · {account_email}"))
    } else {
        (
            format!("收到 {count} 封新邮件"),
            format!("{from}：{subject} · {account_email}"),
        )
    }
}

/// 可被停止标志打断的分段睡眠；返回 true 表示收到停止请求。
fn sleep_or_stop(stop: &AtomicBool, total: Duration) -> bool {
    const STEP: Duration = Duration::from_millis(250);
    let mut left = total;
    while left > Duration::ZERO {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        let chunk = left.min(STEP);
        thread::sleep(chunk);
        left = left.saturating_sub(chunk);
    }
    stop.load(Ordering::Relaxed)
}

fn grow(backoff: Duration) -> Duration {
    (backoff * 2).min(BACKOFF_MAX)
}

/// 监听线程的常驻日志：追加写入应用数据目录的 `pony-watch.log`（与 pony.db 同目录，
/// 即 `%APPDATA%\com.skyline.ponymail\`）。文件超过 2MB 时在进程首次写入时清卷重建。
///
/// 排查「新邮件到达慢」这类问题的核心时间线就在这里——连接建立/中断、取信、
/// 通知与重连退避，全部带毫秒时间戳。开 `PONY_IMAP_DEBUG` 时同一内容也输出到
/// stderr，并同时开启 vendored imap 的逐行协议日志（S:/C:，同样带时间戳），
/// 便于把延迟精确归因到某个协议环节。
fn watch_log(app: &AppHandle, msg: &str) {
    let stamp = chrono::Local::now().format("%m-%d %H:%M:%S%.3f");
    if std::env::var_os("PONY_IMAP_DEBUG").is_some() {
        eprintln!("[watch {stamp}] {msg}");
    }
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    let path = dir.join("pony-watch.log");
    // 体积检查每个进程只做一次，避免每次写日志都 stat。
    static SIZE_CHECKED: AtomicBool = AtomicBool::new(false);
    if !SIZE_CHECKED.swap(true, Ordering::Relaxed) {
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > 2 * 1024 * 1024 {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[watch {stamp}] {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ParsedMail;

    fn mail(uid: u32, from_name: &str, from_addr: &str, subject: &str) -> ParsedMail {
        ParsedMail {
            uid,
            from_name: from_name.to_string(),
            from_addr: from_addr.to_string(),
            subject: subject.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn select_targets_filters_and_caps_to_latest() {
        // 游标 100；服务器返回 99、100（不足，应剔除）、101..=105（应保留最新 3 个）。
        let uids = vec![99, 100, 101, 102, 103, 104, 105];
        assert_eq!(select_targets(uids, 100, 3), vec![103, 104, 105]);
    }

    #[test]
    fn select_targets_rfc_gotcha_last_uid_included() {
        // RFC 3501：`UID n:*` 在 n 超过最大 UID 时仍返回最后一封——
        // 客户端必须自己按 `uid > since` 过滤，避免把旧邮件误判为新邮件。
        let uids = vec![42];
        assert!(select_targets(uids, 42, 50).is_empty());
    }

    #[test]
    fn select_targets_sorts_ascending_for_sequence_set() {
        // 保证升序：`fetch_headers` 用它对 UID 压缩成 sequence set。
        let uids = vec![30, 12, 25];
        assert_eq!(select_targets(uids, 10, 10), vec![12, 25, 30]);
    }

    #[test]
    fn notification_text_single_mail_uses_subject_as_title() {
        let m = mail(1, "张三", "z@example.com", "会议纪要");
        let (title, body) = notification_text("me@example.com", &m, 1);
        assert_eq!(title, "会议纪要");
        assert_eq!(body, "张三 · me@example.com");
    }

    #[test]
    fn notification_text_multiple_mails_summarizes_with_newest() {
        let m = mail(9, "李四", "l@example.com", "报价单");
        let (title, body) = notification_text("me@example.com", &m, 4);
        assert_eq!(title, "收到 4 封新邮件");
        assert!(body.contains("李四"), "body 应包含最新一封发件人：{body}");
        assert!(body.contains("报价单"), "body 应包含最新一封主题：{body}");
    }

    #[test]
    fn notification_text_falls_back_for_empty_fields() {
        let m = mail(1, "", "anon@example.com", "");
        let (title, body) = notification_text("me@example.com", &m, 1);
        assert_eq!(title, "（无主题）");
        assert_eq!(body, "anon@example.com · me@example.com");
    }

    #[test]
    fn grow_doubles_and_caps_at_max() {
        assert_eq!(grow(BACKOFF_MIN), BACKOFF_MIN * 2);
        assert_eq!(grow(BACKOFF_MAX), BACKOFF_MAX);
        assert_eq!(grow(Duration::from_secs(200)), BACKOFF_MAX);
    }
}
