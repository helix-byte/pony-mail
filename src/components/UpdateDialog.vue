<script setup lang="ts">
import { openUrl } from "@tauri-apps/plugin-opener";
import { computed } from "vue";

import { useMailStore } from "../stores/mail";

const store = useMailStore();

/** 主按钮指向「来源平台」（较新版本的所在平台），另一平台作为备选入口。 */
const primary = computed(() => {
  const i = store.updateInfo;
  if (!i) return null;
  const isGitee = i.source !== "github";
  const url = isGitee ? (i.giteeUrl ?? i.githubUrl) : (i.githubUrl ?? i.giteeUrl);
  return { label: `前往下载（${isGitee ? "Gitee" : "GitHub"}）`, url };
});

const secondary = computed(() => {
  const i = store.updateInfo;
  if (!i) return null;
  const isGitee = i.source !== "github";
  const url = isGitee ? i.githubUrl : i.giteeUrl;
  if (!url) return null;
  return { label: isGitee ? "GitHub 下载" : "Gitee 下载", url };
});

async function openDownload(url: string | null) {
  if (!url) {
    store.toast("下载链接缺失，请到仓库 Releases 页面手动下载", "error");
    return;
  }
  try {
    await openUrl(url);
    store.ui.updateOpen = false;
  } catch {
    store.toast("打开下载页失败，请检查系统默认浏览器", "error");
  }
}

async function skip() {
  const latest = store.updateInfo?.latest;
  if (latest) await store.skipVersion(latest);
  store.ui.updateOpen = false;
}
</script>

<template>
  <div
    v-if="store.updateInfo"
    class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-6 backdrop-blur-sm"
  >
    <div class="flex max-h-full w-full max-w-lg flex-col overflow-hidden rounded-2xl border border-line bg-panel shadow-2xl">
      <div class="flex shrink-0 items-center border-b border-line px-5 py-3.5">
        <h2 class="text-[14px] font-semibold text-zinc-100">发现新版本</h2>
        <span class="ml-3 text-[11.5px] text-zinc-500">
          v{{ store.updateInfo.current }} → v{{ store.updateInfo.latest }}
        </span>
        <button
          class="ml-auto rounded-lg p-1.5 text-zinc-500 transition hover:bg-panel-2 hover:text-zinc-300"
          @click="store.ui.updateOpen = false"
        >
          <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M6 6l12 12M18 6L6 18" stroke-linecap="round" />
          </svg>
        </button>
      </div>

      <div class="min-h-0 flex-1 space-y-4 overflow-y-auto px-5 py-4">
        <p class="text-[12.5px] leading-relaxed text-zinc-400">
          Pony Mail v{{ store.updateInfo.latest }} 已发布。下载安装包覆盖安装即可，
          账号与本地邮件数据不受影响。
        </p>

        <div v-if="store.updateInfo.notes" class="rounded-xl border border-line bg-panel-2/60 p-3.5">
          <div class="mb-1.5 text-[11px] font-medium tracking-wide text-zinc-500">更新说明</div>
          <div class="max-h-48 overflow-y-auto whitespace-pre-wrap text-[12px] leading-relaxed text-zinc-400">
            {{ store.updateInfo.notes }}
          </div>
        </div>
      </div>

      <div class="flex shrink-0 flex-wrap items-center gap-2 border-t border-line px-5 py-3.5">
        <button
          class="rounded-lg bg-accent px-4 py-2 text-[12.5px] font-medium text-white transition hover:bg-indigo-500 disabled:opacity-50"
          :disabled="!primary?.url"
          @click="openDownload(primary?.url ?? null)"
        >
          {{ primary?.label ?? "前往下载" }}
        </button>
        <button
          v-if="secondary"
          class="rounded-lg border border-line px-3 py-2 text-[12.5px] text-zinc-300 transition hover:border-accent/50 hover:text-indigo-300"
          @click="openDownload(secondary.url)"
        >
          {{ secondary.label }}
        </button>
        <button
          class="ml-auto rounded-lg px-3 py-2 text-[12.5px] text-zinc-400 transition hover:bg-panel-2 hover:text-zinc-200"
          @click="skip"
        >
          跳过此版本
        </button>
        <button
          class="rounded-lg px-3 py-2 text-[12.5px] text-zinc-400 transition hover:bg-panel-2 hover:text-zinc-200"
          @click="store.ui.updateOpen = false"
        >
          稍后
        </button>
      </div>
    </div>
  </div>
</template>
