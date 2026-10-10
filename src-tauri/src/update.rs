//! 检查更新：并发查询 Gitee 与 GitHub 的 `releases/latest`，取较新版本返回给前端。
//!
//! 全部在 Rust 侧完成——渲染进程的 CSP 禁止外部请求，也不该开放。
//! 只查版本号与下载页链接、只提示不下载；应用内下载 / 静默安装是后续阶段的叠加。
//!
//! 发布仓库两端同名；owner 建仓后替换为实际账号（两端用户名可以不同）。

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::AppHandle;

use crate::error::{AppError, AppResult};
use crate::log::log_line;

const GITHUB_OWNER: &str = "helix-byte";
const GITHUB_REPO: &str = "pony-mail";
const GITEE_OWNER: &str = "helix-byte";
const GITEE_REPO: &str = "pony-mail";

/// 发现新版本事件（前端据此弹出更新对话框）。
pub const EVENT_UPDATE_AVAILABLE: &str = "update:available";

/// 单源请求超时。双源并发，整个检查的等待上限即该值。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Release 说明最多展示的字符数（防止超长正文撑爆对话框）。
const NOTES_MAX_CHARS: usize = 4000;

/// 检查结果（`check_update` 命令与 `update:available` 事件共用，字段与前端 TS 类型一一对应）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// 当前运行版本。
    pub current: String,
    /// 两平台比较后较新的版本（无更新时即当前版本）。
    pub latest: String,
    pub has_update: bool,
    /// 较新版本的来源平台："gitee" / "github"（无更新时为空串）。
    pub source: String,
    /// 两平台的 Release 页面链接（该平台版本落后于最新版本时为 None）。
    pub gitee_url: Option<String>,
    pub github_url: Option<String>,
    /// 更新说明（取自较新版本的 Release 正文，可能为空）。
    pub notes: String,
    /// 检查时间（本地时间，展示用）。
    pub checked_at: String,
}

impl UpdateInfo {
    /// 应用「跳过此版本」偏好：自动检查（force=false）跳过匹配的版本；
    /// 手动检查（force=true）始终展示。
    pub fn apply_skip(&mut self, skipped: &str, force: bool) {
        if !force && self.has_update && !skipped.is_empty() && self.latest == skipped {
            self.has_update = false;
        }
    }
}

/// 单源查询结果。
struct SourceRelease {
    platform: &'static str,
    version: semver::Version,
    url: Option<String>,
    notes: String,
}

