//! 系统托盘：图标与右键菜单、左键显示 / 隐藏主窗口、关闭到托盘。
//!
//! 关闭拦截（[`on_window_event`]）在「关闭到托盘」开启时把窗口隐藏而非销毁；
//! 真正的退出入口是托盘菜单的「退出」项（`app.exit` 直接终止事件循环，
//! 不经过 CloseRequested，无需防重入标志）。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Window, WindowEvent};
use tauri_plugin_notification::NotificationExt;

use crate::AppState;

/// 托盘图标 id（`app.tray_by_id` 检索用；关闭拦截依赖它判断托盘是否存活）。
pub const TRAY_ID: &str = "tray";
const MENU_SHOW: &str = "tray-show";
const MENU_QUIT: &str = "tray-quit";

/// 两次「左键切换窗口」之间的最小间隔。双击会额外产生一个 Click 事件，
/// 不设防抖会让双击变成「切换两次」（窗口闪一下又回到原状态）。
const TOGGLE_DEBOUNCE: Duration = Duration::from_millis(500);

/// 常规托盘图标（32×32）。编译期内嵌为原始 RGBA，与 `default_window_icon`
/// 同机制，无需 image-png feature；路径相对 CARGO_MANIFEST_DIR。
const ICON_BASE: Image<'static> = tauri::include_image!("icons/32x32.png");
/// 有未读时替换的带红点版本（由 `examples/gen_tray_unread.rs` 生成）。
const ICON_UNREAD: Image<'static> = tauri::include_image!("icons/tray-unread.png");

/// 在 setup 中创建托盘。返回 Err 由调用方记日志，不阻断启动
/// （关闭拦截会自动退化为「放行关闭」，见 [`on_window_event`]）。
pub fn init(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, MENU_SHOW, "显示主窗口", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &sep, &quit])?;

    // 防抖状态存回调闭包内（仅托盘事件线程访问，锁竞争可忽略）。
    let last_toggle = Mutex::new(None::<Instant>);

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(ICON_BASE)
        .tooltip("Pony Mail")
        .menu(&menu)
        // 默认左键也弹菜单；关掉后左键留给「显示 / 隐藏窗口」，右键仍弹菜单。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_SHOW => show_main(app),
            MENU_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(move |tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                // Windows 单击会先发 Down 再发 Up 两个 Click，只看 Up，
                // 否则一次点击会被处理两次。
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let now = Instant::now();
                {
                    let mut last = last_toggle.lock().unwrap();
                    if last.is_some_and(|t| now.duration_since(t) < TOGGLE_DEBOUNCE) {
                        return;
                    }
                    *last = Some(now);
                }
                let app = tray.app_handle();
                let Some(w) = app.get_webview_window("main") else {
                    return;
                };
                // 最小化的窗口 is_visible 仍为 true：只有「可见且未最小化」才隐藏。
                let shown = w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false);
                if shown {
                    let _ = w.hide();
                } else {
                    show_main(app);
                }
            }
        })
        .build(app)?;
    Ok(())
}

/// 显示并聚焦主窗口（托盘菜单 / 左键 / 单实例回调共用）。
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        // 顺序固定：先还原再显示再聚焦。对隐藏窗口只调 set_focus 不会把它
        // 带出来（tauri #12936），必须先 show。
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// 按未读总数刷新托盘 tooltip 与图标（未读 > 0 时换成带红点版本）。
/// 幂等，所有改变未读数的位置都应调用；数值从 SQLite 未读总数派生
/// （不做内存缓存，避免多写点漂移）。可在任意线程调用（Tauri 内部自动切主线程）。
pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let unread = app.state::<AppState>().store.unread_total().unwrap_or(0);
    let _ = tray.set_tooltip(Some(tooltip_text(unread)));
    let _ = tray.set_icon(Some(if unread > 0 { ICON_UNREAD } else { ICON_BASE }));
}

/// tooltip 文案（纯函数，便于单测）。
fn tooltip_text(unread: i64) -> String {
    match unread {
        n if n <= 0 => "Pony Mail".to_string(),
        n if n > 99 => "Pony Mail - 99+ 封未读".to_string(),
        n => format!("Pony Mail - {n} 封未读"),
    }
}

/// 关闭窗口拦截：开启「关闭到托盘」且托盘存活时，改为隐藏到托盘。
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if window.label() != "main" {
        return;
    }
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    let app = window.app_handle();
    let tray_alive = app.tray_by_id(TRAY_ID).is_some();
    // 托盘创建失败（极罕见）时放行关闭，避免「隐藏后无托盘可恢复」的僵尸状态。
    if !tray_alive {
        crate::log::log_line(app, "tray", "关闭请求：托盘不存在，放行关闭");
        return;
    }
    let close_to_tray = app
        .state::<AppState>()
        .store
        .tray_settings()
        .map(|s| s.close_to_tray)
        .unwrap_or(true);
    if !close_to_tray {
        crate::log::log_line(app, "tray", "关闭请求：关闭到托盘已关，放行关闭");
        return; // 放行 → 窗口销毁 → 无窗口 → 应用退出（默认行为）
    }
    api.prevent_close();
    if let Err(e) = window.hide() {
        crate::log::log_line(app, "tray", &format!("隐藏窗口失败：{e}"));
    } else {
        crate::log::log_line(app, "tray", "关闭请求：已隐藏到托盘");
    }
    notify_first_hide(app);
}

/// 首次隐藏到托盘时弹一次系统通知；只在发送成功时落标记，失败则下次隐藏重试。
fn notify_first_hide(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.store.tray_hide_notified().unwrap_or(true) {
        return;
    }
    let shown = app
        .notification()
        .builder()
        .title("Pony Mail 仍在后台运行")
        .body("窗口已隐藏到系统托盘，新邮件会继续实时接收；点击托盘图标可重新打开。")
        .show()
        .is_ok();
    if shown {
        let _ = state.store.set_tray_hide_notified();
    }
}

#[cfg(test)]
mod tests {
    use super::tooltip_text;

    #[test]
    fn tooltip_without_unread_is_plain_name() {
        assert_eq!(tooltip_text(0), "Pony Mail");
        // 理论不可达（COUNT 不会为负），防御性兜底成「无未读」。
        assert_eq!(tooltip_text(-1), "Pony Mail");
    }

    #[test]
    fn tooltip_with_unread_shows_count() {
        assert_eq!(tooltip_text(3), "Pony Mail - 3 封未读");
        assert_eq!(tooltip_text(99), "Pony Mail - 99 封未读");
    }

    #[test]
    fn tooltip_caps_at_99_plus() {
        assert_eq!(tooltip_text(150), "Pony Mail - 99+ 封未读");
    }
}
