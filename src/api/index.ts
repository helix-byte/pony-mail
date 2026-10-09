import { invoke } from "@tauri-apps/api/core";

import type {
  Account,
  AccountInput,
  AiSettings,
  AiStatus,
  DeleteReport,
  MessageDetail,
  MessageHeader,
  SendPayload,
  Summary,
  SyncReport,
  Tag,
  TestReport,
} from "./types";

/** Tauri command 的前端封装。Rust 侧参数为 snake_case，此处统一用 camelCase 调用。 */
export const api = {
  // ---- 账号 ----
  listAccounts: () => invoke<Account[]>("list_accounts"),
  addAccount: (input: AccountInput) => invoke<Account>("add_account", { input }),
  deleteAccount: (id: string) => invoke<void>("delete_account", { id }),
  setAccountPassword: (id: string, password: string) =>
    invoke<void>("set_account_password", { id, password }),
  testConnection: (input: AccountInput) => invoke<TestReport>("test_connection", { input }),

  // ---- 同步与读取 ----
  syncInbox: (accountId: string, folder = "INBOX", limit = 40) =>
    invoke<SyncReport>("sync_inbox", { accountId, folder, limit }),
  /** 列邮件；传 `tagId` 时只返回打过该标签的邮件（筛选在 SQL 层完成）。 */
  listMessages: (
    accountId: string,
    folder = "INBOX",
    limit = 200,
    offset = 0,
    tagId: number | null = null,
  ) => invoke<MessageHeader[]>("list_messages", { accountId, folder, limit, offset, tagId }),
  loadMessageBody: (id: number) => invoke<MessageDetail>("load_message_body", { id }),
  setSeen: (id: number, seen: boolean) => invoke<void>("set_seen", { id, seen }),
  setFlagged: (id: number, flagged: boolean) => invoke<void>("set_flagged", { id, flagged }),
  deleteMessage: (id: number) => invoke<DeleteReport>("delete_message", { id }),
  deleteMessages: (ids: number[]) => invoke<DeleteReport>("delete_messages", { ids }),

  // ---- 标签 ----
  listTags: (accountId: string) => invoke<Tag[]>("list_tags", { accountId }),
  createTag: (accountId: string, name: string, color: string) =>
    invoke<Tag>("create_tag", { accountId, name, color }),
  updateTag: (id: number, patch: { name?: string; color?: string }) =>
    invoke<void>("update_tag", { id, name: patch.name ?? null, color: patch.color ?? null }),
  deleteTag: (id: number) => invoke<void>("delete_tag", { id }),
  /** 给单封邮件打 / 取消标签，幂等。 */
  tagMessage: (messageId: number, tagId: number, attached: boolean) =>
    invoke<void>("tag_message", { messageId, tagId, attached }),

  // ---- 发送 ----
  sendMail: (payload: SendPayload) => invoke<void>("send_mail", { payload }),

  // ---- AI ----
  aiStatus: () => invoke<AiStatus>("ai_status"),
  getAiSettings: () => invoke<AiSettings>("get_ai_settings"),
  setAiSettings: (settings: AiSettings) => invoke<void>("set_ai_settings", { settings }),
  summarizeMessage: (id: number, force = false) =>
    invoke<Summary>("summarize_message", { id, force }),
  /** 只读摘要缓存，未命中返回 null（不会触发模型推理）。 */
  getCachedSummary: (id: number) => invoke<Summary | null>("get_cached_summary", { id }),
};

/** 统一把后端错误（字符串）转成可展示文案。 */
export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e ?? "未知错误");
}
