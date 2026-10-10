use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;
use uuid::Uuid;

use crate::ai;
use crate::classify::{self, Pick, SENDER_SKIP_MIN_SAMPLES};
use crate::credentials;
use crate::error::{AppError, AppResult};
use crate::mail::{imap_client, smtp_client};
use crate::models::*;
use crate::store::GroupAssignment;
use crate::tray;
use crate::update::{self, UpdateInfo};
use crate::watcher;
use crate::AppState;

// ---------------------------------------------------------------------
// 账号
// ---------------------------------------------------------------------

#[tauri::command]
pub fn list_accounts(state: State<'_, AppState>) -> AppResult<Vec<Account>> {
    state.store.list_accounts()
}

#[tauri::command]
pub fn add_account(
    app: AppHandle,
    state: State<'_, AppState>,
    input: AccountInput,
) -> AppResult<Account> {
    let id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();

    // 先写凭据再写库，任一步失败都清理，避免出现「有账号无密码」的脏状态
    credentials::save_password(&id, &input.password)?;
    match state.store.insert_account(&id, &input, &created_at) {
        Ok(account) => {
            // 账号就绪即开始实时收信：监听线程会静默补齐最近一批邮件，
            // 无需用户再手动点一次同步。
            watcher::start(&app, account.id.clone());
            Ok(account)
        }
        Err(e) => {
            let _ = credentials::delete_password(&id);
            Err(e)
        }
    }
}

#[tauri::command]
pub fn delete_account(app: AppHandle, state: State<'_, AppState>, id: String) -> AppResult<()> {
    // 先停监听，避免线程继续用已删除的账号重连。
    watcher::stop(&app, &id);
    state.store.delete_account(&id)?;
    credentials::delete_password(&id)?;
    // 级联删除会移除该账号的全部邮件（含未读），刷新托盘未读提示。
    tray::refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn set_account_password(
    app: AppHandle,
    id: String,
    password: String,
) -> AppResult<()> {
    credentials::save_password(&id, &password)?;
    // 重启监听：旧线程可能正拿着旧密码在退避重连，不重启会一直失败到下次冷启动。
    watcher::start(&app, id);
    Ok(())
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
    app: AppHandle,
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

    let report =
        tauri::async_runtime::spawn_blocking(move || sync_folder(&store, &account, &password, &folder, limit))
            .await
            .map_err(|e| AppError::Other(format!("同步任务异常：{e}")))??;
    // 重同步会用服务端 seen 覆盖本地状态（双向变化都可能），刷新托盘未读提示。
    tray::refresh(&app);
    Ok(report)
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

// ---------------------------------------------------------------------
// 实时收信设置与监听状态
// ---------------------------------------------------------------------

#[tauri::command]
pub fn get_mail_settings(state: State<'_, AppState>) -> AppResult<MailSettings> {
    state.store.mail_settings()
}

#[tauri::command]
pub fn set_mail_settings(state: State<'_, AppState>, settings: MailSettings) -> AppResult<()> {
    state.store.save_mail_settings(&settings)
}

// ---------------------------------------------------------------------
// 更新检查
// ---------------------------------------------------------------------

/// 检查更新：并发查询 Gitee / GitHub 的最新 Release（`force=true` 忽略「跳过此版本」）。
///
/// 手动检查（设置页按钮）直接调本命令取结果；自动检查在 `lib.rs` 启动后延迟
/// 触发，走 `update:available` 事件推送。
#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    state: State<'_, AppState>,
    force: bool,
) -> AppResult<UpdateInfo> {
    let current = app.package_info().version.to_string();
    // 先同步读设置，再发起网络请求（settings 读为短操作，不入 await 窗口）。
    let settings = state.store.update_settings()?;
    let mut info = update::check(&app, &state.http, &current).await?;
    info.apply_skip(&settings.skipped_version, force);
    Ok(info)
}

#[tauri::command]
pub fn get_update_settings(state: State<'_, AppState>) -> AppResult<UpdateSettings> {
    state.store.update_settings()
}

#[tauri::command]
pub fn set_update_settings(state: State<'_, AppState>, settings: UpdateSettings) -> AppResult<()> {
    state.store.save_update_settings(&settings)
}

// ---------------------------------------------------------------------
// 托盘与开机自启
// ---------------------------------------------------------------------

#[tauri::command]
pub fn get_tray_settings(state: State<'_, AppState>) -> AppResult<TraySettings> {
    state.store.tray_settings()
}

#[tauri::command]
pub fn set_tray_settings(state: State<'_, AppState>, settings: TraySettings) -> AppResult<()> {
    state.store.save_tray_settings(&settings)
}

/// 开机自启当前状态：直读 Windows 注册表实际值（非本地缓存，
/// 用户从任务管理器禁用启动项后此处也能如实反映）。
#[tauri::command]
pub fn get_autostart(app: AppHandle) -> AppResult<bool> {
    app.autolaunch()
        .is_enabled()
        .map_err(|e| AppError::Other(format!("读取开机自启状态失败：{e}")))
}

/// 启用 / 关闭开机自启（写当前用户的启动项，无需管理员权限）。
#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> AppResult<()> {
    let result = if enabled {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    };
    result.map_err(|e| {
        AppError::Other(format!(
            "{}开机自启失败：{e}",
            if enabled { "启用" } else { "关闭" }
        ))
    })?;

    // 上游 auto-launch 写出的 Run 值不带引号，安装路径含空格时（如 D:\Programs\Mine\Pony Mail）
    // 登录启动会被 Windows 解析失败并静默清理条目。启用后重写为规范格式；
    // 重写失败则回滚，避免留下「看似启用、实际无效」的启动项。
    #[cfg(windows)]
    if enabled {
        let name = app.package_info().name.clone();
        let exe = std::env::current_exe()
            .map_err(|e| AppError::Other(format!("读取程序路径失败：{e}")))?;
        // 先确认插件已写入（能读到值即视为已写入），再原位重写为规范格式。
        let rewritten =
            read_run_value(&name).and_then(|_| write_run_value(&name, &autostart_value(&exe)));
        if let Err(msg) = rewritten {
            let _ = app.autolaunch().disable();
            return Err(AppError::Other(format!("启用开机自启失败：{msg}")));
        }
    }

    Ok(())
}

