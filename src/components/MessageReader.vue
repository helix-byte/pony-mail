<script setup lang="ts">
import { computed, ref, watch } from "vue";

import { useMailStore } from "../stores/mail";
import { avatarColor, avatarText, formatDateTime, formatSize } from "../utils/format";

const store = useMailStore();

const iframeRef = ref<HTMLIFrameElement | null>(null);
const frameHeight = ref(0);

const detail = computed(() => store.detail);

/**
 * 邮件正文用 sandbox iframe 渲染：
 * 不授予 allow-scripts，即使清洗层漏掉脚本也无法执行，也就拿不到 Tauri 能力。
 */
const srcdoc = computed(() => {
  const d = store.detail;
  if (!d) return "";
  const body = d.bodyHtml?.trim() ? d.bodyHtml : "<p style='color:#71717a'>（这封邮件没有可显示的正文）</p>";
  return `<!doctype html><html><head><meta charset="utf-8"><base target="_blank"><style>
html,body{margin:0;padding:0}
body{font-family:-apple-system,"Segoe UI","Microsoft YaHei",sans-serif;font-size:14px;line-height:1.72;color:#18181b;padding:18px 22px;word-break:break-word;background:#fff}
img{max-width:100%;height:auto}
table{max-width:100%;border-collapse:collapse}
a{color:#4f46e5}
pre{white-space:pre-wrap;font-family:inherit}
blockquote{border-left:3px solid #e4e4e7;margin:8px 0;padding-left:12px;color:#52525b}
</style></head><body>${body}</body></html>`;
});

function measure() {
  const el = iframeRef.value;
  if (!el) return;
  try {
    const doc = el.contentDocument;
    if (doc && doc.documentElement) {
      frameHeight.value = doc.documentElement.scrollHeight;
    }
  } catch {
    // 跨源访问受限时保持默认高度
  }
}

watch(srcdoc, () => {
  frameHeight.value = 0;
});

function shortAddr(list: string): string {
  return list
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean)
    .join("、");
}
</script>

