<script setup lang="ts">
import { onMounted, ref } from "vue";

import { api, errorText } from "../api";
import { useMailStore } from "../stores/mail";

const store = useMailStore();

const baseUrl = ref("http://localhost:11434");
const model = ref("qwen3.5:9b");
/** 同步完成后自动整理新邮件（缺省开启）。 */
const autoClassify = ref(true);
/** 打开邮件时自动生成摘要（缺省开启）。 */
const autoSummarize = ref(true);
/** 新邮件到达时弹系统通知（缺省开启）。 */
const notifyNewMail = ref(true);
const models = ref<string[]>([]);
const online = ref(false);
const message = ref("");
const checking = ref(false);
const saving = ref(false);

onMounted(async () => {
  try {
    const s = await api.getAiSettings();
    baseUrl.value = s.baseUrl;
    model.value = s.model;
    autoClassify.value = s.autoClassify;
    autoSummarize.value = s.autoSummarize;
  } catch (e) {
    store.toast(errorText(e), "error");
  }
  try {
    const m = await api.getMailSettings();
    notifyNewMail.value = m.notifyNewMail;
  } catch (e) {
    store.toast(errorText(e), "error");
  }
});

async function check() {
  checking.value = true;
  message.value = "";
  try {
    const s = await api.aiStatus();
    online.value = s.online;
    models.value = s.models;
    message.value = s.message;
    if (s.online && s.models.length > 0 && !s.models.includes(model.value)) {
      model.value = s.models[0];
    }
  } catch (e) {
    online.value = false;
    message.value = errorText(e);
  } finally {
    checking.value = false;
  }
}