/// 开机自启启动项的目标格式：`"C:\...\pony-mail.exe" --autostart`。
/// 路径必须带引号（含空格的安装路径否则会被系统解析失败并静默清理）；
/// 附带 `--autostart` 参数供启动时识别，静默驻留系统托盘不弹窗。
#[cfg(windows)]
fn autostart_value(exe: &std::path::Path) -> String {
    format!("\"{}\" {}", exe.display(), crate::AUTOSTART_ARG)
}

/// 升级自愈（仅正式构建）：开机自启处于启用状态但 Run 值不是规范格式时
/// （旧版本写入的无引号值，或缺 `--autostart` 参数）原位重写。
/// 用户经任务管理器禁用的启动项 is_enabled 为 false，不会被触碰。
#[cfg(windows)]
pub fn ensure_autostart_value(app: &AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    if !matches!(app.autolaunch().is_enabled(), Ok(true)) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let name = app.package_info().name.clone();
    let expected = autostart_value(&exe);
    let Ok(current) = read_run_value(&name) else {
        return;
    };
    if current == expected {
        return;
    }
    match write_run_value(&name, &expected) {
        Ok(()) => crate::log::log_line(
            app,
            "autostart",
            "启动项已重写为带引号并携带 --autostart 参数",
        ),
        Err(e) => crate::log::log_line(app, "autostart", &format!("启动项自愈失败：{e}")),
    }
}

/// 读取当前用户 Run 启动项中指定名称的值。
#[cfg(windows)]
fn read_run_value(name: &str) -> Result<String, String> {
    let key = open_run_key()?;
    key.get_string(name)
        .map_err(|e| format!("读取启动项失败：{e}"))
}

/// 写入当前用户 Run 启动项中指定名称的值。
#[cfg(windows)]
fn write_run_value(name: &str, value: &str) -> Result<(), String> {
    let key = open_run_key()?;
    key.set_string(name, value)
        .map_err(|e| format!("写入启动项失败：{e}"))
}

