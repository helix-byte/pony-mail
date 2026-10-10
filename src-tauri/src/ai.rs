use std::time::Duration;

use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::models::{AiSettings, AiStatus, MessageDetail};

const SYSTEM_PROMPT: &str =
    "你是一位专业的邮件助理，负责用简体中文提炼真实邮件的关键信息。";

const USER_TEMPLATE: &str = r#"下面是一封真实收到的邮件。请阅读它，然后输出一个 JSON 对象（只输出 JSON）：

{"summary":"一句话概括这封邮件的主题","points":["2 到 4 条关键要点"],"action":"收件人需要采取的行动，没有则输出 null"}

===== 邮件开始 =====
{content}
===== 邮件结束 =====

要求：summary 一句话；points 每条提炼实际内容（不要照抄发件人、时间等头部信息）；全部使用简体中文（品牌名保留原名）。"#;

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ChatOptions {
    temperature: f32,
    num_predict: i32,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    /// 关闭思考模式（Qwen3 / Qwen3.5 等支持）。
    ///
    /// 这类模型默认会先用大量 token 输出推理过程；实测开启后会把预算烧在英文
    /// 推理上，`content` 为空且 `done_reason: length`，摘要表现为「空结果」。
    /// 显式关闭后，同样的任务耗时从数十秒降到数秒，且直接产出中文摘要。
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    /// 让 Ollama 在采样层面按 JSON 语法约束输出（只让结构化任务使用）。
    ///
    /// 实测：不加也能出合法 JSON 的模型加了更快；对不听话的模型则从机制上
    /// 消除「前后加解释 / 代码块围栏 / 少个括号」这类格式问题。是增强而非必需。
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<&'static str>,
    options: ChatOptions,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: Option<ChatMessageOut>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessageOut {
    #[serde(default)]
    content: String,
    /// 思考类模型（qwen3 / deepseek-r1 等）把推理过程放在这里，
    /// 正文未生成完时 `content` 可能为空字符串。
    #[serde(default)]
    thinking: String,
}

impl ChatMessageOut {
    /// 取模型的有效输出：优先 `content`，为空时回退到 `thinking`。
    fn effective_text(&self) -> &str {
        let c = self.content.trim();
        if !c.is_empty() {
            return c;
        }
        self.thinking.trim()
    }
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Deserialize)]
struct TagEntry {
    #[serde(default)]
    name: String,
}

/// 探测 Ollama 服务可用性并列出已安装模型。
pub async fn status(client: &reqwest::Client, settings: &AiSettings) -> AiStatus {
    let url = format!("{}/api/tags", base_of(settings));
    let mut out = AiStatus {
        online: false,
        models: Vec::new(),
        base_url: settings.base_url.clone(),
        model: settings.model.clone(),
        message: String::new(),
    };

    match client.get(&url).timeout(Duration::from_secs(5)).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<TagsResponse>().await {
            Ok(tags) => {
                out.online = true;
                out.models = tags
                    .models
                    .into_iter()
                    .map(|m| m.name)
                    .filter(|n| !n.is_empty())
                    .collect();
                // 配置的模型不在本机时给出明确提示，避免用户反复点「生成摘要」才排查。
                if !out.models.iter().any(|m| m == &out.model) {
                    out.message = format!(
                        "已连接，检测到 {} 个模型；但配置的「{}」未安装，\
                         生成摘要时会自动改用其他可用模型。",
                        out.models.len(),
                        out.model
                    );
                } else {
                    out.message = format!("已连接，检测到 {} 个模型", out.models.len());
                }
                out
            }
            Err(e) => {
                out.message = format!("响应解析失败：{e}");
                out
            }
        },
        Ok(resp) => {
            out.message = format!("服务返回 HTTP {}", resp.status());
            out
        }
        Err(e) => {
            out.message = if e.is_connect() {
                "无法连接，请确认 Ollama 已启动（ollama serve）".to_string()
            } else if e.is_timeout() {
                "连接超时".to_string()
            } else {
                e.to_string()
            };
            out
        }
    }
}

/// 摘要的 JSON 输出载荷（模型被要求输出的结构）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SummaryPayload {
    pub summary: String,
    pub points: Vec<String>,
    pub action: Option<String>,
}

/// 调用本地模型生成邮件摘要，组装为既有的三节文本。
///
/// 输出走 `format:"json"` 单对象（实测 2026-10-10：自由文本模式下 qwen2.5:7b
/// 对营销/通知类邮件频繁复读与角色跑偏——把邮件当"待校对的文档"、复读指令与
/// 输入特征，38~60 秒烧完预算；JSON 约束把模型锁进"填字段"角色后 4/4 稳定、
/// 10~16 秒、无复读）。JSON 合法但缺 summary 视为跑偏，重试一发。
pub async fn summarize(
    client: &reqwest::Client,
    settings: &AiSettings,
    detail: &MessageDetail,
) -> AppResult<String> {
    let content = build_content(detail);
    if content.trim().is_empty() {
        return Ok("【一句话概括】无有效内容".to_string());
    }

    let first = run_with_recovery(client, settings, |model| {
        request_summary(client, settings, model, &content)
    })
    .await?;
    if let Some(payload) = parse_summary(&first) {
        return Ok(render_summary(&payload));
    }

    // 输出跑偏（JSON 非法 / 缺 summary 字段）→ 重试一发；仍失败不再硬凑，
    // 不把垃圾写进缓存。
    let retry = run_with_recovery(client, settings, |model| {
        request_summary(client, settings, model, &content)
    })
    .await?;
    parse_summary(&retry)
        .map(|payload| render_summary(&payload))
        .ok_or_else(|| {
            AppError::Other(format!(
                "模型「{}」连续两次未能按 JSON 格式输出摘要。可尝试在「设置」中更换模型后重试。",
                settings.model
            ))
        })
}

/// 提取第一个 JSON 对象：剥 ```` ```json ```` / ```` ``` ```` 围栏，取首 `{` 到末 `}`。
///
/// `format:"json"` 下通常用不到，作为兜底保留。分类与摘要两条链路共用。
pub(crate) fn extract_json_object(raw: &str) -> Option<&str> {
    let t = raw.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t);
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&t[start..=end])
}

