<script setup lang="ts">
import { computed, ref } from "vue";

import { useMailStore } from "../stores/mail";
import { avatarColor, avatarText, GROUP_PALETTE, TAG_PALETTE, tagDotStyle } from "../utils/format";

const store = useMailStore();

function onRemove(id: string, email: string) {
  if (confirm(`确定要移除账号 ${email} 吗？\n本地已同步的邮件缓存与全部标签也会一并删除。`)) {
    void store.removeAccount(id);
  }
}

// ------------------------------------------------------------------
// 标签的增 / 改 / 删
// ------------------------------------------------------------------

/** 非 null 表示正在编辑该标签；null 表示「新建」态。 */
const editingId = ref<number | null>(null);
const editorOpen = ref(false);
const draftName = ref("");
const draftColor = ref(TAG_PALETTE[0]);

function openCreate() {
  editingId.value = null;
  draftName.value = "";
  // 按现有标签数轮转取色，新标签自带一个不撞的颜色。
  draftColor.value = TAG_PALETTE[store.tags.length % TAG_PALETTE.length];
  editorOpen.value = true;
}

function openEdit(id: number) {
  const tag = store.tags.find((t) => t.id === id);
  if (!tag) return;
  editingId.value = id;
  draftName.value = tag.name;
  draftColor.value = tag.color;
  editorOpen.value = true;
}

function closeEditor() {
  editorOpen.value = false;
  editingId.value = null;
}

async function submitEditor() {
  const name = draftName.value.trim();
  if (!name) return;
  const ok =
    editingId.value === null
      ? await store.createTag(name, draftColor.value)
      : await store.updateTag(editingId.value, { name, color: draftColor.value });
  if (ok) closeEditor();
}

function onDeleteTag(id: number, name: string) {
  if (confirm(`确定要删除标签「${name}」吗？\n邮件本身不会被删除，只是不再带这个标签。`)) {
    void store.deleteTag(id);
  }
}

// ------------------------------------------------------------------
// 标签组的增 / 改 / 删
//
// 组与标签是多对多：同一个标签可以同时出现在多个组里。因此侧栏里
// 同一标签可能在多个折叠区中各出现一次，这是预期行为而非重复渲染。
// ------------------------------------------------------------------

const groupEditorOpen = ref(false);
/** 非 null 表示正在编辑该组；null 表示「新建」态。 */
const editingGroupId = ref<number | null>(null);
const groupDraftName = ref("");
const groupDraftColor = ref(GROUP_PALETTE[0]);
/** 编辑器里勾选的成员标签 id。 */
const groupDraftTagIds = ref<number[]>([]);
/** 编辑器的「AI 整理」模式："off" / "select" / "create"。 */
const groupDraftAiMode = ref("off");

/** 「AI 整理」三态（值对应后端 `tag_groups.ai_mode`）。 */
const AI_MODE_OPTIONS: { value: string; label: string }[] = [
  { value: "off", label: "不参与" },
  { value: "select", label: "仅选择" },
  { value: "create", label: "可创建" },
];
const AI_MODE_HINTS: Record<string, string> = {
  off: "智能整理不会读写这个组。",
  select: "整理时只能从组内现有标签中选一个。",
  create: "优先从组内选择；识别到新品牌时会自动创建标签并加入本组。",
};

/** 正在编辑的标签是否为 AI 创建（编辑器里给一行说明）。 */
const editingTagCreatedByAi = computed(() => {
  if (editingId.value === null) return false;
  return store.tags.find((t) => t.id === editingId.value)?.createdBy === "ai";
});

function openCreateGroup() {
  editingGroupId.value = null;
  groupDraftName.value = "";
  groupDraftColor.value = GROUP_PALETTE[store.groups.length % GROUP_PALETTE.length];
  groupDraftTagIds.value = [];
  groupDraftAiMode.value = "off";
  groupEditorOpen.value = true;
}

