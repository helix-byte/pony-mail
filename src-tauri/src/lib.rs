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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tauri::Manager;

use store::Store;

/// 全局应用状态，通过 `State<AppState>` 注入到各 command。
pub struct AppState {
    pub store: Store,
    pub http: reqwest::Client,
    /// 正在整理的账户集合（账户级并发去重，防止同一账户跑两批整理）。
    pub classifying: Mutex<HashSet<String>>,
    /// 各账户的整理取消标志（「停止整理」按钮在消息边界生效）。
    pub cancel_flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
