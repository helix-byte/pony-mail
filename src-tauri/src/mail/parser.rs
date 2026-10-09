use mail_parser::{Addr, Address, MessageParser, MimeHeaders};

use crate::models::{AttachmentMeta, ParsedMail};

/// 将一封原始 RFC822 报文（可能只含头部）解析为 `ParsedMail`。
///
/// - `has_body` 为 false 时只解析头部，正文保持为空；
/// - 解析失败返回 None，由调用方跳过该邮件。
pub fn parse(raw: &[u8], uid: u32, seen: bool, flagged: bool, with_body: bool) -> Option<ParsedMail> {
    let msg = MessageParser::default().parse(raw)?;

    let (from_name, from_addr) = split_addr(msg.from());
    let subject = msg.subject().unwrap_or("(无主题)").to_string();

    let mut mail = ParsedMail {
        uid,
        message_id: msg.message_id().unwrap_or_default().to_string(),
        subject: decode_header(&subject),
        from_name: decode_header(&from_name),
        from_addr,
        to_addrs: join_addrs(msg.to()),
        cc_addrs: join_addrs(msg.cc()),
        date_ts: msg.date().map(|d| d.to_timestamp()).unwrap_or(0),
        seen,
        flagged,
        has_body: with_body,
        ..Default::default()
    };

    if with_body {
        mail.body_text = msg
            .body_text(0)
            .map(|s| s.into_owned())
            .unwrap_or_default();
        let raw_html = msg.body_html(0).map(|s| s.into_owned()).unwrap_or_default();
        // 邮件 HTML 一律先清洗掉脚本与事件属性，再交给前端沙箱渲染
        mail.body_html = if raw_html.trim().is_empty() {
            plain_or_empty(&mail.body_text)
        } else {
            let cleaned = super::sanitize::sanitize_html(&raw_html);
            // 清洗后可能只剩空标签，此时退回纯文本，避免正文区一片空白
            if super::sanitize::has_visible_content(&cleaned) {
                cleaned
            } else {
                plain_or_empty(&mail.body_text)
            }
        };

        mail.attachments = msg
            .attachments()
            .filter(|p| p.attachment_name().is_some())
            .map(|p| AttachmentMeta {
                filename: p.attachment_name().unwrap_or("未命名附件").to_string(),
                mime: p
                    .content_type()
                    .map(|ct| {
                        format!("{}/{}", ct.c_type, ct.subtype().unwrap_or("octet-stream"))
                    })
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
                size: p.contents().len(),
            })
            .collect();
    }

    Some(mail)
}

fn split_addr(addr: Option<&Address>) -> (String, String) {
    match addr.and_then(|a| a.first()) {
        Some(Addr { name, address }) => (
            name.as_deref().unwrap_or("").to_string(),
            address.as_deref().unwrap_or("").to_string(),
        ),
        None => (String::new(), String::new()),
    }
}

fn join_addrs(addr: Option<&Address>) -> String {
    match addr {
        Some(a) => a
            .iter()
            .filter_map(|x| x.address.as_deref())
            .collect::<Vec<_>>()
            .join(", "),
        None => String::new(),
    }
}

/// 主题/姓名里常见的 RFC2047 编码词（=?...?=）由 mail-parser 已解码，
/// 此处只做零宽字符与多余空白清理。
fn decode_header(s: &str) -> String {
    s.replace('\u{200b}', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn plain_to_html(text: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!("<pre style=\"white-space:pre-wrap;font-family:inherit\">{escaped}</pre>")
}

/// 没有 HTML 正文（或清洗后已无内容）时的兜底展示。
fn plain_or_empty(text: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        plain_to_html(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GBK 编码的正文必须被正确解码。
    ///
    /// 回归防线：`mail-parser` 的 `default = []`，多字节解码器全部由
    /// `full_encoding` feature 门控。一旦该 feature 被移除，GBK 字节会退化
    /// 成 `from_utf8_lossy`，中文变成不可逆的 U+FFFD（'我' → "��"）。
    #[test]
    fn decodes_gbk_body_without_replacement_chars() {
        // "我" 的 GBK 编码是 CE D2，"们" 是 C3 C7
        let gbk_body: Vec<u8> = vec![0xCE, 0xD2, 0xC3, 0xC7];

        let mut raw = Vec::new();
        raw.extend_from_slice(b"From: =?GBK?B?x7CzzM7e08c=?= <hr@51job.com>\r\n");
        raw.extend_from_slice(b"To: me@example.com\r\n");
        raw.extend_from_slice(b"Subject: =?GBK?B?x7CzzM7e08c=?=\r\n");
        raw.extend_from_slice(b"MIME-Version: 1.0\r\n");
        raw.extend_from_slice(b"Content-Type: text/plain; charset=\"GBK\"\r\n");
        raw.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n");
        raw.extend_from_slice(b"\r\n");
        raw.extend_from_slice(&gbk_body);

        let mail = parse(&raw, 1, false, false, true).expect("应能解析");

        assert_eq!(mail.body_text, "我们", "GBK 正文应被正确解码");
        assert!(
            !mail.body_text.contains('\u{FFFD}'),
            "不应出现 U+FFFD 替换字符，说明 full_encoding 丢失"
        );
        // 发件人姓名 "前程无忧" 的 GBK 编码
        assert_eq!(mail.from_name, "前程无忧", "GBK 编码词应被正确解码");
    }

    /// UTF-8 正文不应受编码改动影响。
    #[test]
    fn still_decodes_utf8_body() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"From: a@example.com\r\n");
        raw.extend_from_slice(b"Subject: hello\r\n");
        raw.extend_from_slice(b"MIME-Version: 1.0\r\n");
        raw.extend_from_slice(b"Content-Type: text/plain; charset=\"utf-8\"\r\n");
        raw.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n");
        raw.extend_from_slice(b"\r\n");
        raw.extend_from_slice("你好，世界".as_bytes());

        let mail = parse(&raw, 2, false, false, true).expect("应能解析");
        assert_eq!(mail.body_text, "你好，世界");
    }
}