function openEditGroup(id: number) {
  const g = store.groups.find((x) => x.id === id);
  if (!g) return;
  editingGroupId.value = id;
  groupDraftName.value = g.name;
  groupDraftColor.value = g.color;
  groupDraftTagIds.value = [...g.tagIds];
  groupDraftAiMode.value = g.aiMode;
  groupEditorOpen.value = true;
}

function closeGroupEditor() {
  groupEditorOpen.value = false;
  editingGroupId.value = null;
}

function toggleDraftTag(tagId: number) {
  groupDraftTagIds.value = groupDraftTagIds.value.includes(tagId)
    ? groupDraftTagIds.value.filter((id) => id !== tagId)
    : [...groupDraftTagIds.value, tagId];
}

async function submitGroupEditor() {
  const name = groupDraftName.value.trim();
  if (!name) return;
  const tagIds = [...groupDraftTagIds.value];
  const ok =
    editingGroupId.value === null
      ? await store.createGroup(name, groupDraftColor.value, tagIds, groupDraftAiMode.value)
      : await store.updateGroup(editingGroupId.value, {
          name,
          color: groupDraftColor.value,
          tagIds,
          aiMode: groupDraftAiMode.value,
        });
  if (ok) closeGroupEditor();
}

function onDeleteGroup(id: number, name: string) {
  if (
    confirm(
      `确定要删除标签组「${name}」吗？\n组内的标签本身会保留，只是不再归入这个组。`,
    )
  ) {
    void store.deleteGroup(id);
  }
}

/** 「未分组」区：统计不属于任何组的标签，方便判断有没有漏归类的。 */
const ungroupedTags = computed(() => {
  const grouped = new Set(store.groups.flatMap((g) => g.tagIds));
  return store.tags.filter((t) => !grouped.has(t.id));
});
</script>