/// 解析摘要 JSON：键名与值形态都做容错——7B 级模型会漂移键名
/// （实测出现「主题 / 关键要点 / 行动」等中文别名，甚至把整封邮件重排成嵌套对象）。
/// 解析不出有效 summary 时返回 `None`（调用方据此判定跑偏并重试）。
pub(crate) fn parse_summary(raw: &str) -> Option<SummaryPayload> {
    let value: serde_json::Value = serde_json::from_str(extract_json_object(raw)?).ok()?;
    let obj = value.as_object()?;

    let mut taken: Vec<String> = Vec::new();
    let (k, summary_value) = pick_key(
        obj,
        &taken,
        &["summary"],
        &["一句话概括", "概括", "摘要", "主题", "概述", "简概"],
    )?;
    taken.push(k.to_string());
    let summary = summary_value.as_str()?.trim().to_string();
    if summary.is_empty() || is_placeholder(&summary) {
        return None;
    }

    let points = pick_key(
        obj,
        &taken,
        &["points"],
        &["关键要点", "要点", "关键信息", "主要内容"],
    )
    .map(|(k, v)| {
        taken.push(k.to_string());
        collect_points(v)
    })
    .unwrap_or_default();

    let action = pick_key(
        obj,
        &taken,
        &["action", "todo"],
        &["需要我做什么", "需要采取的行动", "需要做的", "行动", "下一步", "待办"],
    )
    .and_then(|(_, v)| v.as_str().map(str::trim))
    .filter(|s| !s.is_empty() && !is_placeholder(s))
    .map(str::to_string);

    Some(SummaryPayload {
        summary,
        points,
        action,
    })
}

/// 在响应对象里找一个键（排除已占用者）：精确（ASCII 大小写不敏感）→ 别名精确
/// → 双向包含取任意命中（键 ≥2 字）。
fn pick_key<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    taken: &[String],
    exact: &[&str],
    aliases: &[&str],
) -> Option<(&'a str, &'a serde_json::Value)> {
    let free: Vec<(&str, &serde_json::Value)> = obj
        .iter()
        .filter(|(k, _)| !taken.iter().any(|t| t.as_str() == k.as_str()))
        .map(|(k, v)| (k.as_str(), v))
        .collect();

    for want in exact {
        if let Some((k, v)) = free.iter().find(|(k, _)| k.eq_ignore_ascii_case(want)) {
            return Some((k, v));
        }
    }
    for want in aliases {
        if let Some((k, v)) = free.iter().find(|(k, _)| k.trim() == *want) {
            return Some((k, v));
        }
    }
    for want in aliases {
        if let Some((k, v)) = free.iter().find(|(k, _)| {
            let kk = k.trim();
            kk.chars().count() >= 2 && (kk.contains(*want) || want.contains(kk))
        }) {
            return Some((k, v));
        }
    }
    None
}

/// 把 points 值转成字符串列表：数组逐项取值，字符串按行拆分。
fn collect_points(value: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    let s = s.trim();
                    if !s.is_empty() {
                        out.push(s.to_string());
                    }
                }
            }
        }
        serde_json::Value::String(s) => {
            for line in s.lines() {
                let line = line.trim().trim_start_matches(['-', '*', '•', ' ']).trim();
                if !line.is_empty() {
                    out.push(line.to_string());
                }
            }
        }
        _ => {}
    }
    out
}

/// 组装成既有的三节文本（前端与缓存格式不变）。
pub(crate) fn render_summary(payload: &SummaryPayload) -> String {
    let mut out = format!("【一句话概括】{}\n【关键信息】\n", payload.summary);
    if payload.points.is_empty() {
        out.push_str("- （无）\n");
    } else {
        for point in &payload.points {
            out.push_str(&format!("- {point}\n"));
        }
    }
    let action = payload.action.as_deref().unwrap_or("无");
    out.push_str(&format!("【需要我做什么】{action}"));
    out
}

/// 占位词判定（模型用「无 / null / 不确定」凑字段时视为无值）。
fn is_placeholder(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    matches!(lower.as_str(), "null" | "none" | "nil" | "n/a")
        || matches!(s.trim(), "无" | "没有" | "不确定" | "未知" | "不适用")
}

/// 统一自愈包装：给「用某个模型发一次请求」的操作加上两类恢复。
///
/// 1. **模型未安装**（`ModelNotFound`）：自动换本机第一个可用模型重试一次，
///    避免开箱即报错；
/// 2. **runner 病态**（`ModelBusy`：复读保护 500 或 HTTP 200 空壳，两者实测同源）：
///    卸载模型、稍候、重试一次。实测卸载后立刻重试常常仍然病态（同一输入会
///    先成功后失败，且多模型同时驻留会显著恶化），所以留一拍让 runner 退出重载。
///    再失败则按调用方的连续失败计数/熔断处理，避免无限自愈。
pub(crate) async fn run_with_recovery<F, Fut>(
    client: &reqwest::Client,
    settings: &AiSettings,
    call: F,
) -> AppResult<String>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = AppResult<String>>,
{
    match call(settings.model.clone()).await {
        Err(AppError::ModelNotFound(_)) => {
            let fallback = first_available_model(client, settings).await?;
            call(fallback).await
        }
        Err(AppError::ModelBusy(_)) => {
            let _ = unload_model(client, settings, &settings.model).await;
            // 实测（2026-10-09，本机 0.40.2 + qwen3.5:9b）：
            // - runner 进入病态后不会自发恢复——不卸载的话后续请求持续空壳（跨分钟级）；
            // - 卸载后立即/1.5s 重试仍空壳（runner 尚未退干净），冷却 5s 后重载即恢复
            //   （恢复后多个请求连续健康）。
            // 因此等待取 5s：短暂延迟换自愈成功率；仍失败则由调用方计连续失败并熔断。
            tokio::time::sleep(Duration::from_millis(5000)).await;
            match call(settings.model.clone()).await {
                Ok(text) => Ok(text),
                // 重载后仍然病态，说明不是短暂的 runner 状态问题。给出可执行的建议；
                // 调用方（批量整理）会把它计入连续失败并最终熔断。
                Err(AppError::ModelBusy(_)) => Err(AppError::Other(format!(
                    "模型「{}」连续两次陷入异常状态（重复循环或空响应）。可尝试：\
                     ①在「设置」中改选其他模型（如 qwen2.5:14b / deepseek-r1:7b）；\
                     ②重启 Ollama 服务后重试。",
                    settings.model
                ))),
                other => other,
            }
        }
        other => other,
    }
}

