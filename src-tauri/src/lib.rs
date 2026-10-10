/// 对集成测试可见（tests/ 下需要直接驱动 IMAP 会话）。
pub mod mail;
pub mod models;
/// 对集成测试可见（tests/ 下需要驱动迁移机制与 AI 整理数据层）。
pub mod schema;
/// 对集成测试可见（tests/ 下需要直接驱动删除等数据层逻辑）。
pub mod store;
/// 对集成测试/示例工具可见（examples/ 下需要驱动真实摘要链路）。
pub mod ai;
/// 对集成测试可见（tests/ 下需要构造整理结果与维度组）。
pub mod classify;
mod commands;
mod credentials;
mod error;
/// 通用文件日志（收信监听与更新检查共用 `pony-watch.log`）。
mod log;
/// 检查更新（并发查询 Gitee / GitHub 的 releases/latest）。
mod update;
/// 系统托盘（托盘菜单、关闭到托盘、单实例唤醒窗口的共享入口）。
mod tray;
/// 实时收信监听（IDLE / 轮询 + 新邮件事件与系统通知）。
/// 公开是因为 AppState 的 pub 字段引用了其事件类型。
pub mod watcher;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};

use store::Store;

/// 开机自启启动项的附加参数。注册表值形如 `"C:\...\pony-mail.exe" --autostart`，
/// 启动时据此识别「自启拉起」：静默驻留系统托盘，不弹出主窗口。
pub(crate) const AUTOSTART_ARG: &str = "--autostart";

/// 本次启动是否由开机自启拉起。
fn is_autostart_launch() -> bool {
    std::env::args().any(|a| a == AUTOSTART_ARG)
}

/// 全局应用状态，通过 `State<AppState>` 注入到各 command。
pub struct AppState {
    pub store: Store,
    pub http: reqwest::Client,
    /// 正在整理的账户集合（账户级并发去重，防止同一账户跑两批整理）。
    pub classifying: Mutex<HashSet<String>>,
    /// 各账户的整理取消标志（「停止整理」按钮在消息边界生效）。
    pub cancel_flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// 各账户的收信监听停止标志（账户存在期间常驻；删除账号 / 改密码时置位重启）。
    pub watchers: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// 各账户监听状态的最新快照（`watch_states` 命令与事件广播共用一份数据）。
    pub watch_states: Mutex<HashMap<String, watcher::WatchStateEvent>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 单实例必须最先注册：插件按注册顺序执行，它要在任何建窗逻辑之前
        // 拦下第二实例（第二实例即时退出，回调在第一实例里唤醒窗口）。
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // 自启拉起的第二实例（应用已在运行）：保持现状，不打扰用户。
            if argv.iter().any(|a| a.as_str() == AUTOSTART_ARG) {
                return;
            }
            tray::show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        // 开机自启（Windows 写 HKCU 的 Run 启动项）；开关由设置页命令控制。
        .plugin(tauri_plugin_autostart::init(
            // 仅 macOS 使用；Windows 构建必须照样传（参数为必填）。
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            // 自启命令携带 --autostart 参数：启动时据此静默驻留托盘而非弹窗。
            Some(vec![AUTOSTART_ARG]),
        ))
        .on_window_event(|window, event| tray::on_window_event(window, event))
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            let store = Store::new(data_dir.join("pony.db"))?;
            let http = reqwest::Client::builder()
                .user_agent(concat!("PonyMail/", env!("CARGO_PKG_VERSION")))
                .build()?;

            app.manage(AppState {
                store,
                http,
                classifying: Mutex::new(HashSet::new()),
                cancel_flags: Mutex::new(HashMap::new()),
                watchers: Mutex::new(HashMap::new()),
                watch_states: Mutex::new(HashMap::new()),
            });
            // 为已登记的每个账号拉起实时收信监听（新账号在 add_account 里即时启动）。
            watcher::start_all(app.handle());

            // 系统托盘：关闭到托盘、托盘菜单与左键切换窗口。
            // 失败仅记日志（关闭拦截会自动退化为「放行关闭」），不阻断启动。
            if let Err(e) = tray::init(app.handle()) {
                log::log_line(app.handle(), "tray", &format!("托盘创建失败：{e}"));
            } else {
                // 启动即按库中未读数刷新一次 tooltip。
                tray::refresh(app.handle());
            }

            // 主窗口在 config 中默认隐藏（visible: false），避免开机自启时一闪而过：
            // 手动启动（无 --autostart 参数）在此显式弹出主窗口；自启拉起则静默驻留托盘。
            if is_autostart_launch() {
                log::log_line(app.handle(), "tray", "开机自启：静默启动，驻留系统托盘");
            } else {
                log::log_line(app.handle(), "tray", "手动启动：显示主窗口");
                tray::show_main(app.handle());
            }

            // 升级自愈（仅正式构建）：旧版本写入的开机自启项（无引号、无自启参数）
            // 在此原位重写为规范格式，用户无需重新开关一次；用户在任务管理器
            // 禁用的启动项（is_enabled 为 false）不会被触碰。
            #[cfg(windows)]
            commands::ensure_autostart_value(app.handle());

            // 启动后延迟自动检查更新：避开启动高峰（IMAP 监听/同步相继起步）；
            // 仅正式构建触发；发现新版本且未被跳过时推送 update:available 事件。
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if cfg!(debug_assertions) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let state = handle.state::<AppState>();
                let settings = match state.store.update_settings() {
                    Ok(s) => s,
                    Err(e) => {
                        log::log_line(&handle, "update", &format!("读取更新设置失败：{e}"));
                        return;
                    }
                };
                if !settings.auto_check {
                    return;
                }
                let current = handle.package_info().version.to_string();
                match update::check(&handle, &state.http, &current).await {
                    Ok(mut info) => {
                        info.apply_skip(&settings.skipped_version, false);
                        if info.has_update {
                            log::log_line(
                                &handle,
                                "update",
                                &format!("发现新版本 v{}（来源 {}）", info.latest, info.source),
                            );
                            let _ = handle.emit(update::EVENT_UPDATE_AVAILABLE, &info);
                        } else {
                            log::log_line(&handle, "update", "自动检查：已是最新版本");
                        }
                    }
                    Err(e) => log::log_line(&handle, "update", &format!("自动检查失败：{e}")),
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_accounts,
            commands::add_account,
            commands::delete_account,
            commands::set_account_password,
            commands::test_connection,
            commands::sync_inbox,
            commands::get_mail_settings,
            commands::set_mail_settings,
            commands::watch_states,
            commands::list_messages,
            commands::load_message_body,
            commands::set_seen,
            commands::set_flagged,
            commands::delete_message,
            commands::delete_messages,
            commands::list_tags,
            commands::create_tag,
            commands::update_tag,
            commands::delete_tag,
            commands::tag_message,
            commands::list_tag_groups,
            commands::create_tag_group,
            commands::update_tag_group,
            commands::delete_tag_group,
            commands::set_group_tags,
            commands::resolve_group_tags,
            commands::send_mail,
            commands::ai_status,
            commands::get_ai_settings,
            commands::set_ai_settings,
            commands::summarize_message,
            commands::get_cached_summary,
            commands::classify_messages,
            commands::cancel_classify,
            commands::classify_message,
            commands::get_update_settings,
            commands::set_update_settings,
            commands::check_update,
            commands::get_tray_settings,
            commands::set_tray_settings,
            commands::get_autostart,
            commands::set_autostart,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
