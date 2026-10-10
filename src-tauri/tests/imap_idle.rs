//! 针对实时收信 IDLE 链路的集成测试。
//!
//! 运行（需要真实账号，参考 tests/netease_id.rs 的用法）：
//! ```bash
//! PONY_TEST_EMAIL=you@163.com \
//! PONY_TEST_PASS=授权码 \
//!   cargo test --test imap_idle -- --ignored --nocapture
//! ```
//!
//! 验证目标：能力探测 → 进入 IDLE → 等待结束（DONE 收尾）后，会话仍可继续
//! 收发命令；以及新邮件到达时服务器能立刻推送唤醒（实时收信的根基）。
//! 这是 vendor/imap `idle.rs` 补丁的回归点：
//! 上游等待超时把 DONE 收尾留给 Drop（读无超时，断网时会永久阻塞监听线程），
//! 且建立阶段遇到未预期的响应会 `unreachable!()` 直接崩溃（release panic=abort）。

use std::time::{Duration, Instant};

use imap::extensions::idle::WaitOutcome;
use pony_mail_lib::mail::{imap_client, smtp_client};
use pony_mail_lib::models::{Account, SendPayload};

fn account() -> Account {
    let email = std::env::var("PONY_TEST_EMAIL").expect("缺少 PONY_TEST_EMAIL");
    Account {
        id: "test".into(),
        email: email.clone(),
        display_name: "test".into(),
        imap_host: std::env::var("PONY_TEST_IMAP_HOST").unwrap_or_else(|_| "imap.163.com".into()),
        imap_port: 993,
        imap_tls: true,
        smtp_host: std::env::var("PONY_TEST_SMTP_HOST").unwrap_or_else(|_| "smtp.163.com".into()),
        smtp_port: std::env::var("PONY_TEST_SMTP_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(465),
        smtp_tls: true,
        username: email,
        created_at: String::new(),
    }
}

#[test]
#[ignore]
fn idle_round_trip_keeps_session_usable() {
    let password = std::env::var("PONY_TEST_PASS").expect("缺少 PONY_TEST_PASS");
    let acct = account();

    let mut session = imap_client::connect(&acct, &password).expect("登录失败");
    imap_client::select_folder(&mut session, "INBOX").expect("SELECT INBOX 失败");

    let idle_supported = session
        .capabilities()
        .map(|caps| caps.has_str("IDLE"))
        .unwrap_or(false);
    eprintln!("[1/4] 登录成功，服务器 IDLE 能力：{idle_supported}");
    if !idle_supported {
        eprintln!("服务器不支持 IDLE，跳过监听路径验证（运行时将退化为轮询）");
        imap_client::logout(session);
        return;
    }

    // 两轮 IDLE：第一轮验证进入与超时退出；第二轮验证 DONE 收尾后会话未被污染，
    // 还能再次进入 IDLE（监听线程正是这样循环的）。
    for round in 1..=2 {
        let handle = session.idle().expect("进入 IDLE 失败");
        let outcome = handle
            .wait_with_timeout(Duration::from_secs(4))
            .expect("IDLE 等待出错");
        eprintln!("[2/4] 第 {round} 轮 IDLE 正常结束：{outcome:?}");
    }

    // DONE 收尾之后会话必须仍可正常收发命令。
    let uids = imap_client::list_uids(&mut session, "INBOX").expect("IDLE 之后的 SEARCH 失败");
    eprintln!("[3/4] IDLE 后 SEARCH 成功，共 {} 封", uids.len());

    imap_client::logout(session);
    eprintln!("[4/4] 登出完成");
}

/// 端到端验证「实时收信」的核心承诺：IDLE 期间新邮件到达会立刻唤醒等待。
///
/// 做法：IDLE 期间从另一线程给自己发一封测试邮件，等待必须远早于超时返回，
/// 且唤醒后能在收件箱搜到新邮件。
#[test]
#[ignore]
fn idle_wakes_on_new_mail() {
    let password = std::env::var("PONY_TEST_PASS").expect("缺少 PONY_TEST_PASS");
    let acct = account();

    let mut session = imap_client::connect(&acct, &password).expect("登录失败");
    imap_client::select_folder(&mut session, "INBOX").expect("SELECT INBOX 失败");
    let idle_supported = session
        .capabilities()
        .map(|caps| caps.has_str("IDLE"))
        .unwrap_or(false);
    if !idle_supported {
        eprintln!("服务器不支持 IDLE，跳过（运行时将退化为轮询）");
        imap_client::logout(session);
        return;
    }

    let before = imap_client::list_uids(&mut session, "INBOX")
        .expect("SEARCH 失败")
        .len();

    // 3 秒后给自己发一封测试邮件（独立线程，避免阻塞主线程的 IDLE 读）。
    let send_acct = account();
    let send_pass = password.clone();
    let subject = format!("Pony idle probe {}", std::process::id());
    let send_subject = subject.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        let payload = SendPayload {
            account_id: send_acct.id.clone(),
            to: send_acct.email.clone(),
            cc: String::new(),
            subject: send_subject,
            body: "idle wakeup probe".to_string(),
            html: false,
        };
        match smtp_client::send(&send_acct, &send_pass, &payload) {
            Ok(()) => eprintln!("[send] 自寄测试邮件已发出：{subject}"),
            Err(e) => eprintln!("[send] 发送失败：{e}"),
        }
    });

    let timeout = Duration::from_secs(45);
    let started = Instant::now();
    let handle = session.idle().expect("进入 IDLE 失败");
    let outcome = handle.wait_with_timeout(timeout).expect("IDLE 等待出错");
    let waited = started.elapsed();
    eprintln!("[1/3] IDLE 结束：{outcome:?}，用时 {:.1}s", waited.as_secs_f32());

    let _ = sender.join();
    assert_eq!(
        outcome,
        imap::extensions::idle::WaitOutcome::MailboxChanged,
        "新邮件到达后 IDLE 未被唤醒（等到超时才返回）"
    );

    let after = imap_client::list_uids(&mut session, "INBOX").expect("唤醒后 SEARCH 失败");
    eprintln!("[2/3] 唤醒后收件箱 {} 封（此前 {before} 封）", after.len());
    assert!(
        after.len() > before,
        "IDLE 被唤醒但收件箱没有新邮件（邮件可能投递到了别的文件夹）"
    );

    imap_client::logout(session);
    eprintln!("[3/3] 登出完成，实时收信链路验证通过");
}

