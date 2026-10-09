<script setup lang="ts">
import { onBeforeUnmount, onMounted, reactive } from "vue";

import { useMailStore } from "../stores/mail";
import { avatarColor, avatarText, formatDate } from "../utils/format";

const store = useMailStore();

// ------------------------------------------------------------------
// 右键菜单
// ------------------------------------------------------------------

/**
 * 菜单状态。`visible` 控制显隐，`x/y` 是相对视口的固定定位坐标。
 * 默认隐藏，右键某封邮件时定位到光标处。
 */
const menu = reactive({
  visible: false,
  x: 0,
  y: 0,
  messageId: null as number | null,
});

/** 打开右键菜单：把光标位置夹回视口内，避免菜单溢出屏幕。 */
function openMenu(e: MouseEvent, id: number) {
  e.preventDefault();
  menu.messageId = id;
  // 菜单约 172×140，留 8px 边距做安全区。
  const w = 176;
  const h = 148;
  menu.x = Math.min(e.clientX, window.innerWidth - w - 8);
  menu.y = Math.min(e.clientY, window.innerHeight - h - 8);
  menu.visible = true;
}

function closeMenu() {
  menu.visible = false;
  menu.messageId = null;
}

/** 菜单项是否作用于「当前正打开的邮件」，用于决定删除后要不要清空阅读区。 */
function isFlagged(id: number): boolean {
  return store.messages.find((m) => m.id === id)?.flagged ?? false;
}

function isSeen(id: number): boolean {
  return store.messages.find((m) => m.id === id)?.seen ?? false;
}

function onToggleFlag() {
  const id = menu.messageId;
  closeMenu();
  if (id !== null) void store.toggleFlag(id);
}

function onToggleSeen() {
  const id = menu.messageId;
  const seen = id !== null ? isSeen(id) : false;
  closeMenu();
  if (id !== null) void store.setSeen(id, !seen);
}

function onOpen() {
  const id = menu.messageId;
  closeMenu();
  if (id !== null) void store.openMessage(id);
}

function onDelete() {
  const id = menu.messageId;
  closeMenu();
  if (id === null) return;
  if (
    confirm(
      "确定要永久删除这封邮件吗？\n该操作会在服务器上同步删除，且无法撤销。",
    )
  ) {
    void store.deleteMessage(id);
  }
}

// 滚动 / 缩放 / 按 Esc 都应关闭菜单，否则菜单会「漂」在错误位置。
function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape") closeMenu();
}

onMounted(() => {
  // 注意：右键行本身会冒泡到 window，这里必须区分「刚被本组件打开的菜单」。
  // 做法是让行上的 openMenu 先执行（冒泡阶段 window 后执行），
  // 所以 window 上的 contextmenu 监听只用于「关掉上一次的菜单」，
  // 而 openMenu 里已经置 visible=true，不会被误关。
  window.addEventListener("click", closeMenu);
  window.addEventListener("resize", closeMenu);
  window.addEventListener("keydown", onKeydown);
});

onBeforeUnmount(() => {
  window.removeEventListener("click", closeMenu);
  window.removeEventListener("resize", closeMenu);
  window.removeEventListener("keydown", onKeydown);
});
</script>

