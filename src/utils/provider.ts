/** 邮箱服务商判定工具。判据与 Rust 侧 `Account::is_gmail`（models.rs）保持一致。 */

/**
 * IMAP 主机是否属于 Gmail / Google 系（imap.gmail.com / imap.googlemail.com，
 * Google Workspace 自建域名也走同一台服务器）。
 *
 * 注意域名标签边界：`mygmail.com` 这类并不以 `.gmail.com` 结尾，必须排除。
 */
export function isGmailHost(host: string | undefined | null): boolean {
  if (!host) return false;
  const h = host.trim().toLowerCase();
  return (
    h === "gmail.com" ||
    h === "googlemail.com" ||
    h.endsWith(".gmail.com") ||
    h.endsWith(".googlemail.com")
  );
}
