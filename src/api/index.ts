import { Channel, invoke } from "@tauri-apps/api/core";

import type {
  Account,
  AccountInput,
  AiSettings,
  AiStatus,
  ClassifyEvent,
  DeleteReport,
  MailSettings,
  MessageDetail,
  MessageHeader,
  SendPayload,
  Summary,
  SyncReport,
  Tag,
  TagGroup,
  TestReport,
  TraySettings,
  UpdateInfo,
  UpdateSettings,
  WatchStateEvent,
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

  // ---- 实时收信 ----
  getMailSettings: () => invoke<MailSettings>("get_mail_settings"),
  setMailSettings: (settings: MailSettings) => invoke<void>("set_mail_settings", { settings }),
  /** 各账号监听状态快照（启动对齐用，此后靠 `mail:watch-state` 事件增量更新）。 */
  watchStates: () => invoke<WatchStateEvent[]>("watch_states"),
  /**
   * 列邮件。传 `tagIds` 时只返回打过其中**任一**标签的邮件（OR 语义）。
   * 空数组 / null 表示不筛选。筛在 SQL 层完成，不在前端过滤。
   */
  listMessages: (
    accountId: string,
    folder = "INBOX",
    limit = 200,
    offset = 0,
    tagIds: number[] = [],
  ) => invoke<MessageHeader[]>("list_messages", { accountId, folder, limit, offset, tagIds }),
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

  // ---- 标签组 ----
  listTagGroups: (accountId: string) => invoke<TagGroup[]>("list_tag_groups", { accountId }),
  createTagGroup: (accountId: string, name: string, color: string, tagIds: number[] = []) =>
    invoke<TagGroup>("create_tag_group", { accountId, name, color, tagIds }),
  updateTagGroup: (
    id: number,
    patch: { name?: string; color?: string; collapsed?: boolean; aiMode?: string },
  ) =>
    invoke<void>("update_tag_group", {
      id,
      name: patch.name ?? null,
      color: patch.color ?? null,
      collapsed: patch.collapsed ?? null,
      aiMode: patch.aiMode ?? null,
    }),
  deleteTagGroup: (id: number) => invoke<void>("delete_tag_group", { id }),
  /** 整组覆盖式设置成员标签。 */
  setGroupTags: (groupId: number, tagIds: number[]) =>
    invoke<void>("set_group_tags", { groupId, tagIds }),
  /** 把多个组展开为标签并集，供「选中组本身」时换算筛选集合。 */
  resolveGroupTags: (groupIds: number[]) => invoke<number[]>("resolve_group_tags", { groupIds }),

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
  /**
   * 批量智能整理：逐封推送进度事件（Channel 点对点流）。
   * 调用方自行处理 onmessage 的 start / item / done / error。
   */
  classifyMessages: (accountId: string, onEvent: Channel<ClassifyEvent>) =>
    invoke<void>("classify_messages", { accountId, onEvent }),
  /** 请求中止正在进行的整理（在消息边界生效）。 */
  cancelClassify: (accountId: string) => invoke<void>("cancel_classify", { accountId }),
  /**
   * 为单封邮件执行 AI 打标签（右键菜单入口）。
   * 不受「未整理队列」限制、不走快速通道；返回写回后的权威标签集合。
   */
  classifyMessage: (messageId: number) => invoke<number[]>("classify_message", { messageId }),

  // ---- 更新 ----
  /** 检查更新（查询 Gitee / GitHub 最新 Release）。force=true 时忽略「跳过此版本」。 */
  checkUpdate: (force = false) => invoke<UpdateInfo>("check_update", { force }),
  getUpdateSettings: () => invoke<UpdateSettings>("get_update_settings"),
  setUpdateSettings: (settings: UpdateSettings) =>
    invoke<void>("set_update_settings", { settings }),

  // ---- 托盘与开机自启 ----
  getTraySettings: () => invoke<TraySettings>("get_tray_settings"),
  setTraySettings: (settings: TraySettings) => invoke<void>("set_tray_settings", { settings }),
  /** 开机自启当前状态（读取 Windows 注册表实际值，非本地缓存）。 */
  getAutostart: () => invoke<boolean>("get_autostart"),
  setAutostart: (enabled: boolean) => invoke<void>("set_autostart", { enabled }),
};

/** 统一把后端错误（字符串）转成可展示文案。 */
export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e ?? "未知错误");
}
