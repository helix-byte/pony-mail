/** 与 Rust 侧 `models.rs` 一一对应的数据结构（均使用 camelCase）。 */

export interface Account {
  id: string;
  email: string;
  displayName: string;
  imapHost: string;
  imapPort: number;
  imapTls: boolean;
  smtpHost: string;
  smtpPort: number;
  smtpTls: boolean;
  username: string;
  createdAt: string;
}

export interface AccountInput {
  email: string;
  displayName: string;
  imapHost: string;
  imapPort: number;
  imapTls: boolean;
  smtpHost: string;
  smtpPort: number;
  smtpTls: boolean;
  username: string;
  password: string;
}

export interface MessageHeader {
  id: number;
  uid: number;
  folder: string;
  subject: string;
  fromName: string;
  fromAddr: string;
  dateTs: number;
  seen: boolean;
  flagged: boolean;
  hasAttachments: boolean;
  bodyLoaded: boolean;
  snippet: string;
}

export interface AttachmentMeta {
  filename: string;
  mime: string;
  size: number;
}

export interface MessageDetail {
  header: MessageHeader;
  toAddrs: string;
  ccAddrs: string;
  messageId: string;
  bodyText: string;
  bodyHtml: string;
  attachments: AttachmentMeta[];
}

export interface SyncReport {
  folder: string;
  fetched: number;
  inserted: number;
  totalOnServer: number;
}

/** 删除邮件的结果，区分「服务器已删」与「仅删了本地缓存」。 */
export interface DeleteReport {
  deletedLocal: number;
  deletedRemote: boolean;
  remoteError: string;
}

export interface TestReport {
  imapOk: boolean;
  smtpOk: boolean;
  imapMessage: string;
  smtpMessage: string;
}

export interface Summary {
  text: string;
  model: string;
  cached: boolean;
}

export interface AiStatus {
  online: boolean;
  models: string[];
  baseUrl: string;
  model: string;
  message: string;
}

export interface AiSettings {
  baseUrl: string;
  model: string;
}

export interface SendPayload {
  accountId: string;
  to: string;
  cc: string;
  subject: string;
  body: string;
  html: boolean;
}
