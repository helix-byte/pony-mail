<script setup lang="ts">
import { ref } from "vue";

import { useMailStore } from "../stores/mail";
import { avatarColor, avatarText, TAG_PALETTE } from "../utils/format";

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

        <!-- 标签 -->
        <div v-if="a.id === store.activeAccountId" class="mt-2 space-y-0.5 pl-3">
          <div class="flex items-center gap-1 px-2.5 py-1">
            <span class="text-[11px] font-medium uppercase tracking-wider text-zinc-600">标签</span>
            <button
              class="ml-auto rounded p-1 text-zinc-500 transition hover:bg-panel-2 hover:text-indigo-300"
              title="新建标签"
              @click="openCreate"
            >
              <svg class="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M12 5v14M5 12h14" stroke-linecap="round" />
              </svg>
            </button>
          </div>

          <!-- 「全部」永远在最上面，用于退出标签筛选 -->
          <button
            class="flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-[13px] transition"
            :class="
              store.activeTagId === null
                ? 'bg-hover text-zinc-100'
                : 'text-zinc-400 hover:bg-panel-2 hover:text-zinc-200'
            "
            @click="store.selectTag(null)"
          >
            <svg class="h-3.5 w-3.5 shrink-0 text-zinc-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M4 6h16M4 12h16M4 18h10" stroke-linecap="round" />
            </svg>
            <span class="truncate">全部邮件</span>
          </button>

          <p
            v-if="store.tags.length === 0"
            class="px-2.5 py-1 text-[11.5px] text-zinc-600"
          >
            还没有标签，点 + 新建
          </p>

          <div v-for="t in store.tags" :key="t.id" class="group/tag relative">
            <button
              class="flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-[13px] transition"
              :class="
                store.activeTagId === t.id
                  ? 'bg-hover text-zinc-100'
                  : 'text-zinc-400 hover:bg-panel-2 hover:text-zinc-200'
              "
              @click="store.selectTag(t.id)"
            >
              <span
                class="h-2.5 w-2.5 shrink-0 rounded-full"
                :style="{ background: t.color }"
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
                  title="重命名 / 改色"
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
        AI 设置
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
</template>