/// 查询 Gitee 最新 Release（公开库匿名可读，无需令牌）。
async fn fetch_gitee(http: &reqwest::Client) -> Result<SourceRelease, String> {
    let url = format!("https://gitee.com/api/v5/repos/{GITEE_OWNER}/{GITEE_REPO}/releases/latest");
    let resp = http
        .get(&url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("请求失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let v: Value = resp.json().await.map_err(|e| format!("响应解析失败：{e}"))?;
    parse_release(&v, "gitee", |tag| {
        format!("https://gitee.com/{GITEE_OWNER}/{GITEE_REPO}/releases/tag/{tag}")
    })
}

/// 查询 GitHub 最新 Release（匿名限额 60 次/时，足够低频检查）。
async fn fetch_github(http: &reqwest::Client) -> Result<SourceRelease, String> {
    let url = format!("https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest");
    let resp = http
        .get(&url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("请求失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let v: Value = resp.json().await.map_err(|e| format!("响应解析失败：{e}"))?;
    parse_release(&v, "github", |tag| {
        format!("https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases/tag/{tag}")
    })
}

/// 从 Release JSON 提取版本 / 链接 / 说明（两平台字段名基本一致，缺链接时用页面格式兜底）。
fn parse_release(
    v: &Value,
    platform: &'static str,
    fallback_page: impl FnOnce(&str) -> String,
) -> Result<SourceRelease, String> {
    let tag = v
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| "响应缺少 tag_name".to_string())?;
    let version = parse_version(tag)?;
    let url = v
        .get("html_url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| Some(fallback_page(tag)));
    let notes = v
        .get("body")
        .or_else(|| v.get("description"))
        .and_then(Value::as_str)
        .map(clamp_notes)
        .unwrap_or_default();
    Ok(SourceRelease {
        platform,
        version,
        url,
        notes,
    })
}

/// 解析 tag 为 semver（容忍 `v` 前缀与首尾空白；解析失败则该源忽略）。
fn parse_version(tag: &str) -> Result<semver::Version, String> {
    let t = tag.trim().trim_start_matches(['v', 'V']);
    semver::Version::parse(t).map_err(|e| format!("tag `{tag}` 不是合法版本号：{e}"))
}

/// 裁剪说明长度（按字符数，避免截断多字节）。
fn clamp_notes(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() <= NOTES_MAX_CHARS {
        t.to_string()
    } else {
        let mut out: String = t.chars().take(NOTES_MAX_CHARS).collect();
        out.push('…');
        out
    }
}

/// 合并两源结果：取较新版本，并列时优先 Gitee（国内下载更快）。
///
/// 只有「版本与最新版本一致」的平台才带下载页链接——某平台上一版落后时，
/// 不该把用户引向旧版本的页面。
fn merge(current: &str, mut candidates: Vec<SourceRelease>) -> UpdateInfo {
    candidates.sort_by(|a, b| b.version.cmp(&a.version).then_with(|| a.platform.cmp(b.platform)));
    let best = &candidates[0];

    let current_ver =
        semver::Version::parse(current).unwrap_or_else(|_| semver::Version::new(0, 0, 0));
    let has_update = best.version > current_ver;

    let url_if_best = |platform: &str| {
        candidates
            .iter()
            .find(|c| c.platform == platform && c.version == best.version)
            .and_then(|c| c.url.clone())
    };

    UpdateInfo {
        current: current.to_string(),
        latest: best.version.to_string(),
        has_update,
        source: if has_update {
            best.platform.to_string()
        } else {
            String::new()
        },
        gitee_url: url_if_best("gitee"),
        github_url: url_if_best("github"),
        notes: if has_update {
            best.notes.clone()
        } else {
            String::new()
        },
        checked_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
    }
}

/// 检查更新主入口：并发查询两源；单源失败忽略，全部失败才报错。
pub async fn check(app: &AppHandle, http: &reqwest::Client, current: &str) -> AppResult<UpdateInfo> {
    let (gitee, github) = tokio::join!(fetch_gitee(http), fetch_github(http));

    let mut candidates = Vec::new();
    for (name, r) in [("gitee", gitee), ("github", github)] {
        match r {
            Ok(rel) => candidates.push(rel),
            Err(e) => log_line(app, "update", &format!("查询 {name} 失败：{e}")),
        }
    }
    if candidates.is_empty() {
        return Err(AppError::Other(
            "无法连接更新服务器，请检查网络后重试".to_string(),
        ));
    }
    Ok(merge(current, candidates))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(platform: &'static str, version: &str, url: &str, notes: &str) -> SourceRelease {
        SourceRelease {
            platform,
            version: semver::Version::parse(version).unwrap(),
            url: Some(url.to_string()),
            notes: notes.to_string(),
        }
    }

    #[test]
    fn parse_version_accepts_v_prefix_and_whitespace() {
        assert_eq!(
            parse_version(" v1.2.3 ").unwrap(),
            semver::Version::new(1, 2, 3)
        );
        assert_eq!(parse_version("0.1.0").unwrap(), semver::Version::new(0, 1, 0));
    }

    #[test]
    fn parse_version_rejects_non_semver() {
        assert!(parse_version("nightly").is_err());
        assert!(parse_version("v1.2").is_err());
    }

    #[test]
    fn merge_picks_newest_and_prefers_gitee_on_tie() {
        let info = merge(
            "0.1.0",
            vec![
                rel("github", "0.2.0", "https://gh/rel", "gh notes"),
                rel("gitee", "0.2.0", "https://gt/rel", "gt notes"),
            ],
        );
        assert!(info.has_update);
        assert_eq!(info.latest, "0.2.0");
        assert_eq!(info.source, "gitee", "并列时应有 Gitee 优先");
        assert_eq!(info.gitee_url.as_deref(), Some("https://gt/rel"));
        assert_eq!(info.github_url.as_deref(), Some("https://gh/rel"));
    }

    #[test]
    fn merge_only_links_platform_at_best_version() {
        // Gitee 落后一版：不应把用户引向旧版本的页面。
        let info = merge(
            "0.1.0",
            vec![
                rel("github", "0.3.0", "https://gh/rel", ""),
                rel("gitee", "0.2.0", "https://gt/rel", ""),
            ],
        );
        assert!(info.has_update);
        assert_eq!(info.source, "github");
        assert_eq!(info.gitee_url, None);
        assert_eq!(info.github_url.as_deref(), Some("https://gh/rel"));
    }

    #[test]
    fn merge_no_update_when_releases_not_newer() {
        let info = merge("0.2.0", vec![rel("gitee", "0.2.0", "https://gt/rel", "notes")]);
        assert!(!info.has_update);
        assert_eq!(info.source, "");
        assert_eq!(info.notes, "", "无更新时不携带说明");
    }

    #[test]
    fn apply_skip_only_affects_auto_check() {
        let mut auto = merge("0.1.0", vec![rel("gitee", "0.2.0", "u", "")]);
        auto.apply_skip("0.2.0", false);
        assert!(!auto.has_update, "自动检查应跳过已跳过的版本");

        let mut manual = merge("0.1.0", vec![rel("gitee", "0.2.0", "u", "")]);
        manual.apply_skip("0.2.0", true);
        assert!(manual.has_update, "手动检查必须无视跳过设置");

        let mut other = merge("0.1.0", vec![rel("gitee", "0.3.0", "u", "")]);
        other.apply_skip("0.2.0", false);
        assert!(other.has_update, "跳过的不是这个版本时仍应提示");
    }
}
