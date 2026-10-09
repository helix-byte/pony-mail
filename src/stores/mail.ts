import { defineStore } from "pinia";
import { computed, reactive, ref } from "vue";

import { api, errorText } from "../api";
import type { Account, MessageDetail, MessageHeader, Summary, Tag } from "../api/types";
import { formatDateTime } from "../utils/format";

export type ToastKind = "info" | "ok" | "error";

export interface ToastItem {
  id: number;
  text: string;
  kind: ToastKind;
}

export interface ComposeDraft {
  to: string;
  cc: string;
  subject: string;
  body: string;
  html: boolean;
}

let toastSeq = 0;

export const useMailStore = defineStore("mail", () => {
  // ---- 数据 ----
  const accounts = ref<Account[]>([]);
  const activeAccountId = ref("");
  const messages = ref<MessageHeader[]>([]);
  const activeMessageId = ref<number | null>(null);
  const detail = ref<MessageDetail | null>(null);
  const lastSync = ref("");

  // ---- AI ----
  const summary = ref<Summary | null>(null);
  const summaryLoading = ref(false);
  const summaryError = ref("");

  // ---- 标签 ----
  const tags = ref<Tag[]>([]);
  /** 当前生效的标签筛选；null 表示看全部。 */
  const activeTagId = ref<number | null>(null);

  // ---- UI ----
  const ui = reactive({
    composeOpen: false,
    accountOpen: false,
    settingsOpen: false,
    composeDraft: null as ComposeDraft | null,
  });
  const busy = reactive({
    accounts: false,
    messages: false,
    detail: false,
    sync: false,
    sending: false,
  });
  const toasts = ref<ToastItem[]>([]);

  const activeAccount = computed(
    () => accounts.value.find((a) => a.id === activeAccountId.value) ?? null,
  );
  const unreadCount = computed(() => messages.value.filter((m) => !m.seen).length);
  const activeTag = computed(
    () => tags.value.find((t) => t.id === activeTagId.value) ?? null,
  );

  function toast(text: string, kind: ToastKind = "info") {
    const id = ++toastSeq;
    toasts.value.push({ id, text, kind });
    setTimeout(
      () => {
        toasts.value = toasts.value.filter((t) => t.id !== id);
      },
      kind === "error" ? 7000 : 3200,
    );
  }

  // ------------------------------------------------------------------
  // 账号
  // ------------------------------------------------------------------

  async function loadAccounts() {
    busy.accounts = true;
    try {
      accounts.value = await api.listAccounts();
      const stillThere = accounts.value.some((a) => a.id === activeAccountId.value);
      if (!stillThere) {
        activeAccountId.value = "";
        messages.value = [];
        detail.value = null;
        activeMessageId.value = null;
        tags.value = [];
        activeTagId.value = null;
      }
      if (!activeAccountId.value && accounts.value.length > 0) {
        await selectAccount(accounts.value[0].id);
      }
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      busy.accounts = false;
    }
  }

  async function selectAccount(id: string) {
    if (activeAccountId.value === id) return;
    activeAccountId.value = id;
    // 标签按账号隔离，切账号必须清掉上一个账号的标签与筛选，
    // 否则会拿 A 账号的 tagId 去筛 B 账号的邮件。
    tags.value = [];
    activeTagId.value = null;
    resetReader();
    await Promise.all([loadMessages(), loadTags()]);
  }

  async function removeAccount(id: string) {
    try {
      await api.deleteAccount(id);
      toast("账号已删除", "ok");
      await loadAccounts();
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  // ------------------------------------------------------------------
  // 列表
  // ------------------------------------------------------------------

  async function loadMessages() {
    if (!activeAccountId.value) {
      messages.value = [];
      return;
    }
    const accountId = activeAccountId.value;
    const tagId = activeTagId.value;
    busy.messages = true;
    try {
      const list = await api.listMessages(accountId, "INBOX", 200, 0, tagId);
      // 竞态保护：等待期间用户可能已切账号或切了筛选，旧结果不能覆盖新结果。
      if (activeAccountId.value !== accountId || activeTagId.value !== tagId) return;
      messages.value = list;
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      busy.messages = false;
    }
  }

  /** 切换标签筛选：null 看全部，否则只看该标签下的邮件。 */
  async function selectTag(id: number | null) {
    if (activeTagId.value === id) return;
    activeTagId.value = id;
    resetReader();
    await loadMessages();
  }

  // ------------------------------------------------------------------
  // 标签
  // ------------------------------------------------------------------

  async function loadTags() {
    if (!activeAccountId.value) {
      tags.value = [];
      return;
    }
    const accountId = activeAccountId.value;
    try {
      const list = await api.listTags(accountId);
      if (activeAccountId.value !== accountId) return;
      tags.value = list;
      // 筛选中的标签被别处删掉了 → 退回「全部」，避免停在一个空结果页。
      if (activeTagId.value !== null && !list.some((t) => t.id === activeTagId.value)) {
        activeTagId.value = null;
        void loadMessages();
      }
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  async function createTag(name: string, color: string): Promise<boolean> {
    if (!activeAccountId.value) {
      toast("请先添加一个邮箱账号", "info");
      return false;
    }
    try {
      await api.createTag(activeAccountId.value, name, color);
      await loadTags();
      toast(`已创建标签「${name.trim()}」`, "ok");
      return true;
    } catch (e) {
      toast(errorText(e), "error");
      return false;
    }
  }

  async function updateTag(id: number, patch: { name?: string; color?: string }) {
    try {
      await api.updateTag(id, patch);
      await loadTags();
      toast("标签已更新", "ok");
      return true;
    } catch (e) {
      toast(errorText(e), "error");
      return false;
    }
  }

  async function deleteTag(id: number) {
    try {
      await api.deleteTag(id);
      // 本地先摘掉，避免侧栏在重新拉取前短暂闪回旧标签。
      tags.value = tags.value.filter((t) => t.id !== id);
      // 邮件上的标签点也要同步摘掉，否则会留下「幽灵标签」。
      for (const m of messages.value) {
        m.tagIds = m.tagIds.filter((tid) => tid !== id);
      }
      if (detail.value) {
        detail.value.header.tagIds = detail.value.header.tagIds.filter((tid) => tid !== id);
      }
      if (activeTagId.value === id) {
        activeTagId.value = null;
        await loadMessages();
      }
      toast("标签已删除", "ok");
      return true;
    } catch (e) {
      toast(errorText(e), "error");
      return false;
    }
  }

  /** 给一封邮件切换某个标签的挂载状态。 */
  async function toggleMessageTag(messageId: number, tagId: number) {
    const header =
      messages.value.find((m) => m.id === messageId) ??
      (detail.value?.header.id === messageId ? detail.value.header : undefined);
    const attached = header?.tagIds.includes(tagId) ?? false;
    const next = !attached;

    // 乐观更新：打标是纯本地写，回滚成本低，等往返会让点击有明显延迟。
    applyTagLocally(messageId, tagId, next);
    try {
      await api.tagMessage(messageId, tagId, next);
      await loadTags(); // 刷新角标计数
    } catch (e) {
      applyTagLocally(messageId, tagId, attached); // 回滚
      toast(errorText(e), "error");
      return;
    }

    // 正在按标签筛选且刚把这封邮件从该标签下摘掉 → 它应立即从列表消失。
    if (activeTagId.value === tagId && !next) {
      await loadMessages();
    }
  }

  function applyTagLocally(messageId: number, tagId: number, attached: boolean) {
    for (const target of [messages.value, detail.value ? [detail.value.header] : []]) {
      const m = target.find((h) => h.id === messageId);
      if (!m) continue;
      if (attached) {
        if (!m.tagIds.includes(tagId)) m.tagIds = [...m.tagIds, tagId].sort((a, b) => a - b);
      } else {
        m.tagIds = m.tagIds.filter((tid) => tid !== tagId);
      }
    }
  }

  /** 取某封邮件当前挂载的标签对象列表（列表 / 阅读区渲染用）。 */
  function tagsOf(messageId: number): Tag[] {
    const header =
      messages.value.find((m) => m.id === messageId) ??
      (detail.value?.header.id === messageId ? detail.value.header : undefined);
    if (!header) return [];
    return header.tagIds
      .map((id) => tags.value.find((t) => t.id === id))
      .filter((t): t is Tag => t !== undefined);
  }

  async function sync() {
    if (!activeAccountId.value) {
      toast("请先添加一个邮箱账号", "info");
      return;
    }
    busy.sync = true;
    try {
      const report = await api.syncInbox(activeAccountId.value);
      lastSync.value = new Date().toLocaleTimeString("zh-CN", {
        hour: "2-digit",
        minute: "2-digit",
      });
      toast(
        report.inserted > 0
          ? `同步完成，新增 ${report.inserted} 封（服务器共 ${report.totalOnServer} 封）`
          : `没有新邮件（服务器共 ${report.totalOnServer} 封）`,
        "ok",
      );
      await loadMessages();
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      busy.sync = false;
    }
  }

  // ------------------------------------------------------------------
  // 阅读
  // ------------------------------------------------------------------

  function resetReader() {
    activeMessageId.value = null;
    detail.value = null;
    summary.value = null;
    summaryError.value = "";
  }

  async function openMessage(id: number) {
    if (activeMessageId.value === id && detail.value) return;
    activeMessageId.value = id;
    detail.value = null;
    summary.value = null;
    summaryError.value = "";
    busy.detail = true;
    try {
      const d = await api.loadMessageBody(id);
      detail.value = d;
      if (!d.header.seen) {
        await setSeen(id, true, false);
      }
      // 正文就绪后回填已有摘要缓存：有缓存就直接展示，
      // 不再让用户先看到「生成摘要」按钮再手动点一次。
      //
      // 这里刻意 await：缓存是本地 SQLite 读取，耗时极短，
      // 但若不等它，`busy.detail` 会先落下、页面先渲染出「生成摘要」引导态，
      // 再被缓存内容替换——形成一次可见的闪动。等它一起渲染更干净。
      await loadCachedSummary(id);
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      busy.detail = false;
    }
  }

  /**
   * 拉取摘要缓存，**不触发模型推理**（后端 `get_cached_summary` 只读 `ai_cache`）。
   *
   * 未命中时后端返回 `null`，这里保持 `summary` 为空即可，
   * 界面自然落到「生成摘要」引导态，不弹任何错误提示——
   * 用户没主动要求生成，读缓存失败不该打扰他。
   */
  async function loadCachedSummary(id: number) {
    try {
      const cached = await api.getCachedSummary(id);
      // 竞态保护：等待期间用户可能已切走或删掉了这封邮件。
      if (activeMessageId.value !== id) return;
      if (cached) summary.value = cached;
    } catch {
      // 静默失败：缓存读取异常不该影响阅读正文这一主流程。
    }
  }

  async function setSeen(id: number, seen: boolean, notify = true) {
    try {
      await api.setSeen(id, seen);
      syncSeenLocally(id, seen);
      if (notify) toast(seen ? "已标记为已读" : "已标记为未读", "ok");
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  function syncSeenLocally(id: number, seen: boolean) {
    const m = messages.value.find((x) => x.id === id);
    if (m) m.seen = seen;
    if (detail.value && detail.value.header.id === id) {
      detail.value.header.seen = seen;
    }
  }

  async function toggleFlag(id: number) {
    const current =
      messages.value.find((x) => x.id === id)?.flagged ??
      (detail.value?.header.id === id ? detail.value.header.flagged : false);
    const next = !current;
    try {
      await api.setFlagged(id, next);
      const m = messages.value.find((x) => x.id === id);
      if (m) m.flagged = next;
      if (detail.value && detail.value.header.id === id) {
        detail.value.header.flagged = next;
      }
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  // ------------------------------------------------------------------
  // 删除
  // ------------------------------------------------------------------

  /**
   * 删除单封邮件（服务器 + 本地）。
   *
   * 后端约定「服务器删除失败不阻断本地清理」，因此这里必须区分两种结果：
   * 全部成功 → 绿色提示；仅本地删除 → 黄色警告并透出服务器原因，
   * 避免用户误以为服务器上也删干净了。
   */
  async function deleteMessage(id: number) {
    try {
      const report = await api.deleteMessage(id);
      // 从列表移除，保持界面与后端一致（不重新拉取，避免闪烁）。
      messages.value = messages.value.filter((m) => m.id !== id);
      // 删掉的正是当前打开的邮件 → 清空阅读区。
      if (activeMessageId.value === id) {
        resetReader();
      }
      if (report.deletedRemote) {
        toast("邮件已删除", "ok");
      } else {
        toast(
          `已在本地删除，但服务器删除失败：${report.remoteError || "原因未知"}`,
          "error",
        );
      }
      return report;
    } catch (e) {
      toast(errorText(e), "error");
      return null;
    }
  }

  /** 批量删除（供多选删除复用）。 */
  async function deleteMessages(ids: number[]) {
    if (ids.length === 0) return null;
    try {
      const report = await api.deleteMessages(ids);
      const idSet = new Set(ids);
      messages.value = messages.value.filter((m) => !idSet.has(m.id));
      if (activeMessageId.value !== null && idSet.has(activeMessageId.value)) {
        resetReader();
      }
      if (report.deletedRemote) {
        toast(`已删除 ${report.deletedLocal} 封邮件`, "ok");
      } else {
        toast(
          `已在本地删除 ${report.deletedLocal} 封，但服务器删除失败：${report.remoteError || "原因未知"}`,
          "error",
        );
      }
      return report;
    } catch (e) {
      toast(errorText(e), "error");
      return null;
    }
  }

  // ------------------------------------------------------------------
  // AI 摘要
  // ------------------------------------------------------------------

  async function summarize(force = false) {
    const id = activeMessageId.value;
    if (!id) return;
    summaryLoading.value = true;
    summaryError.value = "";
    try {
      summary.value = await api.summarizeMessage(id, force);
    } catch (e) {
      summaryError.value = errorText(e);
    } finally {
      summaryLoading.value = false;
    }
  }

  // ------------------------------------------------------------------
  // 写信
  // ------------------------------------------------------------------

  function openCompose(draft: ComposeDraft | null = null) {
    ui.composeDraft = draft;
    ui.composeOpen = true;
  }

  function closeCompose() {
    ui.composeOpen = false;
    ui.composeDraft = null;
  }

  function replyToActive() {
    const d = detail.value;
    if (!d) return;
    const subject = d.header.subject.startsWith("Re:")
      ? d.header.subject
      : `Re: ${d.header.subject}`;
    openCompose({
      to: d.header.fromAddr,
      cc: "",
      subject,
      body: `\n\n\n——— 原始邮件 ———\n发件人：${d.header.fromName} <${d.header.fromAddr}>\n时间：${formatDateTime(d.header.dateTs)}\n主题：${d.header.subject}\n\n${d.bodyText}`,
      html: false,
    });
  }

  function forwardActive() {
    const d = detail.value;
    if (!d) return;
    const subject = d.header.subject.startsWith("Fwd:")
      ? d.header.subject
      : `Fwd: ${d.header.subject}`;
    openCompose({
      to: "",
      cc: "",
      subject,
      body: `\n\n\n——— 转发邮件 ———\n发件人：${d.header.fromName} <${d.header.fromAddr}>\n收件人：${d.toAddrs}\n时间：${formatDateTime(d.header.dateTs)}\n主题：${d.header.subject}\n\n${d.bodyText}`,
      html: false,
    });
  }

  async function send(payload: {
    to: string;
    cc: string;
    subject: string;
    body: string;
    html: boolean;
  }) {
    if (!activeAccountId.value) {
      toast("没有可用的发信账号", "error");
      return false;
    }
    busy.sending = true;
    try {
      await api.sendMail({
        accountId: activeAccountId.value,
        to: payload.to,
        cc: payload.cc,
        subject: payload.subject,
        body: payload.body,
        html: payload.html,
      });
      toast("邮件已发送", "ok");
      return true;
    } catch (e) {
      toast(errorText(e), "error");
      return false;
    } finally {
      busy.sending = false;
    }
  }

  return {
    accounts,
    activeAccountId,
    messages,
    activeMessageId,
    detail,
    lastSync,
    summary,
    summaryLoading,
    summaryError,
    tags,
    activeTagId,
    ui,
    busy,
    toasts,
    activeAccount,
    activeTag,
    unreadCount,
    toast,
    loadAccounts,
    selectAccount,
    removeAccount,
    loadMessages,
    selectTag,
    loadTags,
    createTag,
    updateTag,
    deleteTag,
    toggleMessageTag,
    tagsOf,
    sync,
    resetReader,
    openMessage,
    setSeen,
    toggleFlag,
    deleteMessage,
    deleteMessages,
    summarize,
    openCompose,
    closeCompose,
    replyToActive,
    forwardActive,
    send,
  };
});
