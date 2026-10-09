use chrono::Utc;
use tauri::State;
use uuid::Uuid;

use crate::ai;
use crate::credentials;
use crate::error::{AppError, AppResult};
use crate::mail::{imap_client, smtp_client};
use crate::models::*;
use crate::AppState;

// ---------------------------------------------------------------------
// 账号
// ---------------------------------------------------------------------

#[tauri::command]
pub fn list_accounts(state: State<'_, AppState>) -> AppResult<Vec<Account>> {
    state.store.list_accounts()
}

#[tauri::command]
pub fn add_account(state: State<'_, AppState>, input: AccountInput) -> AppResult<Account> {
    let id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();

    // 先写凭据再写库，任一步失败都清理，避免出现「有账号无密码」的脏状态
    credentials::save_password(&id, &input.password)?;
    match state.store.insert_account(&id, &input, &created_at) {
        Ok(account) => Ok(account),
        Err(e) => {
            let _ = credentials::delete_password(&id);
            Err(e)
        }
    }
}

#[tauri::command]
pub fn delete_account(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.store.delete_account(&id)?;
    credentials::delete_password(&id)?;
    Ok(())
}

#[tauri::command]
pub fn set_account_password(id: String, password: String) -> AppResult<()> {
    credentials::save_password(&id, &password)
}

// ---------------------------------------------------------------------
// 连接测试
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn test_connection(input: AccountInput) -> AppResult<TestReport> {
    tauri::async_runtime::spawn_blocking(move || {
        let account = account_from_input(&input, "test", "");
        let mut report = TestReport {
            imap_ok: false,
            smtp_ok: false,
            imap_message: String::new(),
            smtp_message: String::new(),
        };

        match imap_client::connect(&account, &input.password) {
            Ok(mut session) => match imap_client::list_uids(&mut session, "INBOX") {
                Ok(uids) => {
                    report.imap_ok = true;
                    report.imap_message = format!("IMAP 登录成功，收件箱共 {} 封邮件", uids.len());
                }
                Err(e) => report.imap_message = format!("IMAP 打开文件夹失败：{e}"),
            },
            Err(e) => report.imap_message = format!("IMAP 登录失败：{e}"),
        }

        match smtp_client::build_transport(&account, &input.password) {
            Ok(transport) => match transport.test_connection() {
                Ok(true) => {
                    report.smtp_ok = true;
                    report.smtp_message = "SMTP 连接与认证成功".to_string();
                }
                Ok(false) => report.smtp_message = "SMTP 已连接但认证被拒绝".to_string(),
                Err(e) => report.smtp_message = e.to_string(),
            },
            Err(e) => report.smtp_message = e.to_string(),
        }

        Ok(report)
    })
    .await
    .map_err(|e| AppError::Other(format!("连接测试任务异常：{e}")))?
}

// ---------------------------------------------------------------------
// 同步与读取
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn sync_inbox(
    state: State<'_, AppState>,
    account_id: String,
    folder: String,
    limit: u32,
) -> AppResult<SyncReport> {
    let account = state.store.get_account(&account_id)?;
    let password = credentials::get_password(&account_id)?;
    let store = state.store.clone();
    let folder = if folder.trim().is_empty() {
        "INBOX".to_string()
    } else {
        folder
    };
    let limit = limit.clamp(1, 200);

    tauri::async_runtime::spawn_blocking(move || sync_folder(&store, &account, &password, &folder, limit))
        .await
        .map_err(|e| AppError::Other(format!("同步任务异常：{e}")))?
}

fn sync_folder(
    store: &crate::store::Store,
    account: &Account,
    password: &str,
    folder: &str,
    limit: u32,
) -> AppResult<SyncReport> {
    let mut session = imap_client::connect(account, password)?;
    let uids = imap_client::list_uids(&mut session, folder)?;
    let total_on_server = uids.len();

    let since = store.last_uid(&account.id, folder)?;
    let mut targets: Vec<u32> = uids.into_iter().filter(|u| *u > since).collect();
    if targets.len() > limit as usize {
        targets = targets.split_off(targets.len() - limit as usize);
    }

    let now = Utc::now().to_rfc3339();
    let mails = if targets.is_empty() {
        Vec::new()
    } else {
        imap_client::fetch_headers(&mut session, &targets)?
    };
    imap_client::logout(session);

    store.upsert_folder(&account.id, folder)?;
    let inserted = store.upsert_headers(&account.id, folder, &mails, &now)?;

    if let Some(max_uid) = targets.iter().copied().max() {
        store.set_last_uid(&account.id, folder, max_uid, &now)?;
    }

    Ok(SyncReport {
        folder: folder.to_string(),
        fetched: mails.len(),
        inserted,
        total_on_server,
    })
}

