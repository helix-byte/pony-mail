//! 应用文件日志：追加写入应用数据目录的 `pony-watch.log`。
//!
//! 收信监听（watcher）与更新检查共用同一文件——排查线上问题先看它。
//! 行格式 `[tag 月-日 时:分:秒.毫秒] 消息`，文件超过 2MB 时在进程首次写入时
//! 清卷重建。开 `PONY_IMAP_DEBUG` 时同一内容也输出到 stderr。

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager};

/// 写一行日志。目录取不到或写失败时静默返回——日志不该影响主流程。
pub fn log_line(app: &AppHandle, tag: &str, msg: &str) {
    let stamp = chrono::Local::now().format("%m-%d %H:%M:%S%.3f");
    // 先整行格式化为一个字符串，再一次性写入：收信与更新检查并发写同一文件时，
    // 逐片段 write_fmt 会互相交错出乱行（如 "[watch [watch ...] ] "）。
    let line = format!("[{tag} {stamp}] {msg}\n");
    if std::env::var_os("PONY_IMAP_DEBUG").is_some() {
        eprint!("{line}");
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
        let _ = f.write_all(line.as_bytes());
    }
}
