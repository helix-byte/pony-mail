use lettre::message::header::ContentType;
use lettre::message::{Mailbox, Message};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::SmtpTransport;
use lettre::Transport;

use crate::error::{AppError, AppResult};
use crate::models::{Account, SendPayload};

/// 通过 SMTP 发送邮件。
///
/// - 端口 465 使用隐式 TLS（SMTPS）
/// - 其他端口在 `smtp_tls` 为真时使用 STARTTLS
/// - `smtp_tls` 为假时明文发送（仅建议用于本地测试服务器）
pub fn send(account: &Account, password: &str, payload: &SendPayload) -> AppResult<()> {
    if payload.to.trim().is_empty() {
        return Err(AppError::Other("收件人不能为空".to_string()));
    }

    let from: Mailbox = account
        .email
        .parse()
        .map_err(|e: lettre::address::AddressError| AppError::Address(e.to_string()))?;

    let mut builder = Message::builder()
        .from(from)
        .subject(payload.subject.clone());

    for m in parse_mailbox_list(&payload.to)? {
        builder = builder.to(m);
    }
    for m in parse_mailbox_list(&payload.cc)? {
        builder = builder.cc(m);
    }

    let content_type = if payload.html {
        ContentType::TEXT_HTML
    } else {
        ContentType::TEXT_PLAIN
    };

    let email = builder
        .header(content_type)
        .body(payload.body.clone())
        .map_err(|e| AppError::Mail(format!("构造邮件失败：{e}")))?;

    let mailer = build_transport(account, password)?;
    mailer.send(&email)?;
    Ok(())
}

/// 依据账号配置构建 SMTP 传输通道（发送与连通性测试复用）。
pub fn build_transport(account: &Account, password: &str) -> AppResult<SmtpTransport> {
    // 与 IMAP 登录同一套凭据规范化（Gmail 应用专用密码去掉粘贴带入的空白）。
    let password = super::normalize_password(account, password);
    let credentials = Credentials::new(account.username.clone(), password);

    if !account.smtp_tls {
        return Ok(SmtpTransport::builder_dangerous(&account.smtp_host)
            .port(account.smtp_port)
            .credentials(credentials)
            .build());
    }

    let tls_params = TlsParameters::new(account.smtp_host.clone())
        .map_err(|e| AppError::Mail(format!("TLS 初始化失败：{e}")))?;
    let mode = if account.smtp_port == 465 {
        Tls::Wrapper(tls_params)
    } else {
        Tls::Required(tls_params)
    };

    Ok(SmtpTransport::builder_dangerous(&account.smtp_host)
        .port(account.smtp_port)
        .tls(mode)
        .credentials(credentials)
        .build())
}

fn parse_mailbox_list(raw: &str) -> AppResult<Vec<Mailbox>> {
    raw.split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<Mailbox>()
                .map_err(|e| AppError::Address(format!("地址 {s} 无效：{e}")))
        })
        .collect()
}