<template>
  <section class="flex min-w-0 flex-1 flex-col bg-shell">
    <!-- 空状态 -->
    <div
      v-if="!detail && !store.busy.detail"
      class="flex flex-1 flex-col items-center justify-center gap-3 text-zinc-600"
    >
      <svg class="h-11 w-11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.2">
        <rect x="3" y="5" width="18" height="14" rx="2" />
        <path d="M3 7l9 6 9-6" stroke-linecap="round" />
      </svg>
      <p class="text-[13px]">选择一封邮件开始阅读</p>
    </div>

    <div v-else-if="store.busy.detail" class="flex flex-1 items-center justify-center text-[12px] text-zinc-500">
      正在从服务器拉取正文…
    </div>

    <template v-else-if="detail">
      <!-- 头部 -->
      <header class="shrink-0 border-b border-line px-6 pt-5 pb-4">
        <h1 class="text-[17px] font-semibold leading-snug text-zinc-100">
          {{ detail.header.subject }}
        </h1>

        <div class="mt-3 flex items-center gap-3">
          <span
            class="flex h-9 w-9 shrink-0 items-center justify-center rounded-full text-[13px] font-semibold text-white"
            :style="{ background: avatarColor(detail.header.fromAddr || detail.header.fromName) }"
          >
            {{ avatarText(detail.header.fromName, detail.header.fromAddr) }}
          </span>
          <div class="min-w-0 flex-1">
            <p class="truncate text-[13px] text-zinc-200">
              <span class="font-medium">{{ detail.header.fromName || detail.header.fromAddr }}</span>
              <span class="ml-1.5 text-zinc-500">&lt;{{ detail.header.fromAddr }}&gt;</span>
            </p>
            <p class="mt-0.5 truncate text-[11.5px] text-zinc-500">
              收件人：{{ shortAddr(detail.toAddrs) || "—" }}
              <template v-if="detail.ccAddrs"> · 抄送：{{ shortAddr(detail.ccAddrs) }}</template>
              · {{ formatDateTime(detail.header.dateTs) }}
            </p>
          </div>

          <!-- 操作 -->
          <div class="flex shrink-0 items-center gap-1">
            <button
              class="flex h-8 w-8 items-center justify-center rounded-lg transition hover:bg-panel-2"
              :class="detail.header.flagged ? 'text-amber-400' : 'text-zinc-500 hover:text-zinc-300'"
              title="星标"
              @click="store.toggleFlag(detail.header.id)"
            >
              <svg class="h-4 w-4" viewBox="0 0 24 24" :fill="detail.header.flagged ? 'currentColor' : 'none'" stroke="currentColor" stroke-width="1.8">
                <path d="M12 3.5l2.6 5.3 5.9.9-4.2 4.1 1 5.8-5.3-2.8-5.3 2.8 1-5.8L3.5 9.7l5.9-.9z" stroke-linejoin="round" />
              </svg>
            </button>
            <button
              class="flex h-8 w-8 items-center justify-center rounded-lg text-zinc-500 transition hover:bg-panel-2 hover:text-zinc-300"
              title="标记为未读"
              @click="store.setSeen(detail.header.id, false)"
            >
              <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <rect x="3" y="5" width="18" height="14" rx="2" />
                <path d="M3 7l9 6 9-6" stroke-linecap="round" />
              </svg>
            </button>
            <button
              class="flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-[12.5px] text-zinc-400 transition hover:bg-panel-2 hover:text-zinc-200"
              @click="store.replyToActive()"
            >
              <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M9 14L4 9l5-5" stroke-linecap="round" stroke-linejoin="round" />
                <path d="M4 9h10a6 6 0 0 1 6 6v5" stroke-linecap="round" stroke-linejoin="round" />
              </svg>
              回复
            </button>
            <button
              class="flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-[12.5px] text-zinc-400 transition hover:bg-panel-2 hover:text-zinc-200"
              @click="store.forwardActive()"
            >
              <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M15 14l5-5-5-5" stroke-linecap="round" stroke-linejoin="round" />
                <path d="M20 9H10a6 6 0 0 0-6 6v5" stroke-linecap="round" stroke-linejoin="round" />
              </svg>
              转发
            </button>
          </div>
        </div>
      </header>

      <!-- 正文滚动区 -->
      <div class="min-h-0 flex-1 overflow-y-auto">
        <!-- AI 摘要 -->
        <div class="px-6 pt-4">
          <div
            v-if="!store.summary && !store.summaryLoading && !store.summaryError"
            class="flex items-center gap-3 rounded-xl border border-line bg-panel px-4 py-3"
          >
            <div class="flex h-7 w-7 items-center justify-center rounded-lg bg-gradient-to-br from-indigo-500 to-violet-600">
              <svg class="h-3.5 w-3.5 text-white" viewBox="0 0 24 24" fill="currentColor">
                <path d="M12 2l1.9 5.6L19.5 9l-4.3 3.4L16.4 18 12 15.1 7.6 18l1.2-5.6L4.5 9l5.6-1.4z" />
              </svg>
            </div>
            <p class="min-w-0 flex-1 text-[12.5px] text-zinc-400">
              用本地 Ollama 模型快速提炼这封邮件的要点
            </p>
            <button
              class="shrink-0 rounded-lg bg-accent px-3 py-1.5 text-[12.5px] font-medium text-white transition hover:bg-indigo-500"
              @click="store.summarize()"
            >
              生成摘要
            </button>
          </div>

          <div
            v-else
            class="rounded-xl border border-indigo-500/25 bg-gradient-to-br from-indigo-500/[0.09] to-violet-500/[0.05] px-4 py-3.5"
          >
            <div class="flex items-center gap-2">
              <span class="text-[12.5px] font-medium text-indigo-300">AI 摘要</span>
              <span v-if="store.summary?.cached" class="rounded bg-indigo-500/15 px-1.5 py-0.5 text-[10px] text-indigo-300/80">
                缓存
              </span>
              <span v-if="store.summary?.model" class="truncate text-[10.5px] text-zinc-500">
                {{ store.summary.model }}
              </span>
              <div class="ml-auto flex items-center gap-2">
                <button
                  class="text-[11.5px] text-zinc-500 transition hover:text-indigo-300"
                  :disabled="store.summaryLoading"
                  @click="store.summarize(true)"
                >
                  重新生成
                </button>
              </div>
            </div>

            <p v-if="store.summaryLoading" class="mt-2.5 flex items-center gap-2 text-[12.5px] text-zinc-400">
              <span class="h-3 w-3 animate-spin rounded-full border-[1.5px] border-indigo-400 border-t-transparent" />
              本地模型推理中，7B 模型通常需要几秒…
            </p>
            <p v-else-if="store.summaryError" class="mt-2.5 text-[12.5px] leading-relaxed text-rose-300">
              {{ store.summaryError }}
            </p>
            <p v-else class="summary-text mt-2.5 text-[13px] text-zinc-200">{{ store.summary?.text }}</p>
          </div>
        </div>

        <!-- 附件 -->
        <div v-if="detail.attachments.length" class="px-6 pt-4">
          <p class="mb-2 text-[11px] font-medium uppercase tracking-wider text-zinc-600">
            附件（{{ detail.attachments.length }}）
          </p>
          <div class="flex flex-wrap gap-2">
            <div
              v-for="(att, i) in detail.attachments"
              :key="i"
              class="flex items-center gap-2 rounded-lg border border-line bg-panel px-3 py-2"
            >
              <svg class="h-4 w-4 shrink-0 text-zinc-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
                <path d="M14 3v5h5" stroke-linecap="round" stroke-linejoin="round" />
                <path d="M6 2h8l5 5v13a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1z" stroke-linejoin="round" />
              </svg>
              <span class="max-w-[220px] truncate text-[12.5px] text-zinc-300">{{ att.filename }}</span>
              <span class="text-[11px] text-zinc-600">{{ formatSize(att.size) }}</span>
            </div>
          </div>
        </div>

        <!-- 正文 -->
        <div class="p-6">
          <iframe
            ref="iframeRef"
            class="mail-frame"
            sandbox="allow-same-origin"
            :srcdoc="srcdoc"
            :style="{ height: (frameHeight || 560) + 'px' }"
            @load="measure"
          />
        </div>
      </div>
    </template>
  </section>
</template>