#[tauri::command]
pub fn list_messages(
    state: State<'_, AppState>,
    account_id: String,
    folder: String,
    limit: u32,
    offset: u32,
) -> AppResult<Vec<MessageHeader>> {
    state
        .store
        .list_messages(&account_id, &folder, limit.clamp(1, 500), offset)
}

/// 读取邮件详情；正文未缓存时按需从服务器拉取并落库。
#[tauri::command]
pub async fn load_message_body(state: State<'_, AppState>, id: i64) -> AppResult<MessageDetail> {
    let detail = state.store.get_message(id)?;
    if detail.header.body_loaded {
        return Ok(detail);
    }

    let (account_id, folder, uid) = state.store.message_route(id)?;
    let account = state.store.get_account(&account_id)?;
    let password = credentials::get_password(&account_id)?;
    let store = state.store.clone();

    tauri::async_runtime::spawn_blocking(move || -> AppResult<()> {
        let mut session = imap_client::connect(&account, &password)?;
        imap_client::select_folder(&mut session, &folder)?;
        let mail = imap_client::fetch_full(&mut session, uid)?;
        store.save_body(&account.id, &folder, &mail, &Utc::now().to_rfc3339())?;
        imap_client::logout(session);
        Ok(())
    })
    .await
    .map_err(|e| AppError::Other(format!("拉取正文任务异常：{e}")))??;

    state.store.get_message(id)
}

#[tauri::command]
pub async fn set_seen(state: State<'_, AppState>, id: i64, seen: bool) -> AppResult<()> {
    state.store.set_seen(id, seen)?;
    push_flag(state, id, "\\Seen", seen).await;
    Ok(())
}

#[tauri::command]
pub async fn set_flagged(state: State<'_, AppState>, id: i64, flagged: bool) -> AppResult<()> {
    state.store.set_flagged(id, flagged)?;
    push_flag(state, id, "\\Flagged", flagged).await;
    Ok(())
}

/// 把本地标记异步回写 IMAP 服务器。失败不阻塞界面，仅忽略。
async fn push_flag(state: State<'_, AppState>, id: i64, flag: &'static str, on: bool) {
    let Ok((account_id, folder, uid)) = state.store.message_route(id) else {
        return;
    };
    let store = state.store.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let (Ok(account), Ok(password)) = (
            store.get_account(&account_id),
            credentials::get_password(&account_id),
        ) else {
            return;
        };
        if let Ok(mut session) = imap_client::connect(&account, &password) {
            let _ = imap_client::select_folder(&mut session, &folder);
            let _ = imap_client::store_flag(&mut session, uid, flag, on);
            imap_client::logout(session);
        }
    })
    .await;
}

/// 删除邮件：先在服务器永久删除，再清理本地缓存。
///
/// 设计上「服务器失败不阻断本地清理」——否则用户会陷入「删不掉又看不到」的困境。
/// 但会用 [`DeleteReport::deleted_remote`] 如实回报，前端据此提示差异。
#[tauri::command]
pub async fn delete_message(state: State<'_, AppState>, id: i64) -> AppResult<DeleteReport> {
    // 先取路由信息（删除后就读不到了）。
    let (account_id, folder, uid) = state.store.message_route(id)?;
    let store = state.store.clone();
    // 闭包会拿走 account_id / folder，本地清理仍需要它们，故先克隆一份。
    let (route_account, route_folder) = (account_id.clone(), folder.clone());

    // ---- 1) 服务器删除：失败不致命，记录原因 ----
    let remote_error = tauri::async_runtime::spawn_blocking(move || -> Option<String> {
        let (Ok(account), Ok(password)) = (
            store.get_account(&account_id),
            credentials::get_password(&account_id),
        ) else {
            return Some("读取账号凭据失败，未在服务器删除".to_string());
        };
        match imap_client::connect(&account, &password) {
            Ok(mut session) => {
                let result = imap_client::select_folder(&mut session, &folder)
                    .and_then(|_| imap_client::delete_messages(&mut session, &[uid]));
                imap_client::logout(session);
                result.err().map(|e| e.to_string())
            }
            Err(e) => Some(format!("IMAP 连接失败：{e}")),
        }
    })
    .await
    .unwrap_or_else(|e| Some(format!("删除任务异常：{e}")));

    // ---- 2) 本地清理：无论服务器结果如何都执行 ----
    state.store.delete_message(id)?;
    // 同步该文件夹的邮件计数（失败不影响主流程）。
    let _ = state.store.refresh_folder_count(&route_account, &route_folder);

    Ok(DeleteReport {
        deleted_local: 1,
        deleted_remote: remote_error.is_none(),
        remote_error: remote_error.unwrap_or_default(),
    })
}