/// 打开当前用户 Run 键。需要 read+write：仅 write 的句柄不含
/// KEY_QUERY_VALUE，读数会被拒（0x80070005）。
#[cfg(windows)]
fn open_run_key() -> Result<windows_registry::Key, String> {
    windows_registry::CURRENT_USER
        .options()
        .read()
        .write()
        .open(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run")
        .map_err(|e| format!("打开启动项注册表失败：{e}"))
}

/// 各账号监听状态的最新快照。
///
/// 前端启动时先查一次对齐（早期事件可能在页面监听挂上之前就已广播），
/// 此后靠 `mail:watch-state` 事件增量更新。
#[tauri::command]
pub fn watch_states(state: State<'_, AppState>) -> Vec<watcher::WatchStateEvent> {
    state
        .watch_states
        .lock()
        .expect("watch_states 锁中毒")
        .values()
        .cloned()
        .collect()
}

/// 列邮件；传 `tag_ids` 时只返回打过其中**任一**标签的邮件（OR 语义，筛选在 SQL 层完成）。
#[tauri::command]
pub fn list_messages(
    state: State<'_, AppState>,
    account_id: String,
    folder: String,
    limit: u32,
    offset: u32,
    tag_ids: Option<Vec<i64>>,
) -> AppResult<Vec<MessageHeader>> {
    let ids = tag_ids.unwrap_or_default();
    state.store.list_messages_multi(
        &account_id,
        &folder,
        limit.clamp(1, 500),
        offset,
        &ids,
    )
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
pub async fn set_seen(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    seen: bool,
) -> AppResult<()> {
    state.store.set_seen(id, seen)?;
    // 本地已读状态已生效，先刷托盘未读提示，再回写服务器（有网络往返）。
    tray::refresh(&app);
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
pub async fn delete_message(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
) -> AppResult<DeleteReport> {
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
    // 删除未读邮件会改变未读总数，本地清理完成后刷新托盘提示。
    tray::refresh(&app);

    Ok(DeleteReport {
        deleted_local: 1,
        deleted_remote: remote_error.is_none(),
        remote_error: remote_error.unwrap_or_default(),
    })
}

/// 批量删除邮件（供后续多选删除复用）。
#[tauri::command]
pub async fn delete_messages(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<i64>,
) -> AppResult<DeleteReport> {
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
    // 删除未读邮件会改变未读总数，本地清理完成后刷新托盘提示。
    tray::refresh(&app);

    Ok(DeleteReport {
        deleted_local,
        deleted_remote: remote_error.is_none(),
        remote_error: remote_error.unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------
// 标签
// ---------------------------------------------------------------------

#[tauri::command]
pub fn list_tags(state: State<'_, AppState>, account_id: String) -> AppResult<Vec<Tag>> {
    state.store.list_tags(&account_id)
}

#[tauri::command]
pub fn create_tag(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
    color: Option<String>,
) -> AppResult<Tag> {
    let color = color.unwrap_or_else(|| DEFAULT_TAG_COLOR.to_string());
    state
        .store
        .create_tag(&account_id, &name, &color, &Utc::now().to_rfc3339())
}

#[tauri::command]
pub fn update_tag(
    state: State<'_, AppState>,
    id: i64,
    name: Option<String>,
    color: Option<String>,
) -> AppResult<()> {
    state.store.update_tag(id, name.as_deref(), color.as_deref())
}

#[tauri::command]
pub fn delete_tag(state: State<'_, AppState>, id: i64) -> AppResult<()> {
    state.store.delete_tag(id)
}

/// 给单封邮件打 / 取消标签。`attached` 为 true 时打标，false 时撤销。
///
/// 幂等：重复打标或撤销不存在的打标都不报错，前端可放心做开关。
#[tauri::command]
pub fn tag_message(
    state: State<'_, AppState>,
    message_id: i64,
    tag_id: i64,
    attached: bool,
) -> AppResult<()> {
    if attached {
        state
            .store
            .attach_tag(message_id, tag_id, &Utc::now().to_rfc3339())
    } else {
        state.store.detach_tag(message_id, tag_id)
    }
}

/// 单个标签的默认色板，与前端保持一致，避免新建后颜色随机跳变。
const DEFAULT_TAG_COLOR: &str = "#6366f1";
/// 标签组的默认色，刻意与标签默认色区分开，便于侧栏一眼分辨层级。
const DEFAULT_GROUP_COLOR: &str = "#8b5cf6";

// ---------------------------------------------------------------------
// 标签组
// ---------------------------------------------------------------------

#[tauri::command]
pub fn list_tag_groups(state: State<'_, AppState>, account_id: String) -> AppResult<Vec<TagGroup>> {
    state.store.list_tag_groups(&account_id)
}

#[tauri::command]
pub fn create_tag_group(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
    color: Option<String>,
    tag_ids: Option<Vec<i64>>,
) -> AppResult<TagGroup> {
    let color = color.unwrap_or_else(|| DEFAULT_GROUP_COLOR.to_string());
    let now = Utc::now().to_rfc3339();
    let group = state
        .store
        .create_tag_group(&account_id, &name, &color, &now)?;
    // 允许建组时一并指定成员，省去「先建空组再拉标签进去」的两步操作。
    if let Some(ids) = tag_ids {
        if !ids.is_empty() {
            state.store.set_group_tags(group.id, &ids, &now)?;
        }
    }
    // 回读一次，让返回的对象带上成员与计数，前端可直接插入列表。
    Ok(state
        .store
        .list_tag_groups(&account_id)?
        .into_iter()
        .find(|g| g.id == group.id)
        .unwrap_or(group))
}

#[tauri::command]
pub fn update_tag_group(
    state: State<'_, AppState>,
    id: i64,
    name: Option<String>,
    color: Option<String>,
    collapsed: Option<bool>,
    ai_mode: Option<String>,
) -> AppResult<()> {
    state.store.update_tag_group(
        id,
        name.as_deref(),
        color.as_deref(),
        collapsed,
        ai_mode.as_deref(),
    )
}

#[tauri::command]
pub fn delete_tag_group(state: State<'_, AppState>, id: i64) -> AppResult<()> {
    state.store.delete_tag_group(id)
}

/// 整组覆盖式设置成员标签。只影响组成员关系，不动标签本身与邮件打标。
#[tauri::command]
pub fn set_group_tags(state: State<'_, AppState>, group_id: i64, tag_ids: Vec<i64>) -> AppResult<()> {
    state
        .store
        .set_group_tags(group_id, &tag_ids, &Utc::now().to_rfc3339())
}

/// 把多个组展开为标签并集，供「选中组本身」时换算成实际的筛选标签集合。
#[tauri::command]
pub fn resolve_group_tags(state: State<'_, AppState>, group_ids: Vec<i64>) -> AppResult<Vec<i64>> {
    state.store.groups_tag_ids(&group_ids)
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
    let cache_key = summary_cache_key(id, &detail.message_id, &settings.model);

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

/// 只读摘要缓存，**绝不触发模型推理**。
///
/// 命中返回 `Some(Summary)`，未命中返回 `None`（前端据此落到「生成摘要」引导态）。
/// 与 [`summarize_message`] 分开是为了表达清晰：打开邮件时只想「看看有没有现成的」，
/// 不该意外启动一次可能耗时数十秒的本地推理。
#[tauri::command]
pub fn get_cached_summary(state: State<'_, AppState>, id: i64) -> AppResult<Option<Summary>> {
    // 邮件可能已被删除 / 正文未抓取，这两种情况都视为「没有缓存」而非错误。
    let Ok(detail) = state.store.get_message(id) else {
        return Ok(None);
    };
    if !detail.header.body_loaded {
        return Ok(None);
    }

    let settings = state.store.ai_settings()?;
    let cache_key = summary_cache_key(id, &detail.message_id, &settings.model);
    Ok(state
        .store
        .get_cache(&cache_key)?
        .map(|(text, model)| Summary {
            text,
            model,
            cached: true,
        }))
}

/// 摘要缓存键：邮件 id + Message-ID + 模型。
///
/// 带上模型是因为同一封邮件在不同模型下的摘要不可互换；
/// 带上 Message-ID 是为了覆盖「同一账号同一 UID 位置换了新邮件」的情况。
fn summary_cache_key(id: i64, message_id: &str, model: &str) -> String {
    format!("summary:{id}:{message_id}:{model}")
}

// ---------------------------------------------------------------------
// 智能整理
// ---------------------------------------------------------------------

/// 单批整理的邮件数上限。单封推理 6~16 秒（本机 9b 实测），快速通道会显著
/// 缩短实际耗时；剩余邮件由下一次整理续跑（队列即「未整理」）。
const CLASSIFY_BATCH_LIMIT: i64 = 50;
/// 连续失败熔断阈值：模型/解析连续失败这么多封即中止（写库失败不计入）。
const MAX_CONSECUTIVE_FAILURES: usize = 3;
/// 未配置任何 AI 整理维度时的提示（批量与单封两条路径共用）。
const NO_GROUPS_HINT: &str =
    "还没有开启 AI 整理的标签组。请先在左侧标签组的编辑里选择一种「AI 整理」模式。";

/// 批量整理的事件流（Tauri Channel 点对点推送，前端逐封更新界面）。
#[derive(Clone, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "event",
    content = "data"
)]
pub enum ClassifyEvent {
    Start {
        total: usize,
    },
    Item {
        message_id: i64,
        tag_ids: Vec<i64>,
        skipped: bool,
        error: Option<String>,
    },
    Done {
        classified: usize,
        failed: usize,
        skipped: usize,
        remaining: i64,
        cancelled: bool,
    },
    Error {
        message: String,
    },
}

/// 批量智能整理：把未整理的邮件逐个交给本地模型，逐封推送进度。
///
/// 手动按钮与「同步后自动整理」共用本命令；按账户做并发去重。
#[tauri::command]
pub async fn classify_messages(
    state: State<'_, AppState>,
    account_id: String,
    on_event: Channel<ClassifyEvent>,
) -> AppResult<()> {
    {
        let mut running = state.classifying.lock().expect("classifying 锁中毒");
        if !running.insert(account_id.clone()) {
            return Err(AppError::Other("该账户正在整理中，请稍候".to_string()));
        }
    }
    let result = run_classify(&state, &account_id, &on_event).await;
    state
        .classifying
        .lock()
        .expect("classifying 锁中毒")
        .remove(&account_id);
    state
        .cancel_flags
        .lock()
        .expect("cancel_flags 锁中毒")
        .remove(&account_id);
    result
}

/// 请求中止正在进行的整理（在消息边界生效；已写回的结果保留）。
#[tauri::command]
pub fn cancel_classify(state: State<'_, AppState>, account_id: String) -> AppResult<()> {
    if let Some(flag) = state
        .cancel_flags
        .lock()
        .expect("cancel_flags 锁中毒")
        .get(&account_id)
    {
        flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

async fn run_classify(
    state: &AppState,
    account_id: &str,
    on_event: &Channel<ClassifyEvent>,
) -> AppResult<()> {
    // 维度组等快照：此后用户改组只影响下一轮。
    let ctx = ClassifyCtx::load(state, account_id)?;

    // 取消标志（cancel_classify 置位；结束时由 classify_messages 统一清理）。
    let cancel = Arc::new(AtomicBool::new(false));
    state
        .cancel_flags
        .lock()
        .expect("cancel_flags 锁中毒")
        .insert(account_id.to_string(), cancel.clone());

    // `stamp` 是写库用的时间戳（`now` 供模型判断时限，在 ctx 里）。
    let stamp = Utc::now().to_rfc3339();

    let ids = state.store.unclassified_ids(account_id, CLASSIFY_BATCH_LIMIT)?;
    let _ = on_event.send(ClassifyEvent::Start { total: ids.len() });

    let mut classified = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut consecutive = 0usize;
    let mut cancelled = false;

    for id in ids {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }

        // ---- 读阶段（本地查询，短持有连接）----
        let input = match state.store.classify_input(id) {
            Ok(i) => i,
            // 邮件在整理途中被删：跳过，不计数。
            Err(_) => continue,
        };

        // ---- 推理阶段（快速通道 / 模型，与单封打标共用）----
        let outcome = compute_assignments(state, &ctx, id, &input, true).await;

        // ---- 写阶段 / 失败处理 ----
        match outcome {
            Err(message) => {
                failed += 1;
                consecutive += 1;
                let _ = on_event.send(ClassifyEvent::Item {
                    message_id: id,
                    tag_ids: Vec::new(),
                    skipped: false,
                    error: Some(message.clone()),
                });
                if consecutive >= MAX_CONSECUTIVE_FAILURES {
                    let _ = on_event.send(ClassifyEvent::Error {
                        message: format!(
                            "连续 {MAX_CONSECUTIVE_FAILURES} 封整理失败，已中止。{message}"
                        ),
                    });
                    break;
                }
            }
            Ok((assignments, via_fastlane)) => {
                match state
                    .store
                    .save_classify_result(account_id, id, &assignments, &stamp)
                {
                    Ok(tag_ids) => {
                        classified += 1;
                        consecutive = 0;
                        if via_fastlane {
                            skipped += 1;
                        }
                        let _ = on_event.send(ClassifyEvent::Item {
                            message_id: id,
                            tag_ids,
                            skipped: via_fastlane,
                            error: None,
                        });
                    }
                    Err(e) => {
                        // 写库失败不是模型问题：计失败但**不**熔断；
                        // ai_tagged_at 未写，下次整理会自动重试。
                        failed += 1;
                        let _ = on_event.send(ClassifyEvent::Item {
                            message_id: id,
                            tag_ids: Vec::new(),
                            skipped: false,
                            error: Some(format!("写入失败：{e}")),
                        });
                    }
                }
            }
        }
    }

    let remaining = state.store.count_unclassified(account_id).unwrap_or(0);
    let _ = on_event.send(ClassifyEvent::Done {
        classified,
        failed,
        skipped,
        remaining,
        cancelled,
    });
    Ok(())
}

/// 为**单封**邮件执行 AI 打标签（右键菜单「AI 自动标签」）。
///
/// 与批量整理共用推理与写回语义（手动标签保护、组内单选、写回事务全一致），
/// 两处刻意差异：
/// 1. 不受「未整理队列」限制——这是显式动作，已整理过的邮件也会重新判定
///    （模型/prompt 更新后可用它逐封刷新）；
/// 2. 不走发件人快速通道——手动单封就是要模型重新给判断，不被历史共识短路。
#[tauri::command]
pub async fn classify_message(
    state: State<'_, AppState>,
    message_id: i64,
) -> AppResult<Vec<i64>> {
    // 与批量整理共享账户级互斥：批处理进行中时单封动作直接拒绝。
    let (account_id, _, _) = state.store.message_route(message_id)?;
    {
        let mut running = state.classifying.lock().expect("classifying 锁中毒");
        if !running.insert(account_id.clone()) {
            return Err(AppError::Other("该账户正在整理中，请稍候".to_string()));
        }
    }
    let result = run_classify_single(&state, &account_id, message_id).await;
    state
        .classifying
        .lock()
        .expect("classifying 锁中毒")
        .remove(&account_id);
    result
}

async fn run_classify_single(
    state: &AppState,
    account_id: &str,
    message_id: i64,
) -> AppResult<Vec<i64>> {
    let ctx = ClassifyCtx::load(state, account_id)?;
    let stamp = Utc::now().to_rfc3339();
    let input = state.store.classify_input(message_id)?;

    let (assignments, _) = compute_assignments(state, &ctx, message_id, &input, false)
        .await
        .map_err(|message| AppError::Other(format!("AI 打标签失败：{message}")))?;

    let tag_ids = state
        .store
        .save_classify_result(account_id, message_id, &assignments, &stamp)?;
    Ok(tag_ids)
}

/// 单次整理运行的快照：维度组、模型设置与推理所需上下文。
/// 运行中用户改组只影响下一轮（快照语义）。
struct ClassifyCtx {
    account_id: String,
    groups: Vec<classify::DimensionGroup>,
    group_names: Vec<String>,
    settings: AiSettings,
    /// 供模型判断时限的「当前时间」。
    now: String,
}

impl ClassifyCtx {
    /// 读取一次运行所需的全部快照；无启用维度组时报出可操作的中文提示。
    fn load(state: &AppState, account_id: &str) -> AppResult<Self> {
        let groups = state.store.ai_dimension_groups(account_id)?;
        if groups.is_empty() {
            return Err(AppError::Other(NO_GROUPS_HINT.to_string()));
        }
        let settings = state.store.ai_settings()?;
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        let group_names = groups.iter().map(|g| g.name.clone()).collect();
        Ok(Self {
            account_id: account_id.to_string(),
            groups,
            group_names,
            settings,
            now,
        })
    }
}

/// 计算一封邮件的维度指派（推理阶段，不含写库）。
///
/// 返回 `Ok((assignments, via_fastlane))`；`Err(String)` 表示读数据/模型/解析失败，
/// 由调用方决定如何计数（批量走连续失败与熔断，单封直接报错）。
/// `allow_fastlane = false` 时强制走模型（见 `classify_message` 的说明）。
async fn compute_assignments(
    state: &AppState,
    ctx: &ClassifyCtx,
    message_id: i64,
    input: &classify::ClassifyInput,
    allow_fastlane: bool,
) -> Result<(Vec<GroupAssignment>, bool), String> {
    let groups = &ctx.groups;
    let sample_count = state
        .store
        .sender_classified_count(&ctx.account_id, &input.from_addr, message_id)
        .map_err(|e| e.to_string())?;
    let mut stats = Vec::with_capacity(groups.len());
    for g in groups {
        stats.push(
            state
                .store
                .sender_group_stats(&ctx.account_id, &input.from_addr, message_id, g.id)
                .map_err(|e| e.to_string())?,
        );
    }

    // 快速通道：同发件人历史 ≥3 封且各启用组「唯一标签全覆盖或无历史」→ 跳过模型。
    if allow_fastlane {
        if let Some(picks) =
            classify::sender_consensus(&stats, sample_count, SENDER_SKIP_MIN_SAMPLES)
        {
            let assignments = groups
                .iter()
                .zip(picks)
                .map(|(g, p)| GroupAssignment {
                    group_id: g.id,
                    pick: p.map(Pick::Existing),
                })
                .collect();
            return Ok((assignments, true));
        }
    }

    let hints = classify::sender_hints(&stats, &ctx.group_names, sample_count);
    let system = classify::build_system_prompt(groups);
    let user = classify::build_user_prompt(input, &hints, &ctx.now);
    let raw = ai::run_with_recovery(&state.http, &ctx.settings, |model| {
        ai::chat_json(&state.http, &ctx.settings, model, &system, &user, 512)
    })
    .await
    .map_err(|e| e.to_string())?;

    classify::parse_classify(&raw, groups)
        .map(|parsed| {
            let assignments = groups
                .iter()
                .zip(parsed.picks)
                .map(|(g, p)| GroupAssignment {
                    group_id: g.id,
                    pick: p,
                })
                .collect();
            (assignments, false)
        })
        .ok_or_else(|| "模型输出无法解析为有效的整理结果".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_cache_key_includes_id_message_id_and_model() {
        let key = summary_cache_key(7, "AC123@example.com", "qwen3.5:9b");
        assert_eq!(key, "summary:7:AC123@example.com:qwen3.5:9b");
    }

    #[test]
    fn summary_cache_key_differs_by_model() {
        // 同一封邮件换模型后必须视为不同缓存，否则会串味。
        let a = summary_cache_key(1, "mid", "qwen3.5:9b");
        let b = summary_cache_key(1, "mid", "qwen2.5:14b");
        assert_ne!(a, b);
    }

    #[test]
    fn summary_cache_key_differs_by_message_id() {
        // 同一 id 位置若换了邮件（UID 复用），缓存必须失效。
        let a = summary_cache_key(1, "old-mid", "m");
        let b = summary_cache_key(1, "new-mid", "m");
        assert_ne!(a, b);
    }

    #[test]
    fn summary_cache_key_prefix_matches_delete_cleanup_pattern() {
        // store::delete_message 用 `summary:{id}:%` 前缀清理，两者必须一致，
        // 否则删邮件会留下孤儿缓存。
        let key = summary_cache_key(42, "mid", "m");
        assert!(
            key.starts_with("summary:42:"),
            "缓存键前缀必须与删除清理条件匹配：{key}"
        );
    }
}