/// 延迟归因 + 能力诊断：打印服务器能力列表、直接尝试进入 IDLE，
/// 并测量新邮件从「服务器上可见」到「我们唤醒并取回」各环节的耗时。
///
/// 双连接：
///   - 监听连接：登录后不看能力通告，直接尝试进入 IDLE——通告与真实支持
///     不一致的服务器并不少见；
///   - 观察连接：每 200ms `UID SEARCH` 一次，记录邮件首次在服务器可见的时刻；
///   - 发送线程：观察连接就绪后 1 秒，给自己发一封测试邮件。
///
/// 判读：
///   - 「进入 IDLE 失败」或「IDLE 被接受但 60 秒无推送」→ 该服务器无法用
///     IDLE 推送，实时性只能靠轮询兜底；
///   - 「服务器可见→IDLE 唤醒」亚秒级 → 推送正常，延迟在客户端侧；
///   - 该值达秒级/十秒级 → 服务器推送本身延迟。
/// 配合 PONY_IMAP_DEBUG=1 可看到逐行协议时间戳，定位具体环节。
#[test]
#[ignore]
fn measure_wake_latency() {
    let password = std::env::var("PONY_TEST_PASS").expect("缺少 PONY_TEST_PASS");
    let acct = account();

    // 监听连接：记录基线 UID，并把服务器能力列表打印出来（只报告，不断言）。
    let mut watch = imap_client::connect(&acct, &password).expect("监听连接登录失败");
    let base_uids = imap_client::list_uids(&mut watch, "INBOX").expect("基线 SEARCH 失败");
    let baseline = base_uids.iter().copied().max().unwrap_or(0);

    match watch.capabilities() {
        Ok(caps) => {
            let names: Vec<String> = caps.iter().map(|c| format!("{c:?}")).collect();
            eprintln!("[caps] 共 {} 项：{}", names.len(), names.join(" "));
            let idle_ci = names
                .iter()
                .any(|n| n.to_ascii_uppercase().contains("\"IDLE\""));
            eprintln!(
                "[caps] has_str(\"IDLE\") = {}；按名称大小写不敏感复查 = {}",
                caps.has_str("IDLE"),
                idle_ci
            );
        }
        Err(e) => eprintln!("[caps] CAPABILITY 查询失败（能力判断会因此失真）：{e}"),
    }

    // 观察连接：就绪后每 200ms 轮询「新 UID 可见」的时刻。
    let obs_acct = account();
    let obs_pass = password.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let (visible_tx, visible_rx) = std::sync::mpsc::channel::<Instant>();
    let observer = std::thread::spawn(move || {
        let mut s = imap_client::connect(&obs_acct, &obs_pass).expect("观察连接登录失败");
        imap_client::select_folder(&mut s, "INBOX").expect("观察连接 SELECT 失败");
        let _ = ready_tx.send(());
        loop {
            let hits = s
                .uid_search(format!("UID {}:*", baseline.saturating_add(1)))
                .expect("观察 SEARCH 失败");
            if hits.iter().any(|u| *u > baseline) {
                let _ = visible_tx.send(Instant::now());
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        imap_client::logout(s);
    });

    ready_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("观察连接未就绪");

    let t0 = Instant::now();
    let send_acct = account();
    let send_pass = password.clone();
    let subject = format!("Pony latency probe {}", std::process::id());
    let send_subject = subject.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        let payload = SendPayload {
            account_id: send_acct.id.clone(),
            to: send_acct.email.clone(),
            cc: String::new(),
            subject: send_subject,
            body: "latency probe".to_string(),
            html: false,
        };
        match smtp_client::send(&send_acct, &send_pass, &payload) {
            Ok(()) => eprintln!("[send] 测试邮件已发出：{subject}"),
            Err(e) => eprintln!("[send] 发送失败：{e}"),
        }
    });

    // 不看能力通告，直接尝试进入 IDLE。
    // 借用全部收敛在这个块里：块结束时 attempt 即析构、借用结束，
    // 之后两个分支都能安全使用 / 移动会话。
    let wake: Option<WaitOutcome> = {
        let idle_attempt = watch.idle();
        match idle_attempt {
            Ok(handle) => {
                eprintln!("[idle] 服务器接受了 IDLE");
                Some(
                    handle
                        .wait_with_timeout(Duration::from_secs(60))
                        .expect("IDLE 等待出错"),
                )
            }
            Err(e) => {
                eprintln!("[idle] 进入 IDLE 失败（该服务器不接受 IDLE）：{e}");
                None
            }
        }
    };
    let t_wake = Instant::now();

    let Some(wake) = wake else {
        // 服务器不接受 IDLE：等观察者确认邮件确实到达过服务器，再收尾。
        // 这是诊断项，不算测试失败。
        let visible = visible_rx.recv_timeout(Duration::from_secs(30)).ok();
        let _ = observer.join();
        let _ = sender.join();
        match visible {
            Some(v) => eprintln!(
                "[idle] 结论：邮件在发出后 {:?} 已可见于服务器，但无法用 IDLE 推送——实时性只能靠轮询",
                v - t0
            ),
            None => eprintln!("[idle] 观察连接未检测到新邮件，请检查 SMTP 投递是否成功"),
        }
        imap_client::logout(watch);
        return;
    };

    eprintln!("[idle] 等待结束：{wake:?}");

    // 唤醒后的取回序列（与 watcher::poll_new 相同）分段计时。
    let seg = Instant::now();
    let hits = watch
        .uid_search(format!("UID {}:*", baseline.saturating_add(1)))
        .expect("唤醒后 SEARCH 失败");
    let search_elapsed = seg.elapsed();
    let new_uids: Vec<u32> = hits.into_iter().filter(|u| *u > baseline).collect();
    let seg = Instant::now();
    let mails = imap_client::fetch_headers(&mut watch, &new_uids).expect("唤醒后 FETCH 失败");
    let fetch_elapsed = seg.elapsed();

    let visible = visible_rx.recv_timeout(Duration::from_secs(15)).ok();
    let _ = observer.join();
    let _ = sender.join();

    match (wake, visible) {
        (WaitOutcome::TimedOut, Some(v)) => eprintln!(
            "[idle] 结论：IDLE 被接受但 60 秒内没有任何推送（邮件在发出后 {:?} 就已在服务器可见）——推送形同虚设，实时性只能靠轮询",
            v - t0
        ),
        (_, Some(v)) if v <= t_wake => eprintln!(
            "[idle] 服务器可见→IDLE 唤醒：{:?}（观察连接粒度 200ms 起的往返，实际推送延迟 ≤ 此值）",
            t_wake - v
        ),
        (_, Some(v)) => eprintln!(
            "[idle] IDLE 唤醒领先观察连接 {:?}（推送即时到达，属最佳情形）",
            v.saturating_duration_since(t_wake)
        ),
        (_, None) => eprintln!("[idle] 观察连接未检测到新邮件（可见时刻未知，请检查 SMTP 投递）"),
    }
    eprintln!("[idle] t0→等待结束总计：{:?}", t_wake - t0);
    eprintln!(
        "[idle] 唤醒后取回：SEARCH {:?}，FETCH {} 封 {:?}",
        search_elapsed,
        mails.len(),
        fetch_elapsed
    );
    imap_client::logout(watch);
}