/// 批量删除邮件（供后续多选删除复用）。
#[tauri::command]
pub async fn delete_messages(state: State<'_, AppState>, ids: Vec<i64>) -> AppResult<DeleteReport> {
    if ids.is_empty() {
        return Ok(DeleteReport {
            deleted_local: 0,
            deleted_remote: true,
            remote_error: String::new(),
        });
    }

    // 按 (account_id, folder) 归组，一次连接删一批，避免逐封建连。
    let mut groups: std::collections::HashMap<(String, String), Vec<u32>> =
        std::collections::HashMap::new();
    for id in &ids {
        if let Ok((account_id, folder, uid)) = state.store.message_route(*id) {
            groups.entry((account_id, folder)).or_default().push(uid);
        }
    }

    let account_id = groups.keys().next().map(|(a, _)| a.clone()).unwrap_or_default();
    let store = state.store.clone();
    // 闭包会拿走 groups，本地清理仍需要「涉及哪些文件夹」，故先摘出来。
    let folders: Vec<(String, String)> = groups.keys().cloned().collect();
    let remote_error = tauri::async_runtime::spawn_blocking(move || -> Option<String> {
        let (Ok(account), Ok(password)) = (
            store.get_account(&account_id),
            credentials::get_password(&account_id),
        ) else {
            return Some("读取账号凭据失败，未在服务器删除".to_string());
        };
        let mut first_err: Option<String> = None;
        for ((_, folder), uids) in &groups {
            match imap_client::connect(&account, &password) {
                Ok(mut session) => {
                    let result = imap_client::select_folder(&mut session, folder)
                        .and_then(|_| imap_client::delete_messages(&mut session, uids));
                    imap_client::logout(session);
                    if let Err(e) = result {
                        first_err.get_or_insert(e.to_string());
                    }
                }
                Err(e) => {
                    first_err.get_or_insert(format!("IMAP 连接失败：{e}"));
                }
            }
        }
        first_err
    })
    .await
    .unwrap_or_else(|e| Some(format!("删除任务异常：{e}")));

    let deleted_local = state.store.delete_messages(&ids)?;
    // 同步涉及的文件夹计数（失败不影响主流程）。
    for (account_id, folder) in &folders {
        let _ = state.store.refresh_folder_count(account_id, folder);
    }

    Ok(DeleteReport {
        deleted_local,
        deleted_remote: remote_error.is_none(),
        remote_error: remote_error.unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------
// 发送
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn send_mail(state: State<'_, AppState>, payload: SendPayload) -> AppResult<()> {
    let account = state.store.get_account(&payload.account_id)?;
    let password = credentials::get_password(&payload.account_id)?;

    tauri::async_runtime::spawn_blocking(move || smtp_client::send(&account, &password, &payload))
        .await
        .map_err(|e| AppError::Other(format!("发送任务异常：{e}")))?
}

// ---------------------------------------------------------------------
// AI
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn ai_status(state: State<'_, AppState>) -> AppResult<AiStatus> {
    let settings = state.store.ai_settings()?;
    Ok(ai::status(&state.http, &settings).await)
}

#[tauri::command]
pub fn get_ai_settings(state: State<'_, AppState>) -> AppResult<AiSettings> {
    state.store.ai_settings()
}

#[tauri::command]
pub fn set_ai_settings(state: State<'_, AppState>, settings: AiSettings) -> AppResult<()> {
    state.store.save_ai_settings(&settings)
}

/// 生成邮件摘要。相同邮件 + 相同模型命中缓存，`force` 为 true 时强制刷新。
#[tauri::command]
pub async fn summarize_message(
    state: State<'_, AppState>,
    id: i64,
    force: bool,
) -> AppResult<Summary> {
    let detail = state.store.get_message(id)?;
    if !detail.header.body_loaded {
        return Err(AppError::Other("正文尚未加载，请先打开这封邮件".to_string()));
    }

    let settings = state.store.ai_settings()?;
    let cache_key = format!("summary:{id}:{}:{}", detail.message_id, settings.model);

    if !force {
        if let Some((text, model)) = state.store.get_cache(&cache_key)? {
            return Ok(Summary {
                text,
                model,
                cached: true,
            });
        }
    }

    let text = ai::summarize(&state.http, &settings, &detail).await?;
    state.store.put_cache(
        &cache_key,
        "summary",
        &settings.model,
        &text,
        &Utc::now().to_rfc3339(),
    )?;

    Ok(Summary {
        text,
        model: settings.model,
        cached: false,
    })
}

// ---------------------------------------------------------------------

fn account_from_input(input: &AccountInput, id: &str, created_at: &str) -> Account {
    Account {
        id: id.to_string(),
        email: input.email.trim().to_string(),
        display_name: input.display_name.trim().to_string(),
        imap_host: input.imap_host.trim().to_string(),
        imap_port: input.imap_port,
        imap_tls: input.imap_tls,
        smtp_host: input.smtp_host.trim().to_string(),
        smtp_port: input.smtp_port,
        smtp_tls: input.smtp_tls,
        username: input.username.trim().to_string(),
        created_at: created_at.to_string(),
    }
}
