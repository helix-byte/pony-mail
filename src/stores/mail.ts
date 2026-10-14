import { Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { defineStore } from "pinia";
import { computed, reactive, ref } from "vue";

import { api, errorText } from "../api";
import type {
  Account,
  AiSettings,
  ClassifyEvent,
  MessageDetail,
  MessageHeader,
  NewMailEvent,
  Summary,
  Tag,
  TagGroup,
  UpdateInfo,
  UpdateSettings,
  WatchStateEvent,
} from "../api/types";
import { formatDateTime } from "../utils/format";
import { isGmailHost } from "../utils/provider";

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

/**
 * 自动摘要触发前的静默期。
 *
 * 用户翻邮件比模型推理快得多：如果没有这段缓冲，快速点过 N 封就会让本地
 * 模型为每封都排队完整跑一遍。静默期内切走即取消，只有真正停下来阅读的
 * 邮件才会触发推理。
 */
const AUTO_SUMMARY_DELAY_MS = 500;

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
  /** 自动摘要已排程、尚在静默期（见 scheduleAutoSummary），UI 据此提前进入「推理中」态。 */
  const summaryPending = ref(false);

  // ---- 智能整理 ----
  /** AI 设置缓存（含「同步后自动整理」开关，缺省视为开启）。 */
  const aiSettings = ref<AiSettings | null>(null);
  /** 是否正在批量整理（账户级互斥，运行期间禁用入口）。 */
  const classifying = ref(false);
  /** 单封「AI 自动标签」是否在途（防重复触发，右键菜单据此禁用）。 */
  const classifyOneBusy = ref(false);
  /** 整理进度（逐封推进；null 表示未在整理）。 */
  const classifyProgress = ref<{ done: number; total: number } | null>(null);
  const autoClassify = computed(() => aiSettings.value?.autoClassify ?? true);
  /** 打开邮件时自动生成摘要（设置未加载时按默认开启处理）。 */
  const autoSummarize = computed(() => aiSettings.value?.autoSummarize ?? true);

  // ---- 标签 ----
  const tags = ref<Tag[]>([]);
  /** 标签组（内含成员标签 id 与去重计数）。 */
  const groups = ref<TagGroup[]>([]);
  /**
   * 当前生效的筛选标签**集合**（多选，OR 语义）。
   *
   * 用集合而非单值，是因为侧栏既支持勾选单个标签，也支持
   * 「选中某个组」＝选中该组全部标签，还可能组间多选。
   * 空集合 = 看全部邮件。
   */
  const activeTagIds = ref<number[]>([]);
  /** 被直接选中的组（用于侧栏高亮「这个组在筛选中」）。 */
  const activeGroupId = ref<number | null>(null);

  // ---- 实时收信 ----
  /**
   * 各账号的 IMAP 监听状态（后端 `mail:watch-state` 事件与启动快照）。
   *
   * 缺省无键 = 状态未知（如事件尚未到达）；UI 只认 online / reconnecting。
   */
  const watchStates = ref<Record<string, WatchStateEvent>>({});
  /** 当前账号的监听状态（工具条指示用；null = 无信息）。 */
  const activeWatch = computed(() => watchStates.value[activeAccountId.value] ?? null);

  // ---- UI ----
  const ui = reactive({
    composeOpen: false,
    accountOpen: false,
    settingsOpen: false,
    updateOpen: false,
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

  /** 筛选中的标签对象（用于顶部筛选条展示）。 */
  const activeTags = computed(() =>
    activeTagIds.value
      .map((id) => tags.value.find((t) => t.id === id))
      .filter((t): t is Tag => t !== undefined),
  );
  /** 是否处于筛选态。 */
  const filtering = computed(() => activeTagIds.value.length > 0);
  const activeGroup = computed(
    () => groups.value.find((g) => g.id === activeGroupId.value) ?? null,
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
        resetReader();
        tags.value = [];
        groups.value = [];
        activeTagIds.value = [];
        activeGroupId.value = null;
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
    // 标签与组都按账号隔离，切账号必须清掉上一个账号的筛选集合，
    // 否则会拿 A 账号的 tagId 去筛 B 账号的邮件。
    tags.value = [];
    groups.value = [];
    activeTagIds.value = [];
    activeGroupId.value = null;
    resetReader();
    await Promise.all([loadMessages(), loadTags(), loadGroups()]);
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

  /**
   * 拉取当前（账号 + 筛选）下的邮件列表。
   *
   * `silent` 为 true 时不进入 loading 态——实时收信的新邮件刷新用：
   * 这类刷新是后台行为，闪一下「正在加载本地缓存…」会打断阅读。
   */
  async function loadMessages(silent = false) {
    if (!activeAccountId.value) {
      messages.value = [];
      return;
    }
    const accountId = activeAccountId.value;
    // 快照筛选集合并排序成稳定 key，用于下面的竞态比对。
    const tagIds = [...activeTagIds.value].sort((a, b) => a - b);
    const filterKey = tagIds.join(",");
    if (!silent) busy.messages = true;
    try {
      const list = await api.listMessages(accountId, "INBOX", 200, 0, tagIds);
      // 竞态保护：等待期间用户可能已切账号或改了筛选，旧结果不能覆盖新结果。
      const nowKey = [...activeTagIds.value].sort((a, b) => a - b).join(",");
      if (activeAccountId.value !== accountId || nowKey !== filterKey) return;
      messages.value = list;
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      if (!silent) busy.messages = false;
    }
  }

  /** 清空筛选，回到全部邮件。 */
  async function clearFilter() {
    if (activeTagIds.value.length === 0 && activeGroupId.value === null) return;
    activeTagIds.value = [];
    activeGroupId.value = null;
    resetReader();
    await loadMessages();
  }

  /**
   * 勾选 / 取消勾选单个标签（多选，OR 语义）。
   *
   * 与「选中组」是两种独立操作：点在标签上只动这一个标签，
   * 点组名才是整组。多对多下同一标签可能出现在多个组里，
   * 因此不能靠「取消组」去推断要摘掉哪些标签。
   */
  async function toggleTagFilter(tagId: number) {
    const next = activeTagIds.value.includes(tagId)
      ? activeTagIds.value.filter((id) => id !== tagId)
      : [...activeTagIds.value, tagId];
    activeTagIds.value = next;
    // 手动改标签后，「选中的组」不再精确对应筛选集合，清掉高亮避免误导。
    activeGroupId.value = null;
    resetReader();
    await loadMessages();
  }

  /**
   * 选中某个组：把该组全部标签设为筛选集合。
   *
   * 再次点击同一个组则取消（回到全部）。
   */
  async function selectGroup(groupId: number) {
    if (activeGroupId.value === groupId) {
      await clearFilter();
      return;
    }
    const group = groups.value.find((g) => g.id === groupId);
    if (!group) return;
    if (group.tagIds.length === 0) {
      toast(`标签组「${group.name}」还没有标签，先编辑组添加几个`, "info");
      return;
    }
    activeGroupId.value = groupId;
    activeTagIds.value = [...group.tagIds];
    resetReader();
    await loadMessages();
  }

  /** 在「选中组」状态下追加另一个组的标签（组间多选）。 */
  async function addGroupToFilter(groupId: number) {
    const group = groups.value.find((g) => g.id === groupId);
    if (!group || group.tagIds.length === 0) {
      toast("这个组还没有标签", "info");
      return;
    }
    const merged = new Set([...activeTagIds.value, ...group.tagIds]);
    activeTagIds.value = [...merged].sort((a, b) => a - b);
    // 集合已不再等同于单一组，取消组高亮。
    activeGroupId.value = null;
    resetReader();
    await loadMessages();
  }

  /** 按标签 id 集合直接筛选（供切换组、恢复状态等场景复用）。 */
  async function filterByTags(ids: number[], groupId: number | null = null) {
    activeTagIds.value = [...new Set(ids)].sort((a, b) => a - b);
    activeGroupId.value = groupId;
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
      // 筛选中的标签被别处删掉了 → 从集合里摘掉，摘空了就退回「全部」。
      const alive = new Set(list.map((t) => t.id));
      const pruned = activeTagIds.value.filter((id) => alive.has(id));
      if (pruned.length !== activeTagIds.value.length) {
        activeTagIds.value = pruned;
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
      // 组内成员关系在服务端已随标签删除一并清理，重新拉组以刷新成员与计数。
      void loadGroups();
      // 被删的标签若在筛选集合里，摘掉它；集合空了自然回到全部邮件。
      if (activeTagIds.value.includes(id)) {
        activeTagIds.value = activeTagIds.value.filter((tid) => tid !== id);
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
      await loadTags(); // 刷新标签角标
      void loadGroups(); // 组计数也随之变化
    } catch (e) {
      applyTagLocally(messageId, tagId, attached); // 回滚
      toast(errorText(e), "error");
      return;
    }

    // 正在按该标签筛选且刚把这封邮件从它下面摘掉 → 邮件应立即从列表消失。
    // 注意：多选 OR 语义下，只有当这封邮件不再命中集合里**任何**标签时才该消失。
    if (activeTagIds.value.includes(tagId) && !next) {
      const stillMatches = header ? header.tagIds.some((t) => activeTagIds.value.includes(t)) : false;
      if (!stillMatches) await loadMessages();
    }
  }

  // ------------------------------------------------------------------
  // 标签组
  // ------------------------------------------------------------------

  async function loadGroups() {
    if (!activeAccountId.value) {
      groups.value = [];
      return;
    }
    const accountId = activeAccountId.value;
    try {
      const list = await api.listTagGroups(accountId);
      if (activeAccountId.value !== accountId) return;
      groups.value = list;
      // 选中的组被删了 → 清掉高亮（筛选集合本身在 loadTags 里已被剪枝）。
      if (activeGroupId.value !== null && !list.some((g) => g.id === activeGroupId.value)) {
        activeGroupId.value = null;
      }
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  async function createGroup(
    name: string,
    color: string,
    tagIds: number[] = [],
    aiMode = "off",
  ) {
    if (!activeAccountId.value) {
      toast("请先添加一个邮箱账号", "info");
      return false;
    }
    try {
      const created = await api.createTagGroup(activeAccountId.value, name, color, tagIds);
      // create_tag_group 不带 ai_mode，需要时补一次更新。
      if (aiMode !== "off") {
        await api.updateTagGroup(created.id, { aiMode });
      }
      await loadGroups();
      toast(`已创建标签组「${name.trim()}」`, "ok");
      return true;
    } catch (e) {
      toast(errorText(e), "error");
      return false;
    }
  }

  async function updateGroup(
    id: number,
    patch: {
      name?: string;
      color?: string;
      collapsed?: boolean;
      tagIds?: number[];
      aiMode?: string;
    },
  ) {
    try {
      const { tagIds, ...rest } = patch;
      if (
        rest.name !== undefined ||
        rest.color !== undefined ||
        rest.collapsed !== undefined ||
        rest.aiMode !== undefined
      ) {
        await api.updateTagGroup(id, rest);
      }
      if (tagIds) {
        await api.setGroupTags(id, tagIds);
      }
      await loadGroups();
      // 若正在筛选这个组，成员变了 → 筛选集合要跟着变。
      if (activeGroupId.value === id && tagIds) {
        activeTagIds.value = [...tagIds].sort((a, b) => a - b);
        await loadMessages();
      }
      toast("标签组已更新", "ok");
      return true;
    } catch (e) {
      toast(errorText(e), "error");
      return false;
    }
  }

  async function deleteGroup(id: number) {
    try {
      await api.deleteTagGroup(id);
      groups.value = groups.value.filter((g) => g.id !== id);
      // 删组只解绑成员，标签本身仍在，因此筛选集合不受影响。
      if (activeGroupId.value === id) {
        activeGroupId.value = null;
      }
      toast("标签组已删除（标签本身保留）", "ok");
      return true;
    } catch (e) {
      toast(errorText(e), "error");
      return false;
    }
  }

  /** 折叠/展开组。纯 UI 状态，持久化在后端，失败仅提示不阻断。 */
  async function toggleGroupCollapsed(id: number) {
    const g = groups.value.find((x) => x.id === id);
    if (!g) return;
    const next = !g.collapsed;
    // 先改本地让交互即时，再落库。
    g.collapsed = next;
    try {
      await api.updateTagGroup(id, { collapsed: next });
    } catch (e) {
      g.collapsed = !next; // 回滚
      toast(errorText(e), "error");
    }
  }

  /** 某标签所属的组名列表（侧栏「归属」提示用）。 */
  function groupsOfTag(tagId: number): TagGroup[] {
    return groups.value.filter((g) => g.tagIds.includes(tagId));
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
      // 同步完成后自动整理新邮件（默认开启，可在「设置」中关闭）。
      if (autoClassify.value) void classifyAll();
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      busy.sync = false;
    }
  }

  // ------------------------------------------------------------------
  // 实时收信
  // ------------------------------------------------------------------

  /** 监听事件是否已接入（initRealtime 幂等保护）。 */
  let realtimeReady = false;

  /**
   * 接入后端实时收信事件（启动时调用一次，幂等）。
   *
   * - `mail:new`：新邮件到达 → 应用内提示 + 刷新当前账号列表，
   *   并按「同步后自动整理」设置触发整理；
   * - `mail:watch-state`：监听连接状态 → 工具条绿灯/重连提示。
   */
  async function initRealtime() {
    if (realtimeReady) return;
    realtimeReady = true;
    try {
      // 先取快照再挂监听：顺序颠倒会丢掉「快照读取与监听建立之间」的状态变更。
      for (const s of await api.watchStates()) {
        watchStates.value[s.accountId] = s;
      }
      await listen<WatchStateEvent>("mail:watch-state", (e) => {
        watchStates.value[e.payload.accountId] = e.payload;
      });
      await listen<NewMailEvent>("mail:new", (e) => onNewMail(e.payload));
    } catch (e) {
      // 实时链路挂不上不影响手动同步，提示一次即可。
      realtimeReady = false;
      toast(errorText(e), "error");
    }
  }

  /**
   * 后端推送的「新邮件到达」处理：
   * 先应用内提示（任何账号都提示），当前账号再静默刷新列表并触发自动整理。
   */
  function onNewMail(payload: NewMailEvent) {
    const newest = payload.messages[0];
    const prefix =
      payload.accountId === activeAccountId.value
        ? ""
        : `【${accounts.value.find((a) => a.id === payload.accountId)?.email ?? "未知账号"}】`;
    if (payload.count === 1 && newest) {
      const from = newest.fromName || newest.fromAddr;
      toast(`${prefix}新邮件：${newest.subject || "（无主题）"}（${from}）`, "ok");
    } else {
      toast(`${prefix}收到 ${payload.count} 封新邮件`, "ok");
    }

    // 其它账号的新邮件只提示，不打扰当前界面。
    if (payload.accountId !== activeAccountId.value) return;
    lastSync.value = new Date().toLocaleTimeString("zh-CN", {
      hour: "2-digit",
      minute: "2-digit",
    });
    void loadMessages(true);
    // 与手动同步一致：新邮件到达后自动整理（默认开启，可在「设置」中关闭）。
    if (autoClassify.value) void classifyAll();
  }

  // ------------------------------------------------------------------
  // 更新检查
  // ------------------------------------------------------------------

  /** 最新一次检查结果（null = 尚无结果）。 */
  const updateInfo = ref<UpdateInfo | null>(null);
  /** 更新偏好缓存（自动检查开关、已跳过版本）。 */
  const updateSettings = ref<UpdateSettings | null>(null);
  /** 手动检查进行中（设置页按钮据此禁用）。 */
  const updateChecking = ref(false);

  /** 拉取更新偏好（失败静默——设置读取异常不该打断主流程）。 */
  async function loadUpdateSettings() {
    try {
      updateSettings.value = await api.getUpdateSettings();
    } catch {
      // 保持 null，按默认值（自动检查开启、未跳过任何版本）处理。
    }
  }

  /**
   * 手动检查更新（设置页入口）。force=true 忽略「跳过此版本」。
   * 失败时 toast 并返回 null；成功返回结果并写入 updateInfo。
   */
  async function checkUpdate(): Promise<UpdateInfo | null> {
    if (updateChecking.value) return null;
    updateChecking.value = true;
    try {
      const info = await api.checkUpdate(true);
      updateInfo.value = info;
      return info;
    } catch (e) {
      toast(errorText(e), "error");
      return null;
    } finally {
      updateChecking.value = false;
    }
  }

  /** 跳过某个版本：写入偏好，自动检查不再提示它（手动检查仍显示）。 */
  async function skipVersion(version: string) {
    try {
      // 以后端为准现读一次：缓存缺失或过期时，直接写盘会把「自动检查开关」等字段覆盖回旧值。
      const current = await api.getUpdateSettings();
      updateSettings.value = { ...current, skippedVersion: version };
      await api.setUpdateSettings(updateSettings.value);
      toast(`已跳过 v${version} 的更新提示，可在「设置」中手动检查`, "info");
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  /** 监听事件是否已接入（initUpdater 幂等保护）。 */
  let updaterReady = false;

  /**
   * 接入后端更新事件（启动时调用一次，幂等）。
   *
   * 自动检查在 Rust 侧启动后延迟触发；发现新版本时推送 `update:available`，
   * 这里落库到状态并弹出更新对话框。
   */
  async function initUpdater() {
    if (updaterReady) return;
    updaterReady = true;
    try {
      await listen<UpdateInfo>("update:available", (e) => {
        updateInfo.value = e.payload;
        ui.updateOpen = true;
      });
      void loadUpdateSettings();
    } catch (e) {
      updaterReady = false;
      toast(errorText(e), "error");
    }
  }

  // ------------------------------------------------------------------
  // 阅读
  // ------------------------------------------------------------------

  /**
   * 阅读区请求序号：打开新邮件 / 清空阅读区时自增。
   * 在途的正文拉取迟到回来时据此判断「已过时」并丢弃，防止写进后打开的邮件。
   */
  let readerSeq = 0;
  /** 自动摘要排程定时器（静默期内未开跑，见 scheduleAutoSummary）。 */
  let autoSummaryTimer: ReturnType<typeof setTimeout> | null = null;

  /** 撤销在途的自动摘要排程（切邮件 / 清空阅读区 / 手动触发时调用）。 */
  function cancelAutoSummary() {
    if (autoSummaryTimer !== null) {
      clearTimeout(autoSummaryTimer);
      autoSummaryTimer = null;
    }
    summaryPending.value = false;
  }

  /**
   * 排程一次自动摘要：静默期结束后，若该邮件仍是当前打开的、且尚无摘要，
   * 就自动开始生成。静默期内用户切走会被 cancelAutoSummary 取消。
   */
  function scheduleAutoSummary(id: number) {
    cancelAutoSummary();
    if (!autoSummarize.value) return;
    summaryPending.value = true;
    autoSummaryTimer = setTimeout(() => {
      autoSummaryTimer = null;
      summaryPending.value = false;
      if (activeMessageId.value !== id || summary.value || summaryLoading.value) return;
      void summarize(false, true);
    }, AUTO_SUMMARY_DELAY_MS);
  }

  function resetReader() {
    // 自增序号让在途的正文拉取 / 自动摘要结果全部作废（见 openMessage）。
    readerSeq += 1;
    cancelAutoSummary();
    activeMessageId.value = null;
    detail.value = null;
    summary.value = null;
    summaryError.value = "";
    summaryLoading.value = false;
    // 若正有一封邮件在拉正文，阅读区已清空，不该继续显示「拉取中」。
    busy.detail = false;
  }

  async function openMessage(id: number) {
    if (activeMessageId.value === id && detail.value) return;
    const seq = ++readerSeq;
    cancelAutoSummary();
    activeMessageId.value = id;
    detail.value = null;
    summary.value = null;
    summaryError.value = "";
    // 上一封在途的摘要结果会被 summarize 的竞态保护丢弃，它的 loading 一并清掉。
    summaryLoading.value = false;
    busy.detail = true;
    try {
      const d = await api.loadMessageBody(id);
      if (!d.header.seen) {
        await setSeen(id, true, false);
      }
      // 竞态保护：首次拉正文要走 IMAP 可达数秒，期间用户可能已切到别的邮件，
      // 迟到的正文不能盖到当前阅读区上（序号不符直接放弃，正文已落库不浪费）。
      if (readerSeq !== seq) return;
      detail.value = d;
      // 正文就绪后回填已有摘要缓存：有缓存就直接展示，
      // 不再让用户先看到「生成摘要」按钮再手动点一次。
      //
      // 这里刻意 await：缓存是本地 SQLite 读取，耗时极短，
      // 但若不等它，`busy.detail` 会先落下、页面先渲染出「生成摘要」引导态，
      // 再被缓存内容替换——形成一次可见的闪动。等它一起渲染更干净。
      await loadCachedSummary(id);
      // 缓存未命中且开了自动摘要 → 稍候自动生成，实现「点开即总结」。
      if (readerSeq === seq && !summary.value) {
        scheduleAutoSummary(id);
      }
    } catch (e) {
      if (readerSeq === seq) toast(errorText(e), "error");
    } finally {
      // 只有仍是最新一次打开才复位，避免把后一封邮件的 loading 提前关掉。
      if (readerSeq === seq) busy.detail = false;
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
      // 标签与组角标按 message_tags 实时聚合，删除清掉打标关系后必须重拉，否则角标虚高。
      void loadTags();
      void loadGroups();
      if (report.deletedRemote) {
        // Gmail 的删除实际是「移入回收站」，提示文案如实说明，避免用户以为已彻底删除。
        toast(isGmailHost(activeAccount.value?.imapHost) ? "邮件已移入 Gmail 回收站" : "邮件已删除", "ok");
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
      // 同 deleteMessage：批量删除同样清掉 message_tags，标签与组角标要重拉刷新。
      void loadTags();
      void loadGroups();
      if (report.deletedRemote) {
        const deletedText = isGmailHost(activeAccount.value?.imapHost)
          ? `已将 ${report.deletedLocal} 封邮件移入 Gmail 回收站`
          : `已删除 ${report.deletedLocal} 封邮件`;
        toast(deletedText, "ok");
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

  /**
   * 生成摘要。
   *
   * `auto` 为 true 表示由「打开邮件自动摘要」触发：失败时静默回落「生成摘要」
   * 引导态，不展示错误——用户没主动要求，读邮件不该被报错打断；
   * 想手动重试时自会看到具体原因。
   */
  async function summarize(force = false, auto = false) {
    const id = activeMessageId.value;
    if (!id) return;
    // 手动触发先撤销自动排程，避免同一次点击跑两遍推理。
    if (!auto) cancelAutoSummary();
    summaryLoading.value = true;
    summaryError.value = "";
    try {
      const s = await api.summarizeMessage(id, force);
      // 竞态保护：等待期间用户可能已切到别的邮件，迟到的摘要不能写进当前阅读区。
      if (activeMessageId.value !== id) return;
      summary.value = s;
    } catch (e) {
      if (activeMessageId.value !== id) return;
      if (!auto) summaryError.value = errorText(e);
    } finally {
      // 只复位自己这封邮件的 loading，避免误关后一封邮件正在跑的推理状态。
      if (activeMessageId.value === id) summaryLoading.value = false;
    }
  }

  // ------------------------------------------------------------------
  // 智能整理
  // ------------------------------------------------------------------

  /** 拉取 AI 设置（自动整理开关等）。失败静默——设置读取异常不该打断主流程。 */
  async function loadAiSettings() {
    try {
      aiSettings.value = await api.getAiSettings();
    } catch {
      // 保持 null，按默认值（开启自动整理）处理。
    }
  }

  /** 把某封邮件的标签集合就地更新（整理事件回推用，避免整表重拉）。 */
  function setTagsLocally(messageId: number, tagIds: number[]) {
    const sorted = [...tagIds].sort((a, b) => a - b);
    const m = messages.value.find((x) => x.id === messageId);
    if (m) m.tagIds = sorted;
    if (detail.value && detail.value.header.id === messageId) {
      detail.value.header.tagIds = sorted;
    }
  }

  /**
   * 发起批量智能整理（手动按钮与「同步后自动整理」共用）。
   *
   * 逐封进度经 Tauri Channel 点对点推送；运行期间 `classifying` 为真，
   * 入口按钮变为「整理中 x/y（点击停止）」。
   */
  async function classifyAll() {
    const accountId = activeAccountId.value;
    if (!accountId || classifying.value) return;
    classifying.value = true;
    classifyProgress.value = null;

    const channel = new Channel<ClassifyEvent>();
    channel.onmessage = (msg) => {
      if (msg.event === "start") {
        if (msg.data.total > 0) {
          classifyProgress.value = { done: 0, total: msg.data.total };
        } else {
          classifyProgress.value = null;
          toast("没有待整理的邮件", "info");
        }
      } else if (msg.event === "item") {
        if (classifyProgress.value) classifyProgress.value.done += 1;
        // 成功（含快速通道）就地更新标签；失败项保持原样。
        if (!msg.data.error) {
          setTagsLocally(msg.data.messageId, msg.data.tagIds);
          // 出现了未知标签 id（AI 刚创建的新标签）→ 刷新标签库才能渲染出来。
          if (msg.data.tagIds.some((id) => !tags.value.some((t) => t.id === id))) {
            void loadTags();
          }
        }
      } else if (msg.event === "done") {
        classifyProgress.value = null;
        const { cancelled, classified, failed, skipped: byFastlane, remaining } = msg.data;
        const lane = byFastlane > 0 ? `（快速通道 ${byFastlane} 封）` : "";
        if (cancelled) {
          toast(`整理已停止：已整理 ${classified} 封`, "info");
        } else if (failed > 0) {
          toast(
            `整理完成：成功 ${classified} 封${lane}，失败 ${failed} 封，剩余 ${remaining} 封`,
            "info",
          );
        } else {
          toast(
            `整理完成：共 ${classified} 封${lane}${remaining > 0 ? `，剩余 ${remaining} 封` : ""}`,
            "ok",
          );
        }
        // 标签角标 / 组计数 / 筛选中的列表成员都可能变化。
        void loadTags();
        void loadGroups();
        if (filtering.value) void loadMessages();
      } else if (msg.event === "error") {
        classifyProgress.value = null;
        toast(msg.data.message, "error");
      }
    };

    try {
      await api.classifyMessages(accountId, channel);
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      classifying.value = false;
      classifyProgress.value = null;
    }
  }

  /** 请求中止整理（在消息边界生效；已整理的结果保留）。 */
  async function cancelClassify() {
    if (!activeAccountId.value || !classifying.value) return;
    try {
      await api.cancelClassify(activeAccountId.value);
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  /**
   * 为单封邮件执行 AI 打标签（右键菜单「AI 自动标签」）。
   *
   * 后端与批量整理共用同一条推理与写回链路；差异：不受「未整理队列」限制
   * （显式动作，已整理过的邮件也会重新判定），且不走发件人快速通道。
   */
  async function classifyMessage(messageId: number) {
    if (!activeAccountId.value || classifyOneBusy.value || classifying.value) return;
    classifyOneBusy.value = true;
    toast("正在用 AI 为这封邮件打标签…", "info");
    try {
      const tagIds = await api.classifyMessage(messageId);
      setTagsLocally(messageId, tagIds);
      await loadTags();
      void loadGroups();
      const names = tagIds
        .map((id) => tags.value.find((t) => t.id === id)?.name)
        .filter((n): n is string => Boolean(n));
      if (names.length > 0) {
        toast(`AI 已为该邮件打上：${names.join("、")}`, "ok");
      } else {
        toast("AI 判定无需打标签（该维度已有手动标签或没有匹配的标签）", "info");
      }
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      classifyOneBusy.value = false;
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
    // ---- AI ----
    summary,
    summaryLoading,
    summaryError,
    summaryPending,
    aiSettings,
    classifying,
    classifyOneBusy,
    classifyProgress,
    autoClassify,
    loadAiSettings,
    classifyAll,
    cancelClassify,
    classifyMessage,
    setTagsLocally,
    tags,
    groups,
    activeTagIds,
    activeGroupId,
    ui,
    busy,
    toasts,
    activeAccount,
    activeTags,
    activeGroup,
    filtering,
    unreadCount,
    toast,
    loadAccounts,
    selectAccount,
    removeAccount,
    loadMessages,
    clearFilter,
    toggleTagFilter,
    selectGroup,
    addGroupToFilter,
    filterByTags,
    loadTags,
    createTag,
    updateTag,
    deleteTag,
    toggleMessageTag,
    tagsOf,
    loadGroups,
    createGroup,
    updateGroup,
    deleteGroup,
    toggleGroupCollapsed,
    groupsOfTag,
    sync,
    watchStates,
    activeWatch,
    initRealtime,
    // ---- 更新 ----
    updateInfo,
    updateSettings,
    updateChecking,
    loadUpdateSettings,
    checkUpdate,
    skipVersion,
    initUpdater,
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