/// 轮询路径验证：模拟 watcher 在「不支持 IDLE」服务器（如网易 163）上的轮询循环，
/// 实测「固定间隔轮询」发现新邮件的延迟上限，并顺带观察连续轮询是否稳定
/// （多轮 SEARCH 不报错）。
///
/// 用 `POLL_PROBE_INTERVAL`（秒，默认 15，与 watcher 的 POLL_INTERVAL 一致）覆盖间隔。
#[test]
#[ignore]
fn measure_poll_latency() {
    let password = std::env::var("PONY_TEST_PASS").expect("缺少 PONY_TEST_PASS");
    let acct = account();
    let interval = Duration::from_secs(
        std::env::var("POLL_PROBE_INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15),
    );

    // 与 watcher 的轮询路径一致：一次连接、一次 SELECT，之后只做 SEARCH。
    let mut session = imap_client::connect(&acct, &password).expect("登录失败");
    let base_uids = imap_client::list_uids(&mut session, "INBOX").expect("基线 SEARCH 失败");
    let baseline = base_uids.iter().copied().max().unwrap_or(0);

    let send_acct = account();
    let send_pass = password.clone();
    let subject = format!("Pony poll probe {}", std::process::id());
    let send_subject = subject.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        let payload = SendPayload {
            account_id: send_acct.id.clone(),
            to: send_acct.email.clone(),
            cc: String::new(),
            subject: send_subject,
            body: "poll probe".to_string(),
            html: false,
        };
        match smtp_client::send(&send_acct, &send_pass, &payload) {
            Ok(()) => eprintln!("[poll] 测试邮件已发出：{subject}"),
            Err(e) => eprintln!("[poll] 发送失败：{e}"),
        }
    });

    let t0 = Instant::now();
    eprintln!("[poll] 轮询开始（间隔 {interval:?}，邮件将于 3 秒后发出）");
    let mut detected = false;
    for round in 1..=8u32 {
        std::thread::sleep(interval);
        let hits = session
            .uid_search(format!("UID {}:*", baseline.saturating_add(1)))
            .expect("轮询 SEARCH 失败");
        let fresh: Vec<u32> = hits.into_iter().filter(|u| *u > baseline).collect();
        if fresh.is_empty() {
            eprintln!("[poll] 第 {round} 次轮询：无新邮件（t0+{:?}）", t0.elapsed());
            continue;
        }
        eprintln!(
            "[poll] 第 {round} 次轮询检出 {} 封新邮件（t0+{:?}，含邮件发送与服务器固有投递延迟）",
            fresh.len(),
            t0.elapsed()
        );
        detected = true;
        break;
    }
    if !detected {
        eprintln!("[poll] 8 轮轮询内未检出（请检查 SMTP 投递是否成功）");
    }

    let _ = sender.join();
    imap_client::logout(session);
    eprintln!("[poll] 连续轮询期间连接保持稳定、未报错");
}
