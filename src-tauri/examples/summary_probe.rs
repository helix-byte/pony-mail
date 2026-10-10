//! 摘要链路的真实模型评测工具。
//!
//! 用法：`cargo run --example summary_probe -- <db 副本路径> [模型] <邮件 id...>`
//! （模型传空串或省略时用库内 AI 设置）。
//!
//! 走与生产完全一致的 `ai::summarize`（含降噪、JSON 约束、解析、跑偏重试）。
//! 注意：**只能对副本库运行任意读写皆可，不要指向真实库**——Store::new 会执行迁移。

use pony_mail_lib::ai;
use pony_mail_lib::store::Store;

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法：summary_probe <db 副本路径> [模型] <邮件 id...>");
        std::process::exit(1);
    }
    let path = args.remove(0);
    // 剩余参数：数字视为邮件 id，第一个非数字作为模型名覆盖。
    let mut model_override: Option<String> = None;
    let mut ids: Vec<i64> = Vec::new();
    for a in args {
        match a.parse::<i64>() {
            Ok(id) => ids.push(id),
            Err(_) => {
                if model_override.is_none() {
                    model_override = Some(a);
                }
            }
        }
    }

    let store = Store::new(path.clone().into()).expect("打开库失败");
    let mut settings = store.ai_settings().expect("读 AI 设置失败");
    if let Some(model) = model_override {
        if !model.is_empty() {
            println!("（覆盖模型：{} → {model}）", settings.model);
            settings.model = model;
        }
    }
    println!("模型：{} @ {}", settings.model, settings.base_url);

    let client = reqwest::Client::builder()
        .user_agent("PonyMail-probe")
        .build()
        .expect("建 http client 失败");

    for id in ids {
        match store.get_message(id) {
            Ok(detail) => {
                println!(
                    "\n===== 邮件 {id} | {} | 正文{} =====",
                    detail.header.subject.chars().take(44).collect::<String>(),
                    if detail.header.body_loaded { "已加载" } else { "未加载" }
                );
                let started = std::time::Instant::now();
                match ai::summarize(&client, &settings, &detail).await {
                    Ok(text) => println!("[{:.1}s]\n{text}", started.elapsed().as_secs_f32()),
                    Err(e) => println!("[失败 {:.1}s] {e}", started.elapsed().as_secs_f32()),
                }
            }
            Err(e) => println!("邮件 {id} 读取失败：{e}"),
        }
    }
}
