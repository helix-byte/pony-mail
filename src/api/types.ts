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
  /** 该邮件携带的标签 id。列表接口直接返回，无需逐封再查。 */
  tagIds: number[];
}

/** 用户自定义标签（按账号隔离）。 */
export interface Tag {
  id: number;
  accountId: string;
  name: string;
  color: string;
  sortOrder: number;
  /** 已打标邮件数，用于侧栏角标。 */
  messageCount: number;
  /** 创建者："user"（默认）或 "ai"（智能整理创建）。 */
  createdBy: string;
}

/**
 * 标签组：可点击筛选的标签合集，组内标签取并集（OR）。
 *
 * 与标签是**多对多**关系，同一标签可同时归入多个组。
 */
export interface TagGroup {
  id: number;
  accountId: string;
  name: string;
  color: string;
  sortOrder: number;
  /** 侧栏折叠状态，持久化在后端。 */
  collapsed: boolean;
  /** 组内标签 id。 */
  tagIds: number[];
  /** 组内**去重后**的邮件数（一封邮件带组内多个标签只算一次）。 */
  messageCount: number;
  /** 组内各标签邮件数之和（含重复计数），与 messageCount 的差值即重叠量。 */
  rawMessageCount: number;
  /**
   * AI 整理维度模式："off"（默认，不参与）/ "select"（从组内现有标签选一个）
   * / "create"（可从组内选，也可创建新标签并自动入组）。
   */
  aiMode: string;
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
  /** 同步完成后自动整理新邮件（缺省视为开启）。 */
  autoClassify: boolean;
}

/** 批量整理的事件流（与 Rust 侧 `ClassifyEvent` 的 serde 形态一一对应）。 */
export type ClassifyEvent =
  | { event: "start"; data: { total: number } }
  | {
      event: "item";
      data: { messageId: number; tagIds: number[]; skipped: boolean; error: string | null };
    }
  | {
      event: "done";
      data: {
        classified: number;
        failed: number;
        skipped: number;
        remaining: number;
        cancelled: boolean;
      };
    }
  | { event: "error"; data: { message: string } };

export interface SendPayload {
  accountId: string;
  to: string;
  cc: string;
  subject: string;
  body: string;
  html: boolean;
}
