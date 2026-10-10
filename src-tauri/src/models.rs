use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

/// 邮箱账号（不含密码）。密码只存在于系统凭据管理器中。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: bool,
    pub username: String,
    pub created_at: String,
}

/// 新增账号 / 测试连接入参（含明文密码，仅内存传递，不落库）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInput {
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    pub imap_host: String,
    pub imap_port: u16,
    #[serde(default = "default_true")]
    pub imap_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    #[serde(default = "default_true")]
    pub smtp_tls: bool,
    pub username: String,
    pub password: String,
}

/// 邮件列表项（不含正文，列表渲染用）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageHeader {
    pub id: i64,
    pub uid: u32,
    pub folder: String,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub date_ts: i64,
    pub seen: bool,
    pub flagged: bool,
    pub has_attachments: bool,
    pub body_loaded: bool,
    pub snippet: String,
    /// 该邮件携带的标签。列表页直接渲染，避免逐封再查一次接口。
    pub tag_ids: Vec<i64>,
}

/// 用户自定义标签（按账号隔离，同一账号下名称唯一）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub id: i64,
    pub account_id: String,
    pub name: String,
    pub color: String,
    pub sort_order: i64,
    /// 该标签下已打标的邮件数，用于侧栏角标。
    pub message_count: i64,
    /// 创建者：`"user"`（默认）或 `"ai"`（智能整理创建）。AI 标签可在界面区分、用户可删。
    pub created_by: String,
}

/// 标签组：可点击筛选的标签合集，组内标签取**并集**（OR）。
///
/// 与标签是**多对多**关系——同一标签可同时归入多个组。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagGroup {
    pub id: i64,
    pub account_id: String,
    pub name: String,
    pub color: String,
    pub sort_order: i64,
    /// 侧栏折叠状态（持久化，避免每次重启都要重新收起）。
    pub collapsed: bool,
    /// 组内标签 id 列表。
    pub tag_ids: Vec<i64>,
    /// 组内**去重后**的邮件数。不能简单把各标签计数相加——
    /// 一封邮件若同时带组内两个标签，会被重复计入。
    pub message_count: i64,
    /// 组内标签自身的邮件数之和（含重复计数）。仅用于展示「标签数」以外的参考，
    /// 与 `message_count` 的差值即重叠量。
    pub raw_message_count: i64,
    /// AI 整理维度模式：`"off"`（默认，不参与）/ `"select"`（从组内现有标签选一个）
    /// / `"create"`（可从组内选，也可创建新标签并自动入组）。
    pub ai_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    pub filename: String,
    pub mime: String,
    pub size: usize,
}

/// 邮件详情（含正文）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDetail {
    pub header: MessageHeader,
    pub to_addrs: String,
    pub cc_addrs: String,
    pub message_id: String,
    pub body_text: String,
    pub body_html: String,
    pub attachments: Vec<AttachmentMeta>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub folder: String,
    pub fetched: usize,
    pub inserted: usize,
    pub total_on_server: usize,
}

/// 删除邮件的结果。区分「服务器已删」与「仅删了本地缓存」两种情况，
/// 便于前端如实告知用户，而不是笼统提示「已删除」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteReport {
    /// 实际从本地缓存中移除的条数。
    pub deleted_local: usize,
    /// 是否已在服务器上永久删除。服务器删除失败时为 false（仅有本地生效）。
    pub deleted_remote: bool,
    /// 服务器删除失败时的原因；成功或未尝试服务器删除时为空串。
    pub remote_error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestReport {
    pub imap_ok: bool,
    pub smtp_ok: bool,
    pub imap_message: String,
    pub smtp_message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub text: String,
    pub model: String,
    pub cached: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStatus {
    pub online: bool,
    pub models: Vec<String>,
    pub base_url: String,
    pub model: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    pub base_url: String,
    pub model: String,
    /// 同步完成后自动整理新邮件。缺省视为开启（存量 settings JSON 无此键时由 serde 默认补上）。
    #[serde(default = "default_true")]
    pub auto_classify: bool,
    /// 打开邮件时自动生成摘要（缺省视为开启，兼容存量 settings JSON）。
    #[serde(default = "default_true")]
    pub auto_summarize: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434".to_string(),
            // 选一个体积适中、中文摘要稳定的通用模型。用户可在「AI 设置」中改；
            // 若本机未安装，生成摘要时会自动回退到第一个可用模型。
            model: "qwen3.5:9b".to_string(),
            auto_classify: true,
            auto_summarize: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendPayload {
    pub account_id: String,
    pub to: String,
    #[serde(default)]
    pub cc: String,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub html: bool,
}

/// 一封已解析的邮件（解析层输出，落库前使用）。
#[derive(Debug, Clone, Default)]
pub struct ParsedMail {
    pub uid: u32,
    pub message_id: String,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to_addrs: String,
    pub cc_addrs: String,
    pub date_ts: i64,
    pub seen: bool,
    pub flagged: bool,
    pub body_text: String,
    pub body_html: String,
    pub attachments: Vec<AttachmentMeta>,
    pub has_body: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_settings_default_enables_both_auto_switches() {
        let s = AiSettings::default();
        assert!(s.auto_classify);
        assert!(s.auto_summarize);
    }

    #[test]
    fn ai_settings_legacy_json_without_auto_summarize_defaults_to_true() {
        // 存量 settings JSON 没有 autoSummarize 键，反序列化必须补默认开启，
        // 否则老用户升级后自动摘要会被静默关掉。
        let raw = r#"{"baseUrl":"http://localhost:11434","model":"m","autoClassify":false}"#;
        let s: AiSettings = serde_json::from_str(raw).expect("存量 JSON 应可反序列化");
        assert!(!s.auto_classify);
        assert!(s.auto_summarize, "缺省键应默认开启自动摘要");
    }

    #[test]
    fn ai_settings_explicit_auto_summarize_false_roundtrips() {
        let raw = r#"{"baseUrl":"u","model":"m","autoClassify":true,"autoSummarize":false}"#;
        let s: AiSettings = serde_json::from_str(raw).unwrap();
        assert!(!s.auto_summarize, "显式关闭必须被尊重");
        let out = serde_json::to_string(&s).unwrap();
        assert!(out.contains("\"autoSummarize\":false"));
    }
}