<template>
  <section class="flex w-[380px] shrink-0 flex-col border-r border-line bg-panel">
    <!-- 工具条 -->
    <div class="flex h-14 shrink-0 items-center gap-3 border-b border-line px-4">
      <div class="min-w-0 flex-1">
        <p class="truncate text-[13px] font-medium text-zinc-200">
          {{ store.activeAccount?.email ?? "未选择账号" }}
        </p>
        <p class="truncate text-[11px] text-zinc-500">
          收件箱 · {{ store.messages.length }} 封
          <template v-if="store.lastSync"> · 上次同步 {{ store.lastSync }}</template>
        </p>
      </div>
      <button
        class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-line text-zinc-400 transition hover:border-accent/50 hover:text-indigo-300 disabled:opacity-40"
        :disabled="store.busy.sync || !store.activeAccountId"
        title="同步收件箱"
        @click="store.sync()"
      >
        <svg
          class="h-4 w-4"
          :class="store.busy.sync ? 'animate-spin' : ''"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <path d="M21 12a9 9 0 1 1-3-6.7L21 8" stroke-linecap="round" stroke-linejoin="round" />
          <path d="M21 3v5h-5" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>
    </div>

    <!-- 列表 -->
    <div class="min-h-0 flex-1 overflow-y-auto">
      <div v-if="store.busy.messages" class="px-6 py-10 text-center text-[12px] text-zinc-500">
        正在加载本地缓存…
      </div>

      <div
        v-else-if="store.messages.length === 0"
        class="px-8 py-12 text-center text-[12px] leading-relaxed text-zinc-600"
      >
        <svg
          class="mx-auto mb-3 h-8 w-8 text-zinc-700"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
        >
          <rect x="3" y="5" width="18" height="14" rx="2" />
          <path d="M3 7l9 6 9-6" stroke-linecap="round" />
        </svg>
        {{ store.activeAccountId ? "本地还没有邮件，点击右上角同步" : "请先在左侧添加邮箱账号" }}
      </div>

      <button
        v-for="m in store.messages"
        :key="m.id"
        class="group/row flex w-full gap-3 border-b border-line/60 px-4 py-3 text-left transition"
        :class="
          m.id === store.activeMessageId
            ? 'bg-hover'
            : 'hover:bg-panel-2'
        "
        @click="store.openMessage(m.id)"
        @contextmenu="openMenu($event, m.id)"
      >
        <span
          class="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-[12px] font-semibold text-white"
          :style="{ background: avatarColor(m.fromAddr || m.fromName) }"
        >
          {{ avatarText(m.fromName, m.fromAddr) }}
        </span>

        <span class="min-w-0 flex-1">
          <span class="flex items-baseline gap-2">
            <span
              class="min-w-0 flex-1 truncate text-[13px]"
              :class="m.seen ? 'text-zinc-300' : 'font-semibold text-zinc-100'"
            >
              {{ m.fromName || m.fromAddr || "未知发件人" }}
            </span>
            <span class="shrink-0 text-[11px] text-zinc-500">{{ formatDate(m.dateTs) }}</span>
          </span>

          <span class="mt-0.5 flex items-center gap-1.5">
            <span
              v-if="!m.seen"
              class="h-1.5 w-1.5 shrink-0 rounded-full bg-indigo-400"
            />
            <span
              class="min-w-0 flex-1 truncate text-[12.5px]"
              :class="m.seen ? 'text-zinc-400' : 'font-medium text-zinc-200'"
            >
              {{ m.subject }}
            </span>
            <svg
              v-if="m.hasAttachments"
              class="h-3 w-3 shrink-0 text-zinc-500"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
            >
              <path d="M21 11l-9 9a5 5 0 0 1-7-7l9-9a3.5 3.5 0 0 1 5 5l-9 9a2 2 0 0 1-3-3l8-8" stroke-linecap="round" stroke-linejoin="round" />
            </svg>
          </span>

          <span class="mt-1 block truncate text-[11.5px] text-zinc-500">
            {{ m.snippet || "（正文未加载）" }}
          </span>
        </span>

        <span
          class="shrink-0 pt-0.5 transition"
          :class="m.flagged ? 'text-amber-400' : 'text-zinc-700 opacity-0 group-hover/row:opacity-100'"
          title="标记星标"
          @click.stop="store.toggleFlag(m.id)"
        >
          <svg class="h-4 w-4" viewBox="0 0 24 24" :fill="m.flagged ? 'currentColor' : 'none'" stroke="currentColor" stroke-width="1.8">
            <path d="M12 3.5l2.6 5.3 5.9.9-4.2 4.1 1 5.8-5.3-2.8-5.3 2.8 1-5.8L3.5 9.7l5.9-.9z" stroke-linejoin="round" />
          </svg>
        </span>
      </button>
    </div>

    <!-- 右键菜单 -->
    <Teleport to="body">
      <div
        v-if="menu.visible"
        class="fixed z-50 w-44 overflow-hidden rounded-lg border border-line bg-panel py-1 shadow-2xl shadow-black/50"
        :style="{ left: `${menu.x}px`, top: `${menu.y}px` }"
        @click.stop
        @contextmenu.stop.prevent
      >
        <button
          class="flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-[12.5px] text-zinc-300 transition hover:bg-hover"
          @click="onOpen"
        >
          <svg class="h-3.5 w-3.5 shrink-0 text-zinc-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
            <path d="M4 5h11l5 5v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1z" stroke-linejoin="round" />
          </svg>
          打开邮件
        </button>

        <button
          class="flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-[12.5px] text-zinc-300 transition hover:bg-hover"
          @click="onToggleSeen"
        >
          <svg class="h-3.5 w-3.5 shrink-0 text-zinc-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
            <circle cx="12" cy="12" r="3.2" />
            <path d="M2.5 12S6 5.8 12 5.8 21.5 12 21.5 12 18 18.2 12 18.2 2.5 12 2.5 12z" stroke-linejoin="round" />
          </svg>
          {{ menu.messageId !== null && isSeen(menu.messageId) ? "标记为未读" : "标记为已读" }}
        </button>

        <button
          class="flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-[12.5px] text-zinc-300 transition hover:bg-hover"
          @click="onToggleFlag"
        >
          <svg
            class="h-3.5 w-3.5 shrink-0"
            :class="menu.messageId !== null && isFlagged(menu.messageId) ? 'text-amber-400' : 'text-zinc-500'"
            viewBox="0 0 24 24"
            :fill="menu.messageId !== null && isFlagged(menu.messageId) ? 'currentColor' : 'none'"
            stroke="currentColor"
            stroke-width="1.8"
          >
            <path d="M12 3.5l2.6 5.3 5.9.9-4.2 4.1 1 5.8-5.3-2.8-5.3 2.8 1-5.8L3.5 9.7l5.9-.9z" stroke-linejoin="round" />
          </svg>
          {{ menu.messageId !== null && isFlagged(menu.messageId) ? "取消星标" : "标记星标" }}
        </button>

        <div class="my-1 h-px bg-line" />

        <button
          class="flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-[12.5px] text-rose-400 transition hover:bg-rose-500/15"
          @click="onDelete"
        >
          <svg class="h-3.5 w-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
            <path d="M4 7h16M9.5 7V5.5a1 1 0 0 1 1-1h3a1 1 0 0 1 1 1V7M6.5 7l1 12a1 1 0 0 0 1 1h7a1 1 0 0 0 1-1l1-12" stroke-linecap="round" stroke-linejoin="round" />
          </svg>
          删除邮件
        </button>
      </div>
    </Teleport>
  </section>
</template>