<template>
  <aside class="flex w-[244px] shrink-0 flex-col border-r border-line bg-panel">
    <!-- 品牌 -->
    <div class="flex h-14 shrink-0 items-center gap-2.5 px-4">
      <div
        class="flex h-7 w-7 items-center justify-center rounded-lg bg-gradient-to-br from-indigo-500 to-violet-600 text-[13px] font-bold text-white shadow-lg shadow-indigo-900/40"
      >
        P
      </div>
      <span class="text-[15px] font-semibold tracking-tight text-zinc-100">Pony Mail</span>
      <span class="ml-auto text-[10px] font-medium text-zinc-600">v0.1</span>
    </div>

    <!-- 写信按钮 -->
    <div class="shrink-0 px-3 pb-3">
      <button
        class="flex w-full items-center justify-center gap-2 rounded-lg bg-accent px-3 py-2 text-[13px] font-medium text-white transition hover:bg-indigo-500 active:scale-[0.98]"
        @click="store.openCompose()"
      >
        <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M12 5v14M5 12h14" stroke-linecap="round" />
        </svg>
        写邮件
      </button>
    </div>

    <!-- 账号列表 -->
    <div class="min-h-0 flex-1 overflow-y-auto px-3 pb-3">
      <p class="px-1 pb-2 text-[11px] font-medium uppercase tracking-wider text-zinc-600">
        账号
      </p>

      <div
        v-if="store.accounts.length === 0 && !store.busy.accounts"
        class="rounded-lg border border-dashed border-line px-3 py-4 text-center text-[12px] leading-relaxed text-zinc-500"
      >
        还没有邮箱账号<br />点击下方「添加账号」开始
      </div>

      <div v-for="a in store.accounts" :key="a.id" class="group/acc mb-1">
        <div
          class="flex items-center gap-2 rounded-lg px-2 py-2 transition"
          :class="
            a.id === store.activeAccountId
              ? 'bg-hover'
              : 'hover:bg-panel-2'
          "
        >
          <button class="flex min-w-0 flex-1 items-center gap-2 text-left" @click="store.selectAccount(a.id)">
            <span
              class="flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-[11px] font-bold text-white"
              :style="{ background: avatarColor(a.id) }"
            >
              {{ avatarText(a.displayName, a.email) }}
            </span>
            <span class="min-w-0 flex-1">
              <span class="block truncate text-[13px] font-medium text-zinc-200">
                {{ a.displayName || a.email }}
              </span>
              <span class="block truncate text-[11px] text-zinc-500">{{ a.email }}</span>
            </span>
          </button>
          <button
            class="shrink-0 rounded p-1 text-zinc-600 opacity-0 transition hover:bg-rose-500/10 hover:text-rose-400 group-hover/acc:opacity-100"
            title="移除账号"
            @click="onRemove(a.id, a.email)"
          >
            <svg class="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M6 6l12 12M18 6L6 18" stroke-linecap="round" />
            </svg>
          </button>
        </div>

        <!-- 标签组 / 标签 -->
        <div v-if="a.id === store.activeAccountId" class="mt-2 space-y-0.5 pl-3">
          <div class="flex items-center gap-1 px-2.5 py-1">
            <span class="text-[11px] font-medium uppercase tracking-wider text-zinc-600">标签</span>
            <button
              class="ml-auto rounded p-1 text-zinc-500 transition hover:bg-panel-2 hover:text-fuchsia-300"
              title="新建标签组"
              @click="openCreateGroup"
            >
              <svg class="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M3 7a1 1 0 0 1 1-1h4l2 2h7a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V7z" stroke-linejoin="round" />
                <path d="M12 11v5M9.5 13.5h5" stroke-linecap="round" />
              </svg>
            </button>
            <button
              class="rounded p-1 text-zinc-500 transition hover:bg-panel-2 hover:text-indigo-300"
              title="新建标签"
              @click="openCreate"
            >
              <svg class="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M12 5v14M5 12h14" stroke-linecap="round" />
              </svg>
            </button>
          </div>

          <!-- 「全部邮件」：清空筛选集合 -->
          <button
            class="flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-[13px] transition"
            :class="
              !store.filtering
                ? 'bg-hover text-zinc-100'
                : 'text-zinc-400 hover:bg-panel-2 hover:text-zinc-200'
            "
            @click="store.clearFilter()"
          >
            <svg class="h-3.5 w-3.5 shrink-0 text-zinc-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M4 6h16M4 12h16M4 18h10" stroke-linecap="round" />
            </svg>
            <span class="truncate">全部邮件</span>
            <span
              v-if="store.filtering"
              class="ml-auto rounded-full bg-accent/20 px-1.5 py-0.5 text-[10px] font-semibold text-indigo-300"
            >
              筛选中 {{ store.activeTagIds.length }}
            </span>
          </button>

          <p
            v-if="store.tags.length === 0 && store.groups.length === 0"
            class="px-2.5 py-1 text-[11.5px] leading-relaxed text-zinc-600"
          >
            还没有标签，点 + 新建
          </p>

          <!-- 标签组：可折叠，组名点击即筛选本组全部标签 -->
          <!-- `relative` 是必须的：行内的编辑/删除按钮用 absolute right-1.5 定位，
               缺少它就会锚到更外层的定位祖先上，跑到列表外面去。 -->
          <div v-for="g in store.groups" :key="`g${g.id}`" class="group/grp relative">
            <div
              class="flex items-center gap-1 rounded-md px-1.5 py-1 transition"
              :class="
                store.activeGroupId === g.id
                  ? 'bg-hover'
                  : 'hover:bg-panel-2'
              "
            >
              <!-- 折叠开关：只切换展开状态，不触发筛选 -->
              <button
                class="shrink-0 rounded p-0.5 text-zinc-500 transition hover:text-zinc-200"
                :title="g.collapsed ? '展开' : '收起'"
                @click.stop="store.toggleGroupCollapsed(g.id)"
              >
                <svg
                  class="h-3 w-3 transition"
                  :class="g.collapsed ? '' : 'rotate-90'"
                  viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"
                >
                  <path d="M9 6l6 6-6 6" stroke-linecap="round" stroke-linejoin="round" />
                </svg>
              </button>

              <!-- 组名：点击筛选本组（再次点击取消） -->
              <button
                class="flex min-w-0 flex-1 items-center gap-1.5 text-left text-[12.5px]"
                :title="`筛选「${g.name}」组内 ${g.tagIds.length} 个标签`"
                @click="store.selectGroup(g.id)"
              >
                <svg
                  class="h-3.5 w-3.5 shrink-0"
                  :style="{ color: g.color }"
                  viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
                >
                  <path d="M3 7a1 1 0 0 1 1-1h4l2 2h7a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V7z" stroke-linejoin="round" />
                </svg>
                <span
                  class="min-w-0 flex-1 truncate"
                  :class="store.activeGroupId === g.id ? 'font-medium text-zinc-100' : 'text-zinc-300'"
                >
                  {{ g.name }}
                </span>
                <span
                  v-if="g.aiMode !== 'off'"
                  class="shrink-0 rounded bg-indigo-500/15 px-1 py-px text-[9px] font-semibold text-indigo-300"
                  :title="
                    g.aiMode === 'create'
                      ? 'AI 整理维度：从组内选择，也可自动创建新标签'
                      : 'AI 整理维度：只从组内现有标签中选择'
                  "
                >
                  AI
                </span>
                <span class="shrink-0 text-[10px] text-zinc-600">{{ g.tagIds.length }}</span>
              </button>

              <span
                v-if="g.messageCount > 0"
                class="shrink-0 rounded-full bg-panel-2 px-1.5 py-0.5 text-[10px] font-semibold text-zinc-400 group-hover/grp:opacity-0"
              >
                {{ g.messageCount }}
              </span>

              <span class="absolute right-1.5 flex shrink-0 items-center gap-0.5 opacity-0 transition group-hover/grp:opacity-100">
                <span
                  class="rounded p-1 text-zinc-500 transition hover:bg-panel hover:text-zinc-200"
                  title="编辑组"
                  @click.stop="openEditGroup(g.id)"
                >
                  <svg class="h-3 w-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                    <path d="M4 20h4l10-10-4-4L4 16v4zM14 6l4 4" stroke-linecap="round" stroke-linejoin="round" />
                  </svg>
                </span>
                <span
                  class="rounded p-1 text-zinc-500 transition hover:bg-rose-500/10 hover:text-rose-400"
                  title="删除组"
                  @click.stop="onDeleteGroup(g.id, g.name)"
                >
                  <svg class="h-3 w-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                    <path d="M6 6l12 12M18 6L6 18" stroke-linecap="round" />
                  </svg>
                </span>
              </span>
            </div>

            <!-- 组内标签：多选勾选，OR 语义 -->
            <div v-if="!g.collapsed" class="ml-3 space-y-0.5 border-l border-line/70 pl-2">
              <p
                v-if="g.tagIds.length === 0"
                class="px-2 py-1 text-[11px] text-zinc-600"
              >
                组内暂无标签，点编辑添加
              </p>
              <div
                v-for="t in g.tagIds.map((id) => store.tags.find((x) => x.id === id)).filter(Boolean)"
                :key="`gt${g.id}-${t!.id}`"
                class="group/tag relative"
              >
                <button
                  class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-[12.5px] transition"
                  :class="
                    store.activeTagIds.includes(t!.id)
                      ? 'bg-hover text-zinc-100'
                      : 'text-zinc-400 hover:bg-panel-2 hover:text-zinc-200'
                  "
                  @click="store.toggleTagFilter(t!.id)"
                >
                  <!-- 彩色圆点：常态以标签本色低透明度示人，选中时发出同色辉光 -->
                  <span
                    class="h-3 w-3 shrink-0 rounded-full transition"
                    :style="tagDotStyle(t!.color, store.activeTagIds.includes(t!.id))"
                  />
                  <span class="min-w-0 flex-1 truncate text-left">{{ t!.name }}</span>
                  <span
                    v-if="t!.messageCount > 0"
                    class="shrink-0 rounded-full bg-panel-2 px-1.5 py-0.5 text-[10px] font-semibold text-zinc-400 group-hover/tag:opacity-0"
                  >
                    {{ t!.messageCount }}
                  </span>
                  <span class="absolute right-1.5 flex shrink-0 items-center gap-0.5 opacity-0 transition group-hover/tag:opacity-100">
                    <span
                      class="rounded p-1 text-zinc-500 transition hover:bg-panel hover:text-zinc-200"
                      title="编辑标签"
                      @click.stop="openEdit(t!.id)"
                    >
                      <svg class="h-3 w-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M4 20h4l10-10-4-4L4 16v4zM14 6l4 4" stroke-linecap="round" stroke-linejoin="round" />
                      </svg>
                    </span>
                    <span
                      class="rounded p-1 text-zinc-500 transition hover:bg-rose-500/10 hover:text-rose-400"
                      title="删除标签"
                      @click.stop="onDeleteTag(t!.id, t!.name)"
                    >
                      <svg class="h-3 w-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M6 6l12 12M18 6L6 18" stroke-linecap="round" />
                      </svg>
                    </span>
                  </span>
                </button>
              </div>
            </div>
          </div>

          <!-- 未归组标签：多对多下需要单独兜住没进任何组的标签 -->
          <div v-if="ungroupedTags.length" class="pt-1">
            <p class="px-2.5 py-1 text-[10.5px] font-medium uppercase tracking-wider text-zinc-600">
              未分组
            </p>
            <div
              v-for="t in ungroupedTags"
              :key="`u${t.id}`"
              class="group/tag relative"
            >
              <button
                class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-[12.5px] transition"
                :class="
                  store.activeTagIds.includes(t.id)
                    ? 'bg-hover text-zinc-100'
                    : 'text-zinc-400 hover:bg-panel-2 hover:text-zinc-200'
                "
                @click="store.toggleTagFilter(t.id)"
              >
                <!-- 彩色圆点：同组内标签一致的选中语言 -->
                <span
                  class="h-3 w-3 shrink-0 rounded-full transition"
                  :style="tagDotStyle(t.color, store.activeTagIds.includes(t.id))"
                />
                <span class="min-w-0 flex-1 truncate text-left">{{ t.name }}</span>
                <span
                  v-if="t.messageCount > 0"
                  class="shrink-0 rounded-full bg-panel-2 px-1.5 py-0.5 text-[10px] font-semibold text-zinc-400 group-hover/tag:opacity-0"
                >
                  {{ t.messageCount }}
                </span>
                <span class="absolute right-1.5 flex shrink-0 items-center gap-0.5 opacity-0 transition group-hover/tag:opacity-100">
                  <span
                    class="rounded p-1 text-zinc-500 transition hover:bg-panel hover:text-zinc-200"
                    title="编辑标签"
                    @click.stop="openEdit(t.id)"
                  >
                    <svg class="h-3 w-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                      <path d="M4 20h4l10-10-4-4L4 16v4zM14 6l4 4" stroke-linecap="round" stroke-linejoin="round" />
                    </svg>
                  </span>
                  <span
                    class="rounded p-1 text-zinc-500 transition hover:bg-rose-500/10 hover:text-rose-400"
                    title="删除标签"
                    @click.stop="onDeleteTag(t.id, t.name)"
                  >
                    <svg class="h-3 w-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                      <path d="M6 6l12 12M18 6L6 18" stroke-linecap="round" />
                    </svg>
                  </span>
                </span>
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 底部操作 -->
    <div class="shrink-0 space-y-0.5 border-t border-line p-3">
      <button
        class="flex w-full items-center gap-2 rounded-md px-2.5 py-2 text-[13px] text-zinc-400 transition hover:bg-panel-2 hover:text-zinc-200"
        @click="store.ui.accountOpen = true"
      >
        <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M12 5v14M5 12h14" stroke-linecap="round" />
        </svg>
        添加账号
      </button>
      <button
        class="flex w-full items-center gap-2 rounded-md px-2.5 py-2 text-[13px] text-zinc-400 transition hover:bg-panel-2 hover:text-zinc-200"
        @click="store.ui.settingsOpen = true"
      >
        <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <circle cx="12" cy="12" r="3" />
          <path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M19.1 4.9L17 7M7 17l-2.1 2.1" stroke-linecap="round" />
        </svg>
        设置
      </button>
    </div>
  </aside>

  <!-- 标签编辑器 -->
  <Teleport to="body">
    <div
      v-if="editorOpen"
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-6 backdrop-blur-sm"
      @click.self="closeEditor"
    >
      <div class="w-full max-w-xs overflow-hidden rounded-2xl border border-line bg-panel shadow-2xl">
        <div class="border-b border-line px-5 py-3.5">
          <h2 class="text-[13.5px] font-semibold text-zinc-100">
            {{ editingId === null ? "新建标签" : "编辑标签" }}
          </h2>
        </div>

        <div class="space-y-4 px-5 py-4">
          <label class="block">
            <span class="mb-1 block text-[12px] text-zinc-400">名称</span>
            <input
              v-model="draftName"
              class="w-full rounded-lg border border-line bg-panel-2 px-3 py-2 text-[13px] text-zinc-200 outline-none transition focus:border-accent/60"
              placeholder="例如：待办、账单、项目名"
              maxlength="24"
              autofocus
              @keydown.enter="submitEditor"
              @keydown.esc="closeEditor"
            />
          </label>

          <p
            v-if="editingTagCreatedByAi"
            class="rounded-lg bg-indigo-500/10 px-3 py-2 text-[11px] leading-relaxed text-indigo-300/90"
          >
            此标签由智能整理创建；你可以直接改名、改色或删除它。
          </p>

          <div>
            <span class="mb-1.5 block text-[12px] text-zinc-400">颜色</span>
            <div class="flex flex-wrap gap-2">
              <button
                v-for="c in TAG_PALETTE"
                :key="c"
                class="h-6 w-6 rounded-full transition"
                :class="c === draftColor ? 'ring-2 ring-white/70 ring-offset-2 ring-offset-panel' : 'hover:scale-110'"
                :style="{ background: c }"
                :title="c"
                @click="draftColor = c"
              />
            </div>
          </div>
        </div>

        <div class="flex items-center gap-3 border-t border-line px-5 py-3.5">
          <button
            class="rounded-lg bg-accent px-4 py-2 text-[12.5px] font-medium text-white transition hover:bg-indigo-500 disabled:opacity-40"
            :disabled="!draftName.trim()"
            @click="submitEditor"
          >
            {{ editingId === null ? "创建" : "保存" }}
          </button>
          <button
            class="ml-auto rounded-lg px-3 py-2 text-[12.5px] text-zinc-400 transition hover:bg-panel-2 hover:text-zinc-200"
            @click="closeEditor"
          >
            取消
          </button>
        </div>
      </div>
    </div>
  </Teleport>

  <!-- 标签组编辑器 -->
  <Teleport to="body">
    <div
      v-if="groupEditorOpen"
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-6 backdrop-blur-sm"
      @click.self="closeGroupEditor"
    >
      <div class="flex max-h-full w-full max-w-sm flex-col overflow-hidden rounded-2xl border border-line bg-panel shadow-2xl">
        <div class="shrink-0 border-b border-line px-5 py-3.5">
          <h2 class="text-[13.5px] font-semibold text-zinc-100">
            {{ editingGroupId === null ? "新建标签组" : "编辑标签组" }}
          </h2>
          <p class="mt-0.5 text-[11.5px] text-zinc-500">
            组是标签的合集，点击组名可一次筛选组内全部标签
          </p>
        </div>

        <div class="min-h-0 flex-1 space-y-4 overflow-y-auto px-5 py-4">
          <label class="block">
            <span class="mb-1 block text-[12px] text-zinc-400">组名</span>
            <input
              v-model="groupDraftName"
              class="w-full rounded-lg border border-line bg-panel-2 px-3 py-2 text-[13px] text-zinc-200 outline-none transition focus:border-accent/60"
              placeholder="例如：本周关注、客户、财务"
              maxlength="24"
              autofocus
              @keydown.enter="submitGroupEditor"
              @keydown.esc="closeGroupEditor"
            />
          </label>

          <div>
            <span class="mb-1.5 block text-[12px] text-zinc-400">颜色</span>
            <div class="flex flex-wrap gap-2">
              <button
                v-for="c in GROUP_PALETTE"
                :key="c"
                class="h-6 w-6 rounded-full transition"
                :class="c === groupDraftColor ? 'ring-2 ring-white/70 ring-offset-2 ring-offset-panel' : 'hover:scale-110'"
                :style="{ background: c }"
                :title="c"
                @click="groupDraftColor = c"
              />
            </div>
          </div>

          <div>
            <div class="mb-1.5 flex items-baseline gap-2">
              <span class="text-[12px] text-zinc-400">AI 整理</span>
              <span class="text-[10.5px] text-zinc-600">开启后此组作为一个整理维度</span>
            </div>
            <div class="flex rounded-lg border border-line bg-panel-2 p-0.5">
              <button
                v-for="opt in AI_MODE_OPTIONS"
                :key="opt.value"
                class="flex-1 rounded-md px-2 py-1.5 text-[11.5px] transition"
                :class="
                  groupDraftAiMode === opt.value
                    ? 'bg-accent/20 font-medium text-indigo-300'
                    : 'text-zinc-500 hover:text-zinc-300'
                "
                @click="groupDraftAiMode = opt.value"
              >
                {{ opt.label }}
              </button>
            </div>
            <p class="mt-1.5 text-[10.5px] leading-relaxed text-zinc-600">
              {{ AI_MODE_HINTS[groupDraftAiMode] }}
            </p>
          </div>

          <div>
            <div class="mb-1.5 flex items-baseline gap-2">
              <span class="text-[12px] text-zinc-400">组内标签</span>
              <span class="text-[11px] text-zinc-600">
                已选 {{ groupDraftTagIds.length }}
              </span>
              <span class="ml-auto text-[10.5px] text-zinc-600">同一标签可属于多个组</span>
            </div>

            <p v-if="store.tags.length === 0" class="rounded-lg border border-dashed border-line px-3 py-3 text-center text-[12px] text-zinc-500">
              还没有标签，请先在上方 + 里新建标签
            </p>

            <div v-else class="space-y-0.5">
              <button
                v-for="t in store.tags"
                :key="`pick${t.id}`"
                class="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-left text-[12.5px] transition hover:bg-panel-2"
                @click="toggleDraftTag(t.id)"
              >
                <!-- 圆点即勾选态：原「复选框 + 色点」两件套合并为一个发光圆点 -->
                <span
                  class="h-3 w-3 shrink-0 rounded-full transition"
                  :style="tagDotStyle(t.color, groupDraftTagIds.includes(t.id))"
                />
                <span class="min-w-0 flex-1 truncate text-zinc-300">{{ t.name }}</span>
                <span
                  v-if="store.groupsOfTag(t.id).length"
                  class="shrink-0 text-[10px] text-zinc-600"
                  :title="`已在 ${store.groupsOfTag(t.id).map((x) => x.name).join('、')} 组中`"
                >
                  {{ store.groupsOfTag(t.id).length }} 组
                </span>
              </button>
            </div>
          </div>
        </div>

        <div class="flex shrink-0 items-center gap-3 border-t border-line px-5 py-3.5">
          <button
            class="rounded-lg bg-accent px-4 py-2 text-[12.5px] font-medium text-white transition hover:bg-indigo-500 disabled:opacity-40"
            :disabled="!groupDraftName.trim()"
            @click="submitGroupEditor"
          >
            {{ editingGroupId === null ? "创建" : "保存" }}
          </button>
          <button
            class="ml-auto rounded-lg px-3 py-2 text-[12.5px] text-zinc-400 transition hover:bg-panel-2 hover:text-zinc-200"
            @click="closeGroupEditor"
          >
            取消
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>
