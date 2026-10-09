<script setup lang="ts">
import { computed, ref } from "vue";

import { useMailStore } from "../stores/mail";

const store = useMailStore();

const draft = store.ui.composeDraft;
const to = ref(draft?.to ?? "");
const cc = ref(draft?.cc ?? "");
const subject = ref(draft?.subject ?? "");
const body = ref(draft?.body ?? "");
const html = ref(draft?.html ?? false);

const error = ref("");
const showCc = ref(Boolean(draft?.cc));

const isValid = computed(() => to.value.trim().length > 0 && !store.busy.sending);

async function submit() {
  error.value = "";
  if (!to.value.trim()) {
    error.value = "请填写收件人";
    return;
  }
  const ok = await store.send({
    to: to.value,
    cc: cc.value,
    subject: subject.value,
    body: body.value,
    html: html.value,
  });
  if (ok) store.closeCompose();
}
</script>

<template>
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-6 backdrop-blur-sm">
    <div class="flex max-h-full w-full max-w-2xl flex-col overflow-hidden rounded-2xl border border-line bg-panel shadow-2xl">
      <div class="flex h-13 shrink-0 items-center border-b border-line px-5 py-3.5">
        <h2 class="text-[14px] font-semibold text-zinc-100">写邮件</h2>
        <span class="ml-3 text-[11.5px] text-zinc-500">发件账号：{{ store.activeAccount?.email ?? "未选择" }}</span>
        <button
          class="ml-auto rounded-lg p-1.5 text-zinc-500 transition hover:bg-panel-2 hover:text-zinc-300"
          @click="store.closeCompose()"
        >
          <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M6 6l12 12M18 6L6 18" stroke-linecap="round" />
          </svg>
        </button>
      </div>

      <div class="min-h-0 flex-1 overflow-y-auto">
        <div class="flex items-center border-b border-line/70 px-5">
          <span class="w-14 shrink-0 text-[12.5px] text-zinc-500">收件人</span>
          <input
            v-model="to"
            class="min-w-0 flex-1 bg-transparent py-2.5 text-[13px] text-zinc-200 outline-none placeholder:text-zinc-600"
            placeholder="多个地址用逗号分隔"
          />
          <button
            v-if="!showCc"
            class="shrink-0 text-[11.5px] text-zinc-500 transition hover:text-indigo-300"
            @click="showCc = true"
          >
            抄送
          </button>
        </div>

        <div v-if="showCc" class="flex items-center border-b border-line/70 px-5">
          <span class="w-14 shrink-0 text-[12.5px] text-zinc-500">抄送</span>
          <input
            v-model="cc"
            class="min-w-0 flex-1 bg-transparent py-2.5 text-[13px] text-zinc-200 outline-none placeholder:text-zinc-600"
            placeholder="可选"
          />
        </div>

        <div class="flex items-center border-b border-line/70 px-5">
          <span class="w-14 shrink-0 text-[12.5px] text-zinc-500">主题</span>
          <input
            v-model="subject"
            class="min-w-0 flex-1 bg-transparent py-2.5 text-[13px] text-zinc-200 outline-none placeholder:text-zinc-600"
            placeholder="邮件主题"
          />
        </div>

        <textarea
          v-model="body"
          class="h-72 w-full resize-none bg-transparent px-5 py-4 text-[13px] leading-relaxed text-zinc-200 outline-none placeholder:text-zinc-600"
          :placeholder="html ? '支持 HTML 源码' : '写点什么…'"
        />
      </div>

      <div class="flex shrink-0 items-center gap-3 border-t border-line px-5 py-3.5">
        <button
          class="flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-[13px] font-medium text-white transition hover:bg-indigo-500 disabled:opacity-50"
          :disabled="!isValid"
          @click="submit"
        >
          <svg v-if="store.busy.sending" class="h-3.5 w-3.5 animate-spin" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5">
            <path d="M21 12a9 9 0 1 1-6.2-8.6" stroke-linecap="round" />
          </svg>
          <svg v-else class="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M4 12l16-8-6 8 6 8z" stroke-linejoin="round" />
          </svg>
          {{ store.busy.sending ? "发送中…" : "发送" }}
        </button>

        <label class="flex cursor-pointer select-none items-center gap-1.5 text-[12px] text-zinc-400">
          <input v-model="html" type="checkbox" class="h-3.5 w-3.5 accent-indigo-500" />
          以 HTML 格式发送
        </label>

        <span v-if="error" class="text-[12px] text-rose-400">{{ error }}</span>

        <button
          class="ml-auto rounded-lg px-3 py-2 text-[12.5px] text-zinc-400 transition hover:bg-panel-2 hover:text-zinc-200"
          @click="store.closeCompose()"
        >
          取消
        </button>
      </div>
    </div>
  </div>
</template>