/// 结构化分类请求：`format:"json"` + `think:false` + `temperature:0`。
///
/// 返回模型输出的 JSON 原文（解析与容错由 `classify` 模块负责）。
/// 与摘要共用 [`run_with_recovery`] 的自愈链路，参数差异只在本函数里；
/// `model` 按值接收的原因同 [`request_summary`]。
pub(crate) async fn chat_json(
    client: &reqwest::Client,
    settings: &AiSettings,
    model: String,
    system: &str,
    user: &str,
    num_predict: i32,
) -> AppResult<String> {
    let url = format!("{}/api/chat", base_of(settings));
    let body = ChatRequest {
        model: model.clone(),
        messages: vec![
            ChatMessage {
                role: "system",
                content: system.to_string(),
            },
            ChatMessage {
                role: "user",
                content: user.to_string(),
            },
        ],
        stream: false,
        think: Some(false),
        format: Some("json"),
        options: ChatOptions {
            // 分类同属确定性任务：温度取 0 压住小模型的格式抖动。
            temperature: 0.0,
            num_predict,
        },
    };

    let resp = client
        .post(&url)
        .json(&body)
        // 分类输出很短，但首次调用可能含模型加载；3 分钟足够覆盖冷启动。
        .timeout(Duration::from_secs(180))
        .send()
        .await?;

    if !resp.status().is_success() {
        let code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if let Some(mapped) = map_ollama_error(&text) {
            return Err(mapped);
        }
        return Err(AppError::Other(format!(
            "AI 服务返回 {code}：{}",
            truncate(&text, 200)
        )));
    }

    let parsed: ChatResponse = resp.json().await?;
    extract_content(parsed, &model)
}

/// 请求 Ollama 立即卸载指定模型（释放 runner / 显存）。
///
/// 用于从「runner 卡死导致复读保护误报」的状态中自愈。忽略一切错误：
/// 卸载失败不应影响主流程，交给后续重试或错误提示处理。
pub async fn unload_model(client: &reqwest::Client, settings: &AiSettings, model: &str) -> bool {
    let url = format!("{}/api/generate", base_of(settings));
    let payload = serde_json::json!({ "model": model, "keep_alive": 0 });
    matches!(
        client.post(&url).json(&payload).timeout(Duration::from_secs(30)).send().await,
        Ok(resp) if resp.status().is_success()
    )
}

