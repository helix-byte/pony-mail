//! 一次性验证工具：真实模型端到端 probe（验证后即删）。
//!
//! 用法：cargo run --example classify_probe -- <db 副本路径> [邮件数，默认 3]
//!
//! 流程与 `commands::classify_messages` 主链路一致
//! （维度组快照 → 快速通道 → prompt 构造 → 请求 → 解析 → 写回）；
//! 仅有请求部分内联（`ai::chat_json` 是 pub(crate)，example 不可见），
//! 请求参数与 `chat_json` 逐项一致：think:false / format:"json" /
//! temperature:0 / num_predict:512。

use std::time::Duration;

use pony_mail_lib::classify::{self, Pick};
use pony_mail_lib::store::{GroupAssignment, Store};

async fn chat(
    client: &reqwest::Client,
    base: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    // 模拟生产 `run_with_recovery`：runner 病态（空壳 / 复读）→ 卸载 + 5s + 重试一次。
    match chat_once(client, base, model, system, user).await {
        Err(e) if e.contains("空响应") || e.contains("repeat limit") => {
            println!("[自愈] runner 病态，卸载模型 + 等待 5s 后重试…");
            let _ = client
                .post(format!("{}/api/generate", base.trim_end_matches('/')))
                .json(&serde_json::json!({ "model": model, "keep_alive": 0 }))
                .timeout(Duration::from_secs(30))
                .send()
                .await;
            tokio::time::sleep(Duration::from_millis(5000)).await;
            chat_once(client, base, model, system, user).await
        }
        other => other,
    }
}

async fn chat_once(
    client: &reqwest::Client,
    base: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    let url = format!("{}/api/chat", base.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "think": false,
        "format": "json",
        "options": { "temperature": 0.0, "num_predict": 512 },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    let resp = client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {}", &text[..text.len().min(240)]));
    }
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let content = v
        .pointer("/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if content.is_empty() {
        return Err(format!("空响应（runner 病态）: {}", &text[..text.len().min(240)]));
    }
    Ok(content)
}

#[tokio::main]
async fn main() {
    let path = std::env::args().nth(1).expect("用法：classify_probe <db 副本路径> [邮件数]");
    let limit: i64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let store = Store::new(path.into()).expect("打开库失败");

    // 选未整理邮件最多的账号。
    let accounts = store.list_accounts().expect("列账号失败");
    let account = accounts
        .iter()
        .max_by_key(|a| store.count_unclassified(&a.id).unwrap_or(0))
        .expect("没有账号");
    println!("账号：{}（{}）", account.email, account.id);

    // 配置维度组：名字含「发件/品牌」的组设为 create（测新标签创建），其余 select。
    let all_tags = store.list_tags(&account.id).expect("列标签失败");
    for g in store.list_tag_groups(&account.id).expect("列组失败") {
        let mode = if g.name.contains("发件") || g.name.contains("品牌") {
            "create"
        } else {
            "select"
        };
        store
            .update_tag_group(g.id, None, None, None, Some(mode))
            .expect("设置 ai_mode 失败");
        let names: Vec<&str> = g
            .tag_ids
            .iter()
            .filter_map(|id| all_tags.iter().find(|t| t.id == *id).map(|t| t.name.as_str()))
            .collect();
        println!("组「{}」→ {mode}（成员：{names:?}）", g.name);
    }

    let groups = store.ai_dimension_groups(&account.id).expect("读维度组失败");
    let mut settings = store.ai_settings().expect("读 AI 设置失败");
    // 可选覆盖模型：classify_probe <path> [count] [model]
    if let Some(m) = std::env::args().nth(3) {
        println!("（覆盖模型：{} → {m}）", settings.model);
        settings.model = m;
    }
    println!("模型：{} @ {}", settings.model, settings.base_url);

    let client = reqwest::Client::builder()
        .user_agent("PonyMail-probe")
        .build()
        .expect("建 http client 失败");
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let stamp = chrono::Utc::now().to_rfc3339();
    let group_names: Vec<String> = groups.iter().map(|g| g.name.clone()).collect();

    let ids = store.unclassified_ids(&account.id, limit).expect("读队列失败");
    println!("\n待整理（前 {limit} 封）：{ids:?}");

    for id in ids {
        let input = store.classify_input(id).expect("读邮件失败");
        println!(
            "\n===== 邮件 {id} | 主题：{} | 发件人：{} <{}> =====",
            input.subject.chars().take(40).collect::<String>(),
            input.from_name,
            input.from_addr
        );

        let sample = store
            .sender_classified_count(&account.id, &input.from_addr, id)
            .unwrap();
        let mut stats = Vec::new();
        for g in &groups {
            stats.push(
                store
                    .sender_group_stats(&account.id, &input.from_addr, id, g.id)
                    .unwrap(),
            );
        }
        let consensus = classify::sender_consensus(&stats, sample, classify::SENDER_SKIP_MIN_SAMPLES);

        let assignments: Vec<GroupAssignment>;
        if let Some(picks) = consensus {
            println!("[快速通道] 命中共识（样本 {sample} 封），跳过模型");
            assignments = groups
                .iter()
                .zip(picks)
                .map(|(g, p)| GroupAssignment {
                    group_id: g.id,
                    pick: p.map(Pick::Existing),
                })
                .collect();
        } else {
            let hints = classify::sender_hints(&stats, &group_names, sample);
            let system = classify::build_system_prompt(&groups);
            let user = classify::build_user_prompt(&input, &hints, &now);
            println!(
                "[prompt] system {} 字 / user {} 字（正文{}）",
                system.chars().count(),
                user.chars().count(),
                if input.body_text.is_some() { "已加载" } else { "未加载，用 snippet" }
            );
            let started = std::time::Instant::now();
            match chat(&client, &settings.base_url, &settings.model, &system, &user).await {
                Ok(raw) => {
                    println!("[响应 {:.1}s] {raw}", started.elapsed().as_secs_f32());
                    match classify::parse_classify(&raw, &groups) {
                        Some(parsed) => {
                            for (g, p) in groups.iter().zip(&parsed.picks) {
                                println!("[解析] {} → {p:?}", g.name);
                            }
                            assignments = groups
                                .iter()
                                .zip(parsed.picks)
                                .map(|(g, p)| GroupAssignment { group_id: g.id, pick: p })
                                .collect();
                        }
                        None => {
                            println!("[解析失败] 整体判失败（不写标记，将自动重试）");
                            continue;
                        }
                    }
                }
                Err(e) => {
                    println!("[请求失败] {e}");
                    continue;
                }
            }
        }

        match store.save_classify_result(&account.id, id, &assignments, &stamp) {
            Ok(tag_ids) => println!("[写回] tag_ids = {tag_ids:?}"),
            Err(e) => println!("[写回失败] {e}"),
        }
    }

    println!("\n=== 结束 ===");
    println!("剩余未整理：{}", store.count_unclassified(&account.id).unwrap());
    let tags = store.list_tags(&account.id).unwrap();
    for t in tags.iter().filter(|t| t.created_by == "ai") {
        println!("AI 创建的新标签：{}（{}，{}）", t.name, t.color, t.message_count);
    }
}
