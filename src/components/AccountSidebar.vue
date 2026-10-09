<script setup lang="ts">
import { useMailStore } from "../stores/mail";
import { avatarColor, avatarText } from "../utils/format";

const store = useMailStore();

function onRemove(id: string, email: string) {
  if (confirm(`确定要移除账号 ${email} 吗？\n本地已同步的邮件缓存也会一并删除。`)) {
    void store.removeAccount(id);
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

        <!-- 文件夹 -->
        <div v-if="a.id === store.activeAccountId" class="mt-0.5 space-y-0.5 pl-3">
          <div
            class="flex items-center gap-2 rounded-md px-2.5 py-1.5 text-[13px] text-zinc-300"
          >
            <svg class="h-3.5 w-3.5 text-zinc-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M3 7h18v12H3zM3 7l2-3h5l2 3" stroke-linejoin="round" />
            </svg>
            <span>收件箱</span>
            <span
              v-if="store.unreadCount > 0"
              class="ml-auto rounded-full bg-accent/20 px-1.5 py-0.5 text-[10px] font-semibold text-indigo-300"
            >
              {{ store.unreadCount }}
            </span>
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
</template>