/// 列出本机已安装的模型名。
pub async fn list_models(client: &reqwest::Client, settings: &AiSettings) -> Vec<String> {
    let url = format!("{}/api/tags", base_of(settings));
    match client.get(&url).timeout(Duration::from_secs(5)).send().await {
        Ok(resp) if resp.status().is_success() => resp
            .json::<TagsResponse>()
            .await
            .map(|t| {
                t.models
                    .into_iter()
                    .map(|m| m.name)
                    .filter(|n| !n.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

async fn first_available_model(
    client: &reqwest::Client,
    settings: &AiSettings,
) -> AppResult<String> {
    let models = list_models(client, settings).await;
    models.into_iter().next().ok_or_else(|| {
        AppError::Other(format!(
            "本机没有可用的 Ollama 模型。请先执行 `ollama pull qwen3.5:9b`，\
             或在「设置」中检查服务地址（当前：{}）",
            settings.base_url
        ))
    })
}

/// 用指定模型发一次摘要请求。
///
/// `model` 按值接收：调用方是 `run_with_recovery` 的闭包，闭包返回的 future
/// 不能借用闭包局部变量（生命周期逃逸），按值 move 进 future 才能成立。
async fn request_summary(
    client: &reqwest::Client,
    settings: &AiSettings,
    model: String,
    content: &str,
) -> AppResult<String> {
    let url = format!("{}/api/chat", base_of(settings));
    let body = ChatRequest {
        model: model.clone(),
        messages: vec![
            ChatMessage {
                role: "system",
                content: SYSTEM_PROMPT.to_string(),
            },
            ChatMessage {
                role: "user",
                content: USER_TEMPLATE.replace("{content}", content),
            },
        ],
        stream: false,
        think: Some(false),
        // 摘要同样走 JSON 语法约束（见 summarize 的注释：防复读与角色跑偏）。
        format: Some("json"),
        options: ChatOptions {
            // 摘要属确定性任务，温度取 0 可显著提升小模型的格式稳定性。
            temperature: 0.0,
            // 摘要（含要点）约 300 字；预算给太大会让 4B 级模型重复输出、跑偏格式。
            num_predict: 512,
        },
    };

    let resp = client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(180))
        .send()
        .await?;

    if !resp.status().is_success() {
        let code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if let Some(mapped) = map_ollama_error(&text) {
            return Err(mapped);
        }
        return Err(AppError::Other(format!(
            "AI 服务返回 {code}：{}",
            truncate(&text, 200)
        )));
    }

    let parsed: ChatResponse = resp.json().await?;
    extract_content(parsed, &model)
}

/// 从 Ollama 响应中提取有效输出；空内容映射为 [`AppError::ModelBusy`]。
///
/// HTTP 200 空壳响应（`{"model":"","created_at":"0001-01-01T00:00:00Z",...}`，
/// message 存在但 content 为空）与复读保护 500 实测**同源**，都是 runner 的
/// 时间性病态（同一输入会先成功后失败），必须走「卸载 → 稍候 → 重试」的自愈
/// 链路，绝不能把空内容当成「模型判定为空」静默消费。
fn extract_content(parsed: ChatResponse, model: &str) -> AppResult<String> {
    if let Some(err) = parsed.error {
        if err.contains("not found") {
            return Err(AppError::ModelNotFound(model.to_string()));
        }
        return Err(AppError::Other(format!("AI 服务错误：{err}")));
    }

    let text = parsed
        .message
        .as_ref()
        .map(|m| m.effective_text())
        .unwrap_or_default()
        .trim()
        .to_string();

    if text.is_empty() {
        return Err(AppError::ModelBusy(format!(
            "Ollama 返回空响应（模型：{model}）"
        )));
    }
    Ok(text)
}

fn base_of(settings: &AiSettings) -> String {
    settings.base_url.trim().trim_end_matches('/').to_string()
}

/// 把 Ollama 的错误体翻译成可分类的错误；无法识别时返回 `None`，由调用方兜底。
///
/// 重点是复读保护：它返回 HTTP 500 + `prediction aborted, token repeat limit
/// reached`。实测根因有两层——
/// 1. 正文里存在高度重复的 token 序列（零宽填充字符、重复句），已在
///    [`reduce_noise`] 中处理；
/// 2. **Ollama 的 runner 会偶发卡死**，同样输入时好时坏，卸载模型再重载即可恢复。
///
/// 因此这里映射为 [`AppError::ModelBusy`]，让 [`summarize`] 能自动「卸载 → 重试」，
/// 而不是直接把英文原文抛给用户。
fn map_ollama_error(text: &str) -> Option<AppError> {
    if text.contains("token repeat limit") || text.contains("prediction aborted") {
        return Some(AppError::ModelBusy(
            "Ollama 报告生成重复循环".to_string(),
        ));
    }
    None
}

/// 组装给模型的邮件正文：优先纯文本，其次从 HTML 提取。
///
/// 正文先做「降噪」再截断——营销邮件里常带大量重复结构的促销条目、跟踪参数和
/// 超长带 token 的 URL，既占预算又干扰提炼。（注意：实测 Ollama 的
/// `token repeat limit` 报错根因在模型权重，与本函数无关，见 [`map_ollama_error`]。）
fn build_content(detail: &MessageDetail) -> String {
    let raw = if !detail.body_text.trim().is_empty() {
        detail.body_text.clone()
    } else {
        strip_html(&detail.body_html)
    };
    // 正文上限 1200 字（2026-10-10 实测）：失败/复读集中在大输入上；JSON 约束
    // 虽大幅降低长度敏感性，仍保持小输入以进一步压复读风险、加快推理。
    let body = truncate(&reduce_noise(&raw), 1200);

    format!(
        "发件人：{} <{}>\n收件人：{}\n时间：{}\n主题：{}\n\n正文：\n{}",
        detail.header.from_name,
        detail.header.from_addr,
        detail.to_addrs,
        format_time(detail.header.date_ts),
        detail.header.subject,
        body
    )
}

/// 降低正文噪音：去掉不可见填充字符与 URL、折叠连续重复行/句、压缩空白、
/// 过滤结构性垃圾行（CSS 残渣 / 纯符号行）、全局面级去重。
///
/// 核心目的是削弱「高度重复的 token 序列」——
/// - 营销邮件用零宽字符填充预览文本（见 [`strip_invisible`]）
/// - 用重复促销条目堆正文
/// - 用同一句话重复占位（清掉零宽字符后常会被并到同一行）
///
/// 这些都会让小模型陷入复读，触发 Ollama 的
/// `prediction aborted, token repeat limit reached`。
///
/// 2026-10-10 增强（实测驱动）：HTML 降级提取的 `body_text` 里常有成片的
/// `sup { font-size:100% !important; }` 样式残渣与图片 alt 留下的 `( )` 纯符号行，
/// 实测直接诱发模型复读符号本身；重复行也不再要求"连续"——被空行分隔的重复
/// （实测的 `( )\n\n( )` 形态）同样照抄诱发复读，故改为**全局面级去重**。
///
/// `pub(crate)`：智能整理的分类输入也要先过同一套降噪。
pub(crate) fn reduce_noise(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut prev_line: Option<String> = None;

    for line in text.lines() {
        let cleaned = strip_urls(line);
        let collapsed = collapse_repeats(&collapse_ws(&cleaned));
        // 结构性垃圾行（CSS 残渣 / 纯符号）直接丢弃。
        if is_junk_line(&collapsed) {
            continue;
        }
        if collapsed.is_empty() {
            // 保留段落分隔，但避免连续空行堆积。
            if matches!(out.last(), Some(l) if !l.is_empty()) {
                out.push(String::new());
            }
            prev_line = None;
            continue;
        }
        // 丢弃与上一行完全相同的行（促销邮件常见的大段重复条目）。
        if prev_line.as_deref() == Some(collapsed.as_str()) {
            continue;
        }
        // 全局面级去重：重复行从第二次出现起一律丢弃。
        if !seen.insert(collapsed.clone()) {
            continue;
        }
        prev_line = Some(collapsed.clone());
        out.push(collapsed);
    }

    // 去掉首尾空行。
    while matches!(out.first(), Some(l) if l.is_empty()) {
        out.remove(0);
    }
    while matches!(out.last(), Some(l) if l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// 结构性垃圾行判定：HTML 降级提取的正文里的 CSS 残渣与纯符号行。
///
/// - 含 `!important` / `mso-` → CSS 声明残渣；
/// - 含 `{` 或 `}` → CSS 块残渣（正文里的花括号极罕见）；
/// - 没有任何字母/数字/汉字（如 `( )`、`****`）→ 图片 alt 占位或装饰线。
fn is_junk_line(line: &str) -> bool {
    if line.is_empty() {
        return false; // 空行承担分段语义，交给调用方处理
    }
    if line.contains("!important") || line.contains("mso-") {
        return true;
    }
    if line.contains('{') || line.contains('}') {
        return true;
    }
    !line.chars().any(|c| c.is_alphanumeric())
}

/// 折叠行内「相邻重复的句子/短语」。
///
/// 例：`Register for more details and your chance to attend. Register for more
/// details and your chance to attend.` → 只保留一句。
/// 这类重复在清掉零宽字符后会显现出来，是让模型复读的直接诱因。
fn collapse_repeats(line: &str) -> String {
    // 按句末标点切句，保留标点。中英文句末符号都覆盖。
    let mut sentences: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in line.chars() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?' | '。' | '！' | '？' | '；' | ';') {
            sentences.push(std::mem::take(&mut cur));
        }
    }
    if !cur.trim().is_empty() {
        sentences.push(cur);
    }
    // 只有一句（或无句末标点）时，退化为「短语级」折叠，处理无标点的重复。
    if sentences.len() <= 1 {
        return collapse_adjacent_phrases(line);
    }

    let mut kept: Vec<String> = Vec::new();
    let mut prev: Option<String> = None;
    for s in sentences {
        let key = s.trim().to_string();
        if prev.as_deref() == Some(key.as_str()) {
            continue; // 与上一句完全相同 → 丢弃
        }
        prev = Some(key);
        kept.push(s.trim().to_string());
    }
    kept.join(" ")
}

/// 短语级兜底：把同一行内连续重复的整段前缀折叠掉。
///
/// 处理 `abcabcabc` 或 `abc abc abc` 这类无句末标点的重复（常见于被拼接的填充文本）。
fn collapse_adjacent_phrases(line: &str) -> String {
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.len() < 2 {
        return line.trim().to_string();
    }
    let mut kept: Vec<&str> = Vec::with_capacity(words.len());
    for w in words {
        // 连续重复的同一个词只留一个。
        if kept.last() == Some(&w) {
            continue;
        }
        kept.push(w);
    }
    // 再尝试折叠「相邻的等长词块」重复（如 "a b a b a b"）。
    let n = kept.len();
    for size in 1..=n / 2 {
        if !n.is_multiple_of(size) {
            continue;
        }
        let blocks = n / size;
        if blocks < 2 {
            continue;
        }
        let first = &kept[0..size];
        if (1..blocks).all(|i| &kept[i * size..(i + 1) * size] == first) {
            return first.join(" ");
        }
    }
    kept.join(" ")
}

/// 删除行内的噪音：不可见填充字符、`<https://...>`、裸 `http(s)://...`。
fn strip_urls(line: &str) -> String {
    // 先清掉零宽/方向类填充字符——否则它们会留在词间干扰后续切分与模型。
    let mut s = strip_invisible(line);

    // 处理尖括号包裹的 URL：整段（含尖括号）删掉。
    while let (Some(a), Some(b)) = (s.find('<'), s.find('>')) {
        if b <= a {
            break;
        }
        let inner = &s[a + 1..b];
        if inner.starts_with("http://") || inner.starts_with("https://") {
            s.replace_range(a..=b, " ");
            continue;
        }
        // 不是 URL 的尖括号（如 <br>）保留，避免死循环。
        break;
    }

    // 再处理裸 URL：按空白切分，丢弃以 http(s) 开头的片段。
    let kept: Vec<&str> = s
        .split_whitespace()
        .filter(|w| !w.starts_with("http://") && !w.starts_with("https://"))
        .collect();
    kept.join(" ")
}

/// 把连续空白压成单个空格。
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 删掉不可见的填充字符。
///
/// 营销邮件常在预览文本（preheader）里塞进成百上千个零宽字符来占位——实测微软、
/// 会议邀请类邮件的 `body_text` 开头是几十组重复的
/// `\u{200c} \u{00a0} ...`（零宽非连接符 + 不换行空格 + 组合字素连接符）。
/// 这些字符肉眼不可见，却会构成**高度重复的 token 序列**，直接把模型推进复读，
/// 触发 Ollama 的 `prediction aborted, token repeat limit reached`。
///
/// 注意 `split_whitespace()` 只认 `\u{00a0}` 这类标准空白，认不出 `\u{200c}`/`\u{034f}`，
/// 所以必须在降噪阶段单独清除。
fn strip_invisible(s: &str) -> String {
    const INVISIBLE: &[char] = &[
        '\u{200b}', // 零宽空格
        '\u{200c}', // 零宽非连接符
        '\u{200d}', // 零宽连接符
        '\u{2060}', // word joiner
        '\u{feff}', // 零宽不换行空格 / BOM
        '\u{034f}', // 组合字素连接符（邮件里伪装成空白用）
        '\u{00ad}', // 软连字符
        '\u{180e}', // 蒙古文元音分隔符
        '\u{200e}', // 从左至右标记
        '\u{200f}', // 从右至左标记
        '\u{202a}', // 以下方向类控制符
        '\u{202b}',
        '\u{202c}',
        '\u{202d}',
        '\u{202e}',
        '\u{2066}',
        '\u{2067}',
        '\u{2068}',
        '\u{2069}',
    ];
    s.chars().filter(|c| !INVISIBLE.contains(c)).collect()
}

pub(crate) fn format_time(ts: i64) -> String {
    if ts <= 0 {
        return "未知".to_string();
    }
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "未知".to_string())
}

/// 极简 HTML 转文本：去掉 script/style 与标签，还原常见实体。
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut skip_depth = 0i32;
    let bytes = html.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            let end = html[i..].find('>').map(|p| i + p).unwrap_or(bytes.len());
            let tag = html[i + 1..end].trim().to_ascii_lowercase();
            if tag.starts_with("script") || tag.starts_with("style") {
                skip_depth += 1;
            } else if tag.starts_with("/script") || tag.starts_with("/style") {
                skip_depth = (skip_depth - 1).max(0);
            } else if skip_depth == 0
                && (tag.starts_with("br")
                    || tag.starts_with("p")
                    || tag.starts_with("/p")
                    || tag.starts_with("div")
                    || tag.starts_with("/div")
                    || tag.starts_with("tr")
                    || tag.starts_with("li"))
            {
                out.push('\n');
            }
            i = end + 1;
            continue;
        }
        if skip_depth == 0 {
            let ch = html[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        } else {
            i += 1;
        }
    }

    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let cut: String = s.chars().take(max_chars).collect();
    format!("{cut}……（内容已截断）")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> ChatResponse {
        serde_json::from_str(json).expect("样例 JSON 应能解析")
    }

    /// 回归测试：思考类模型把内容放在 `thinking`、`content` 为空时，
    /// 必须回退到 thinking，否则摘要会报「模型返回了空结果」。
    #[test]
    fn falls_back_to_thinking_when_content_empty() {
        let r = parse(r#"{"message":{"role":"assistant","content":"","thinking":"思考过程内容"}}"#);
        let m = r.message.unwrap();
        assert_eq!(m.effective_text(), "思考过程内容");
    }

    /// 正常模型（无 thinking 字段）应直接取 content。
    #[test]
    fn prefers_content_when_present() {
        let r = parse(r#"{"message":{"role":"assistant","content":"正式摘要"}}"#);
        let m = r.message.unwrap();
        assert_eq!(m.effective_text(), "正式摘要");
    }

    /// 两者都有时优先 content。
    #[test]
    fn content_wins_over_thinking() {
        let r = parse(
            r#"{"message":{"role":"assistant","content":"正文","thinking":"推理"}}"#,
        );
        let m = r.message.unwrap();
        assert_eq!(m.effective_text(), "正文");
    }

    /// 纯空白 content 也要回退到 thinking。
    #[test]
    fn blank_content_falls_back_to_thinking() {
        let r = parse(r#"{"message":{"role":"assistant","content":"   \n","thinking":"推理"}}"#);
        let m = r.message.unwrap();
        assert_eq!(m.effective_text(), "推理");
    }

    /// Ollama 的模型不存在响应：HTTP 200 + error 字段。
    #[test]
    fn parses_model_not_found_error() {
        let r = parse(r#"{"error":"model 'qwen2.5:7b' not found"}"#);
        assert!(r.error.unwrap().contains("not found"));
        assert!(r.message.is_none());
    }

    /// 真实响应样例（截取自支持思考模式的模型实测）不应解析失败。
    #[test]
    fn parses_realistic_thinking_response() {
        let r = parse(
            r#"{"model":"qwen3.5:9b","message":{"role":"assistant","content":"","thinking":"Thinking Process:/n/n1. Analyze..."},"done":true,"done_reason":"length"}"#,
        );
        let m = r.message.unwrap();
        assert!(!m.effective_text().is_empty(), "thinking 应作为回退内容");
    }

    /// build_content 在只有 HTML 正文时也要产出可用内容。
    #[test]
    fn strip_html_produces_readable_text() {
        let html = "<html><head><style>p{color:red}</style></head><body><p>你好</p><p>世界</p></body></html>";
        let out = strip_html(html);
        assert!(out.contains("你好"), "应保留正文：{out}");
        assert!(out.contains("世界"));
        assert!(!out.contains("color:red"), "style 内容应被移除：{out}");
        assert!(!out.contains('<'), "不应残留标签：{out}");
    }

    /// 回归测试（关键）：请求体必须带 `"think": false`。
    ///
    /// 实测 qwen3.5:4b 开启思考时会把 2048 token 全烧在英文推理上，`content` 为空、
    /// `done_reason: length`，摘要直接失败；关闭后约 5 秒产出中文摘要。
    /// 少这个字段就会退回「AI 摘要返回空」的旧 bug。
    #[test]
    fn request_body_disables_thinking() {
        let body = ChatRequest {
            model: "qwen3.5:4b".to_string(),
            messages: vec![],
            stream: false,
            think: Some(false),
            format: None,
            options: ChatOptions {
                temperature: 0.2,
                num_predict: 1024,
            },
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(
            json.contains(r#""think":false"#),
            "请求体必须显式关闭思考模式：{json}"
        );
    }

    /// `think` 为 None 时不序列化该字段，保证对不支持它的老版本 Ollama 也能用。
    #[test]
    fn request_body_omits_think_when_none() {
        let body = ChatRequest {
            model: "m".to_string(),
            messages: vec![],
            stream: false,
            think: None,
            format: None,
            options: ChatOptions {
                temperature: 0.2,
                num_predict: 64,
            },
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(!json.contains("think"), "None 时不应输出该字段：{json}");
    }

    /// 分类请求必须带 `"format":"json"`（语法层约束模型输出合法 JSON）。
    #[test]
    fn request_body_sets_json_format_when_requested() {
        let body = ChatRequest {
            model: "m".to_string(),
            messages: vec![],
            stream: false,
            think: Some(false),
            format: Some("json"),
            options: ChatOptions {
                temperature: 0.0,
                num_predict: 256,
            },
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains(r#""format":"json""#), "分类请求应约束 JSON：{json}");
    }

    /// 摘要请求不带 `format`，保持原有纯文本输出契约。
    #[test]
    fn request_body_omits_format_when_none() {
        let body = ChatRequest {
            model: "m".to_string(),
            messages: vec![],
            stream: false,
            think: Some(false),
            format: None,
            options: ChatOptions {
                temperature: 0.0,
                num_predict: 512,
            },
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(!json.contains("format"), "未指定时不应输出该字段：{json}");
    }

    /// 回归测试（关键）：HTTP 200 空壳响应必须映射为 `ModelBusy` 走自愈，
    /// 绝不能静默当成「模型判定为空」。
    ///
    /// 空壳原文取自本机实测（与复读 500 同源的 runner 病态，curl 直连亦可复现）。
    #[test]
    fn empty_shell_response_maps_to_model_busy() {
        let r = parse(
            r#"{"model":"","created_at":"0001-01-01T00:00:00Z","message":{"role":"","content":""},"done":false}"#,
        );
        let err = extract_content(r, "qwen3.5:9b").expect_err("空壳应报错");
        assert!(
            matches!(err, AppError::ModelBusy(_)),
            "空壳应分类为 ModelBusy 以便自动重试，实际：{err:?}"
        );
    }

    /// 模型不存在的响应经 extract_content 应分类为 ModelNotFound（走模型回退）。
    #[test]
    fn extract_content_maps_model_not_found() {
        let r = parse(r#"{"error":"model 'qwen2.5:7b' not found"}"#);
        let err = extract_content(r, "qwen2.5:7b").expect_err("不应成功");
        assert!(matches!(err, AppError::ModelNotFound(_)), "应分类为 ModelNotFound：{err:?}");
    }

    /// 回归测试：Ollama 的复读保护必须被识别为「可自愈」类错误。
    ///
    /// 实测该错误有两个来源：正文含高度重复 token 序列（见 `reduce_noise` 系列测试），
    /// 以及 **Ollama runner 偶发卡死**——同样输入时好时坏，卸载模型重载即恢复。
    /// 因此必须映射为 `ModelBusy`，让 `summarize` 能自动「卸载 → 重试」。
    #[test]
    fn maps_repeat_limit_error_to_model_busy() {
        let err = map_ollama_error("prediction aborted, token repeat limit reached")
            .expect("复读保护错误应被识别");
        assert!(
            matches!(err, AppError::ModelBusy(_)),
            "应分类为 ModelBusy 以便自动重试，实际：{err:?}"
        );
    }

    /// 非复读类的错误体不应被误判为 ModelBusy。
    #[test]
    fn does_not_map_unknown_errors() {
        assert!(map_ollama_error("something else went wrong").is_none());
        assert!(map_ollama_error("").is_none());
    }

    /// 尖括号包裹的 URL 整段删掉，正文其余部分保留。
    #[test]
    fn strip_urls_removes_angle_bracket_url() {
        let line = "详见 <https://store.steampowered.com/x?token=abc123> 谢谢";
        let out = strip_urls(line);
        assert!(!out.contains("http"), "URL 应被移除：{out}");
        assert!(out.contains("详见") && out.contains("谢谢"), "正文应保留：{out}");
    }

    /// 裸 URL（无尖括号）也要删掉，且不残留跟踪参数。
    #[test]
    fn strip_urls_removes_bare_url() {
        let line = "点击 https://example.com/a?b=c&d=e 领取";
        let out = strip_urls(line);
        assert!(!out.contains("http") && !out.contains("token"), "URL 应被移除：{out}");
        assert!(out.contains("点击") && out.contains("领取"), "正文应保留：{out}");
    }

    /// 非 URL 的尖括号（如 HTML 残留 `<br>`）不应导致死循环或被误删。
    #[test]
    fn strip_urls_keeps_non_url_angle_brackets() {
        let out = strip_urls("第一行<br>第二行");
        assert!(out.contains("第一行") && out.contains("第二行"), "内容应保留：{out}");
    }

    /// 回归测试（关键）：零宽字符必须被清除。
    ///
    /// 实测微软 / 会议邀请类邮件的 `body_text` 开头是几十组重复的
    /// `\u{200c}\u{00a0}\u{034f}`——肉眼不可见却构成高度重复 token 序列，
    /// 直接把模型推进复读。修掉后这些邮件才能正常生成摘要。
    #[test]
    fn strip_invisible_removes_zero_width_fillers() {
        let padded = "\u{200c}\u{00a0}\u{034f}\u{200c}\u{00a0}\u{034f}正文开始";
        let out = strip_invisible(padded);
        assert!(!out.contains('\u{200c}'), "零宽非连接符应被清除：{out:?}");
        assert!(!out.contains('\u{034f}'), "组合字素连接符应被清除：{out:?}");
        assert!(out.contains("正文开始"), "正文应保留：{out:?}");

        // 不换行空格是标准空白，由 `collapse_ws` 负责压缩（两者职责分离）。
        let with_nbsp = collapse_ws(&strip_invisible("\u{200c}\u{00a0}\u{200c}\u{00a0}正文"));
        assert_eq!(with_nbsp, "正文", "零宽 + 不换行空格应被联合清除：{with_nbsp:?}");
    }

    /// 真实场景：带零宽填充的长行经 `strip_urls` + `collapse_ws` 后应变得很短。
    #[test]
    fn strip_urls_collapses_invisible_padding() {
        let filler = "\u{200c} \u{00a0} ͏ ".repeat(200); // 模拟预览文本填充
        let line = format!("{filler}Register for more details.");
        let out = collapse_ws(&strip_urls(&line));
        assert_eq!(out, "Register for more details.", "填充应被清除：{out:?}");
    }

    /// 整封带填充的邮件经 `reduce_noise` 后应被大幅压缩。
    #[test]
    fn reduce_noise_strips_preheader_padding() {
        let filler = "\u{200c} \u{00a0} ͏ ".repeat(300);
        let src = format!("{filler}正文一\n\n正文二");
        let out = reduce_noise(&src);
        assert!(out.len() < 40, "应被大幅压缩，实际 {} 字符：{out:?}", out.len());
        assert!(out.contains("正文一") && out.contains("正文二"));
    }

    /// 回归测试（关键）：行内相邻重复的句子必须折叠。
    ///
    /// 实测会议邀请类邮件清掉零宽字符后，同一句话会被并到一行重复多次
    /// （`Register for more details... . Register for more details... .`），
    /// 直接把模型推进复读。
    #[test]
    fn collapse_repeats_folds_duplicate_sentences() {
        let line = "Register for more details. Register for more details. See you there.";
        let out = collapse_repeats(line);
        assert_eq!(
            out.matches("Register for more details.").count(),
            1,
            "重复句应只留一句：{out:?}"
        );
        assert!(out.contains("See you there."), "其他句子应保留：{out:?}");
    }

    /// 中文句子重复同样要折叠。
    #[test]
    fn collapse_repeats_folds_chinese_sentences() {
        let line = "请尽快确认。请尽快确认。谢谢。";
        let out = collapse_repeats(line);
        assert_eq!(out.matches("请尽快确认").count(), 1, "重复中文句应折叠：{out:?}");
        assert!(out.contains("谢谢"));
    }

    /// 无标点的连续词块重复也要折叠（`a b a b a b` → `a b`）。
    #[test]
    fn collapse_repeats_folds_adjacent_word_blocks() {
        assert_eq!(collapse_repeats("a b a b a b"), "a b");
    }

    /// 连续重复的单个词只保留一个。
    #[test]
    fn collapse_repeats_folds_duplicate_words() {
        assert_eq!(collapse_repeats("谢谢 谢谢 谢谢"), "谢谢");
    }

    /// 正常文本（无重复）不应丢失内容。
    ///
    /// 句子会用空格重新拼接，因此不做逐字节相等断言，只校验语义保留：
    /// 原文里的每个句子都应完整出现。
    #[test]
    fn collapse_repeats_keeps_normal_text() {
        let out = collapse_repeats("会议将于周三下午三点在会议室举行。请准时参加。");
        assert!(out.contains("会议将于周三下午三点在会议室举行。"), "{out:?}");
        assert!(out.contains("请准时参加。"), "{out:?}");
        // 无重复内容，句子数应保持 2。
        assert_eq!(out.matches('。').count(), 2, "不应增删句子：{out:?}");
    }

    /// 连续重复行折叠成一行（促销邮件常见）。
    #[test]
    fn reduce_noise_collapses_consecutive_duplicate_lines() {
        let src = "标题\n重复条目\n重复条目\n重复条目\n结尾";
        let out = reduce_noise(src);
        assert_eq!(out.matches("重复条目").count(), 1, "重复行应被折叠：{out}");
        assert!(out.contains("标题") && out.contains("结尾"));
    }

    /// 降噪：删 URL + 折叠重复 + 压缩空白 + 全局去重 + 去掉首尾空行。
    #[test]
    fn reduce_noise_end_to_end() {
        let src = "\n\n  A   B  \n<https://x.com/1>\nA B\n\n\nC\n\n";
        let out = reduce_noise(src);
        assert_eq!(out, "A B\n\nC", "降噪结果不符：{out:?}");
    }

    /// 全局面级去重：被空行/URL 分隔的重复行（实测的 `( )\n\n( )` 形态）也丢弃
    /// ——模型会照抄这类重复，仅折叠"连续"重复不够。
    #[test]
    fn reduce_noise_dedupes_across_paragraphs() {
        let out = reduce_noise("A B\n<https://x.com/1>\nA B");
        assert_eq!(out, "A B", "被分隔的重复行应全局去重：{out:?}");
    }

    /// 结构性垃圾行（CSS 残渣 / 纯符号）直接丢弃；含文字的正文行保留。
    #[test]
    fn reduce_noise_drops_css_and_symbol_junk() {
        let src = "Microsoft Rewards Logo ( )\n\n( )\n\nsup, sub {\nfont-size:100% !important; }\n\n正文第一行\n****\n正文第二行";
        let out = reduce_noise(src);
        assert!(
            out.contains("正文第一行") && out.contains("正文第二行"),
            "正文应保留：{out:?}"
        );
        assert!(
            out.contains("Microsoft Rewards Logo"),
            "含文字的图片 alt 行应保留：{out:?}"
        );
        assert!(
            !out.lines().any(|l| l.trim() == "( )" || l.trim() == "****"),
            "纯符号行应被丢弃：{out:?}"
        );
        assert!(
            !out.contains("!important") && !out.contains("sup, sub {"),
            "CSS 残渣应被丢弃：{out:?}"
        );
    }

    /// 真实 Steam 类营销邮件应被显著压缩（实测 909 → ~499 字符）。
    #[test]
    fn reduce_noise_shrinks_realistic_newsletter() {
        let src = "Helix Byte 您好！\nSteam 秋季特卖现已盛大开幕\n\nMECCHA CHAMELEON - 省 30%！\n\nPalworld / 幻兽帕鲁 - 省 30%！\n\n您收到此邮件是因为我们强烈向您推荐上述项目。\n如果将来您不想收到愿望单通知消息，您可以通过点击以下链接编辑您的电子邮件偏好：<https://store.steampowered.com/account/notificationsettings?token=4b8ab6b810bd1db4948b5fcee05a86d0937a3b6845b01e285d035254a74f3f67633e7f7c176e8852c5db1257c8013b6b>\n";
        let out = reduce_noise(src);
        assert!(out.len() < src.len(), "应被压缩：{} → {}", src.len(), out.len());
        assert!(!out.contains("token="), "长 token URL 应被移除：{out}");
        assert!(out.contains("MECCHA CHAMELEON"), "促销正文应保留：{out}");
    }

    // ---- 摘要 JSON 解析与组装 ----

    /// 标准英文键的完整载荷。
    #[test]
    fn parse_summary_standard_keys() {
        let raw = r#"{"summary":"会议邀请","points":["10 月 28-29 日","旧金山"],"action":"报名参加"}"#;
        let p = parse_summary(raw).expect("应解析成功");
        assert_eq!(p.summary, "会议邀请");
        assert_eq!(p.points, vec!["10 月 28-29 日", "旧金山"]);
        assert_eq!(p.action.as_deref(), Some("报名参加"));
    }

    /// 实测漂移形态：键名变成中文别名（「主题 / 关键要点 / 行动」）。
    #[test]
    fn parse_summary_accepts_chinese_aliases() {
        let raw = r#"{"主题":"领取积分提醒","关键要点":["积分即将过期","尽快兑换"],"行动":"访问网站兑换"}"#;
        let p = parse_summary(raw).expect("中文别名键应被接住");
        assert_eq!(p.summary, "领取积分提醒");
        assert_eq!(p.points.len(), 2);
        assert_eq!(p.action.as_deref(), Some("访问网站兑换"));
    }

    /// points 为字符串（换行 + 前缀符号）时按行拆分。
    #[test]
    fn parse_summary_points_as_string() {
        let raw = r#"{"summary":"周报","points":"- 完成 A\n- 进行 B\n\n* 计划 C","action":null}"#;
        let p = parse_summary(raw).unwrap();
        assert_eq!(p.points, vec!["完成 A", "进行 B", "计划 C"]);
        assert_eq!(p.action, None, "null 行动应转 None");
    }

    /// 实测漂移形态（整封邮件被重排）：summary 从「邮件主题」命中；
    /// 「邮件正文」是嵌套对象，不得被误当作 points。
    #[test]
    fn parse_summary_reorganized_email_still_reads_summary() {
        let raw = r#"{"邮件主题":"职位推荐","发件人":"51Job","发件时间":"2026-09-23","邮件正文":{"职位":"运维工程师"}}"#;
        let p = parse_summary(raw).expect("应从「邮件主题」取到 summary");
        assert_eq!(p.summary, "职位推荐");
        assert!(p.points.is_empty(), "嵌套正文对象不应作为要点");
        assert_eq!(p.action, None);
    }

    #[test]
    fn parse_summary_missing_or_blank_summary_fails() {
        assert!(parse_summary(r#"{"points":["a"]}"#).is_none(), "缺 summary 应判失败");
        assert!(parse_summary(r#"{"summary":"  "}"#).is_none(), "空 summary 应判失败");
        assert!(parse_summary(r#"{"summary":"无"}"#).is_none(), "占位词应判失败");
        assert!(parse_summary("not json at all").is_none(), "非法 JSON 应判失败");
    }

    /// 组装成三节文本（缓存与前端依赖这个格式）。
    #[test]
    fn render_summary_composes_three_sections() {
        let p = SummaryPayload {
            summary: "账单通知".to_string(),
            points: vec!["金额 100 元".to_string(), "到期日 10 月 20 日".to_string()],
            action: Some("尽快缴费".to_string()),
        };
        assert_eq!(
            render_summary(&p),
            "【一句话概括】账单通知\n【关键信息】\n- 金额 100 元\n- 到期日 10 月 20 日\n【需要我做什么】尽快缴费"
        );
    }

    #[test]
    fn render_summary_without_action_or_points() {
        let p = SummaryPayload {
            summary: "纯通知".to_string(),
            points: vec![],
            action: None,
        };
        assert_eq!(
            render_summary(&p),
            "【一句话概括】纯通知\n【关键信息】\n- （无）\n【需要我做什么】无"
        );
    }
}