async function save() {
  saving.value = true;
  try {
    await api.setMailSettings({ notifyNewMail: notifyNewMail.value });
    await api.setAiSettings({
      baseUrl: baseUrl.value.trim(),
      model: model.value.trim(),
      autoClassify: autoClassify.value,
      autoSummarize: autoSummarize.value,
    });
    // 刷新 store 缓存：自动整理开关立即生效（sync 完成后读取的正是它）。
    void store.loadAiSettings();
    store.toast("设置已保存", "ok");
    store.ui.settingsOpen = false;
  } catch (e) {
    store.toast(errorText(e), "error");
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-6 backdrop-blur-sm">
    <div class="flex max-h-full w-full max-w-lg flex-col overflow-hidden rounded-2xl border border-line bg-panel shadow-2xl">
      <div class="flex shrink-0 items-center border-b border-line px-5 py-3.5">
        <h2 class="text-[14px] font-semibold text-zinc-100">设置</h2>
        <span class="ml-3 text-[11.5px] text-zinc-500">收信、通知与本地 AI</span>
        <button
          class="ml-auto rounded-lg p-1.5 text-zinc-500 transition hover:bg-panel-2 hover:text-zinc-300"
          @click="store.ui.settingsOpen = false"
        >
          <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M6 6l12 12M18 6L6 18" stroke-linecap="round" />
          </svg>
        </button>
      </div>

      <div class="min-h-0 flex-1 space-y-4 overflow-y-auto px-5 py-4">
        <label class="flex cursor-pointer items-start gap-3 rounded-xl border border-line bg-panel-2/60 p-3.5">
          <input v-model="notifyNewMail" type="checkbox" class="mt-0.5 h-4 w-4 shrink-0 accent-indigo-500" />
          <span class="min-w-0">
            <span class="block text-[12.5px] text-zinc-300">新邮件通知</span>
            <span class="mt-0.5 block text-[11px] leading-relaxed text-zinc-500">
              收到新邮件时弹出系统通知。应用会为每个账号保持 IMAP 实时监听
              （服务器支持时走 IDLE 推送），新邮件自动出现在列表里，无需手动同步；
              关闭此项后不再弹系统通知，仅保留应用内提示。
            </span>
          </span>
        </label>

        <div class="flex items-center gap-3 pt-1">
          <span class="text-[11px] font-medium tracking-wide text-zinc-500">AI · 本地 Ollama</span>
          <span class="h-px flex-1 bg-line" />
        </div>

        <label class="block">
          <span class="mb-1 block text-[12px] text-zinc-400">Ollama 服务地址</span>
          <input
            v-model="baseUrl"
            class="w-full rounded-lg border border-line bg-panel-2 px-3 py-2 text-[13px] text-zinc-200 outline-none transition focus:border-accent/60"
            placeholder="http://localhost:11434"
          />
        </label>

        <label class="block">
          <span class="mb-1 block text-[12px] text-zinc-400">模型</span>
          <input
            v-model="model"
            class="w-full rounded-lg border border-line bg-panel-2 px-3 py-2 text-[13px] text-zinc-200 outline-none transition focus:border-accent/60"
            placeholder="qwen2.5:7b"
            list="ollama-models"
          />
          <datalist id="ollama-models">
            <option v-for="m in models" :key="m" :value="m" />
          </datalist>
        </label>

        <div v-if="models.length > 0" class="flex flex-wrap gap-1.5">
          <button
            v-for="m in models"
            :key="m"
            class="rounded-md border px-2 py-1 text-[11.5px] transition"
            :class="
              m === model
                ? 'border-accent/60 bg-accent/15 text-indigo-300'
                : 'border-line text-zinc-400 hover:border-accent/40 hover:text-zinc-200'
            "
            @click="model = m"
          >
            {{ m }}
          </button>
        </div>

        <label class="flex cursor-pointer items-start gap-3 rounded-xl border border-line bg-panel-2/60 p-3.5">
          <input v-model="autoClassify" type="checkbox" class="mt-0.5 h-4 w-4 shrink-0 accent-indigo-500" />
          <span class="min-w-0">
            <span class="block text-[12.5px] text-zinc-300">同步完成后自动整理</span>
            <span class="mt-0.5 block text-[11px] leading-relaxed text-zinc-500">
              同步收件箱后，自动用本地模型为未整理的邮件打标签。
              在左侧「标签」区域编辑标签组时，可选择哪些组交给 AI 整理。
            </span>
          </span>
        </label>

        <label class="flex cursor-pointer items-start gap-3 rounded-xl border border-line bg-panel-2/60 p-3.5">
          <input v-model="autoSummarize" type="checkbox" class="mt-0.5 h-4 w-4 shrink-0 accent-indigo-500" />
          <span class="min-w-0">
            <span class="block text-[12.5px] text-zinc-300">打开邮件时自动生成摘要</span>
            <span class="mt-0.5 block text-[11px] leading-relaxed text-zinc-500">
              点开邮件后自动用本地模型提炼要点；命中缓存的邮件直接展示已有摘要，不会重复推理。
              关闭后仍可在阅读区手动点击「生成摘要」。
            </span>
          </span>
        </label>

        <div class="rounded-xl border border-line bg-panel-2/60 p-3.5">
          <div class="flex items-center gap-2">
            <span
              class="h-1.5 w-1.5 rounded-full"
              :class="online ? 'bg-emerald-400' : 'bg-zinc-600'"
            />
            <span class="text-[12.5px] text-zinc-300">
              {{ online ? "Ollama 在线" : "未检测 / 离线" }}
            </span>
            <button
              class="ml-auto rounded-lg border border-line px-3 py-1.5 text-[12px] text-zinc-300 transition hover:border-accent/50 hover:text-indigo-300 disabled:opacity-50"
              :disabled="checking"
              @click="check"
            >
              {{ checking ? "检测中…" : "检测连接" }}
            </button>
          </div>
          <p v-if="message" class="mt-2.5 break-words text-[11.5px] leading-relaxed text-zinc-500">
            {{ message }}
          </p>
          <p class="mt-3 border-t border-line/70 pt-3 text-[11.5px] leading-relaxed text-zinc-500">
            若提示无法连接，请先在终端运行
            <code class="rounded bg-panel px-1.5 py-0.5 text-zinc-300">ollama serve</code>
            （桌面版 Ollama 安装后会自动常驻）。没有模型时用
            <code class="rounded bg-panel px-1.5 py-0.5 text-zinc-300">ollama pull qwen2.5:7b</code>
            下载。
          </p>
        </div>
      </div>

      <div class="flex shrink-0 items-center gap-3 border-t border-line px-5 py-3.5">
        <button
          class="rounded-lg bg-accent px-4 py-2 text-[12.5px] font-medium text-white transition hover:bg-indigo-500 disabled:opacity-50"
          :disabled="saving"
          @click="save"
        >
          {{ saving ? "保存中…" : "保存" }}
        </button>
        <button
          class="ml-auto rounded-lg px-3 py-2 text-[12.5px] text-zinc-400 transition hover:bg-panel-2 hover:text-zinc-200"
          @click="store.ui.settingsOpen = false"
        >
          关闭
        </button>
      </div>
    </div>
  </div>
</template>
