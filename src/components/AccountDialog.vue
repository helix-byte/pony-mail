<script setup lang="ts">
import { computed, reactive, ref } from "vue";
import { openUrl } from "@tauri-apps/plugin-opener";

import { api, errorText } from "../api";
import type { AccountInput } from "../api/types";
import { useMailStore } from "../stores/mail";
import { isGmailHost } from "../utils/provider";

const store = useMailStore();

const form = reactive<AccountInput>({
  email: "",
  displayName: "",
  imapHost: "",
  imapPort: 993,
  imapTls: true,
  smtpHost: "",
  smtpPort: 465,
  smtpTls: true,
  username: "",
  password: "",
});

const testing = ref(false);
const saving = ref(false);
const testResult = ref<{ imapOk: boolean; smtpOk: boolean; imapMessage: string; smtpMessage: string } | null>(
  null,
);
const error = ref("");

/** 常见邮箱服务商的服务器地址预设，省去手填。 */
const presets: Record<string, { imap: string; smtp: string }> = {
  "qq.com": { imap: "imap.qq.com", smtp: "smtp.qq.com" },
  "163.com": { imap: "imap.163.com", smtp: "smtp.163.com" },
  "126.com": { imap: "imap.126.com", smtp: "smtp.126.com" },
  "gmail.com": { imap: "imap.gmail.com", smtp: "smtp.gmail.com" },
  "googlemail.com": { imap: "imap.gmail.com", smtp: "smtp.gmail.com" },
  "outlook.com": { imap: "outlook.office365.com", smtp: "smtp.office365.com" },
  "hotmail.com": { imap: "outlook.office365.com", smtp: "smtp.office365.com" },
  "foxmail.com": { imap: "imap.qq.com", smtp: "smtp.qq.com" },
  "sina.com": { imap: "imap.sina.com", smtp: "smtp.sina.com" },
};

function applyPreset() {
  const domain = form.email.split("@")[1]?.trim().toLowerCase();
  if (!domain) return;
  const hit = presets[domain];
  if (!hit) return;
  form.imapHost = hit.imap;
  form.smtpHost = hit.smtp;
  form.smtpPort = 465;
  if (!form.username) form.username = form.email;
}

/** 服务器地址和邮箱域名任一指向 Gmail 都按 Gmail 引导（Workspace 自定义域名靠主机识别）。 */
const isGmail = computed(() => {
  const domain = form.email.split("@")[1]?.trim().toLowerCase() ?? "";
  return isGmailHost(form.imapHost) || domain === "gmail.com" || domain === "googlemail.com";
});

function openAppPasswordPage() {
  void openUrl("https://myaccount.google.com/apppasswords").catch(() =>
    store.toast("打开链接失败，请检查系统默认应用", "error"),
  );
}

const canSubmit = computed(
  () =>
    form.email.trim().length > 0 &&
    form.imapHost.trim().length > 0 &&
    form.smtpHost.trim().length > 0 &&
    form.username.trim().length > 0 &&
    form.password.length > 0,
);

function validate(): string {
  if (!form.email.includes("@")) return "请填写有效的邮箱地址";
  if (!form.imapHost.trim()) return "请填写 IMAP 服务器地址";
  if (!form.smtpHost.trim()) return "请填写 SMTP 服务器地址";
  if (!form.username.trim()) return "请填写登录用户名";
  if (!form.password) return "请填写密码或授权码";
  return "";
}

async function test() {
  error.value = "";
  const problem = validate();
  if (problem) {
    error.value = problem;
    return;
  }
  testing.value = true;
  testResult.value = null;
  try {
    testResult.value = await api.testConnection({ ...form });
  } catch (e) {
    error.value = errorText(e);
  } finally {
    testing.value = false;
  }
}

async function save() {
  error.value = "";
  const problem = validate();
  if (problem) {
    error.value = problem;
    return;
  }
  saving.value = true;
  try {
    await api.addAccount({ ...form });
    store.toast("账号已添加", "ok");
    store.ui.accountOpen = false;
    await store.loadAccounts();
  } catch (e) {
    error.value = errorText(e);
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-6 backdrop-blur-sm">
    <div class="flex max-h-full w-full max-w-xl flex-col overflow-hidden rounded-2xl border border-line bg-panel shadow-2xl">
      <div class="flex shrink-0 items-center border-b border-line px-5 py-3.5">
        <h2 class="text-[14px] font-semibold text-zinc-100">添加邮箱账号</h2>
        <button
          class="ml-auto rounded-lg p-1.5 text-zinc-500 transition hover:bg-panel-2 hover:text-zinc-300"
          @click="store.ui.accountOpen = false"
        >
          <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M6 6l12 12M18 6L6 18" stroke-linecap="round" />
          </svg>
        </button>
      </div>

      <div class="min-h-0 flex-1 space-y-4 overflow-y-auto px-5 py-4">
        <div class="grid grid-cols-2 gap-3">
          <label class="col-span-2 block">
            <span class="mb-1 block text-[12px] text-zinc-400">邮箱地址</span>
            <input
              v-model="form.email"
              class="w-full rounded-lg border border-line bg-panel-2 px-3 py-2 text-[13px] text-zinc-200 outline-none transition focus:border-accent/60"
              placeholder="you@example.com"
              @blur="applyPreset"
            />
          </label>
          <label class="block">
            <span class="mb-1 block text-[12px] text-zinc-400">显示名称</span>
            <input
              v-model="form.displayName"
              class="w-full rounded-lg border border-line bg-panel-2 px-3 py-2 text-[13px] text-zinc-200 outline-none transition focus:border-accent/60"
              placeholder="可选"
            />
          </label>
          <label class="block">
            <span class="mb-1 block text-[12px] text-zinc-400">登录用户名</span>
            <input
              v-model="form.username"
              class="w-full rounded-lg border border-line bg-panel-2 px-3 py-2 text-[13px] text-zinc-200 outline-none transition focus:border-accent/60"
              placeholder="通常与邮箱相同"
            />
          </label>
          <label class="col-span-2 block">
            <span class="mb-1 block text-[12px] text-zinc-400">密码 / 授权码</span>
            <input
              v-model="form.password"
              type="password"
              class="w-full rounded-lg border border-line bg-panel-2 px-3 py-2 text-[13px] text-zinc-200 outline-none transition focus:border-accent/60"
              :placeholder="
                isGmail
                  ? '粘贴 16 位应用专用密码（可带空格）'
                  : 'QQ/163 等需使用 IMAP 授权码，不是登录密码'
              "
            />
            <span class="mt-1 block text-[11px] text-zinc-600">
              密码保存在 Windows 凭据管理器，不会写入本地数据库。
            </span>
          </label>

          <div
            v-if="isGmail"
            class="col-span-2 rounded-xl border border-line bg-panel-2/60 p-3.5 text-[12px]"
          >
            <p class="mb-1 font-medium text-zinc-300">Gmail 需要「应用专用密码」</p>
            <p class="leading-relaxed text-zinc-500">
              Gmail 不再支持用账号密码登录第三方客户端。请先在 Google 账号开启两步验证，
              然后生成 16 位应用专用密码粘贴到上方（带空格粘贴也没关系，会自动去掉）。
              若网络无法直连 Google 服务器，请先开启代理。
            </p>
            <button
              class="mt-2 text-[12px] text-indigo-300 transition hover:text-indigo-200"
              @click="openAppPasswordPage"
            >
              打开 Google 应用专用密码页面 ↗
            </button>
          </div>
        </div>

        <div class="rounded-xl border border-line bg-panel-2/60 p-3.5">
          <p class="mb-3 text-[12px] font-medium text-zinc-300">接收服务器（IMAP）</p>
          <div class="grid grid-cols-[1fr_96px] gap-3">
            <input
              v-model="form.imapHost"
              class="rounded-lg border border-line bg-panel px-3 py-2 text-[13px] text-zinc-200 outline-none focus:border-accent/60"
              placeholder="imap.example.com"
            />
            <input
              v-model.number="form.imapPort"
              type="number"
              class="rounded-lg border border-line bg-panel px-3 py-2 text-[13px] text-zinc-200 outline-none focus:border-accent/60"
            />
          </div>
          <label class="mt-2.5 flex cursor-pointer select-none items-center gap-2 text-[12px] text-zinc-400">
            <input v-model="form.imapTls" type="checkbox" class="h-3.5 w-3.5 accent-indigo-500" />
            使用 SSL/TLS 加密连接
          </label>
        </div>

        <div class="rounded-xl border border-line bg-panel-2/60 p-3.5">
          <p class="mb-3 text-[12px] font-medium text-zinc-300">发送服务器（SMTP）</p>
          <div class="grid grid-cols-[1fr_96px] gap-3">
            <input
              v-model="form.smtpHost"
              class="rounded-lg border border-line bg-panel px-3 py-2 text-[13px] text-zinc-200 outline-none focus:border-accent/60"
              placeholder="smtp.example.com"
            />
            <input
              v-model.number="form.smtpPort"
              type="number"
              class="rounded-lg border border-line bg-panel px-3 py-2 text-[13px] text-zinc-200 outline-none focus:border-accent/60"
            />
          </div>
          <label class="mt-2.5 flex cursor-pointer select-none items-center gap-2 text-[12px] text-zinc-400">
            <input v-model="form.smtpTls" type="checkbox" class="h-3.5 w-3.5 accent-indigo-500" />
            使用 SSL/TLS 加密连接（465 为 SSL，587 为 STARTTLS）
          </label>
        </div>

        <div v-if="testResult" class="space-y-2 rounded-xl border border-line bg-panel-2/60 p-3.5">
          <div class="flex items-start gap-2 text-[12.5px]">
            <span :class="testResult.imapOk ? 'text-emerald-400' : 'text-rose-400'">
              {{ testResult.imapOk ? "✓" : "✗" }}
            </span>
            <div class="min-w-0">
              <p class="text-zinc-300">IMAP 接收</p>
              <p class="mt-0.5 break-words text-[11.5px] text-zinc-500">{{ testResult.imapMessage }}</p>
            </div>
          </div>
          <div class="flex items-start gap-2 text-[12.5px]">
            <span :class="testResult.smtpOk ? 'text-emerald-400' : 'text-rose-400'">
              {{ testResult.smtpOk ? "✓" : "✗" }}
            </span>
            <div class="min-w-0">
              <p class="text-zinc-300">SMTP 发送</p>
              <p class="mt-0.5 break-words text-[11.5px] text-zinc-500">{{ testResult.smtpMessage }}</p>
            </div>
          </div>
        </div>

        <p v-if="error" class="text-[12.5px] text-rose-400">{{ error }}</p>
      </div>

      <div class="flex shrink-0 items-center gap-3 border-t border-line px-5 py-3.5">
        <button
          class="rounded-lg border border-line px-3.5 py-2 text-[12.5px] text-zinc-300 transition hover:border-accent/50 hover:text-indigo-300 disabled:opacity-50"
          :disabled="testing"
          @click="test"
        >
          {{ testing ? "测试中…" : "测试连接" }}
        </button>
        <button
          class="rounded-lg bg-accent px-4 py-2 text-[12.5px] font-medium text-white transition hover:bg-indigo-500 disabled:opacity-50"
          :disabled="!canSubmit || saving"
          @click="save"
        >
          {{ saving ? "保存中…" : "保存账号" }}
        </button>
        <button
          class="ml-auto rounded-lg px-3 py-2 text-[12.5px] text-zinc-400 transition hover:bg-panel-2 hover:text-zinc-200"
          @click="store.ui.accountOpen = false"
        >
          取消
        </button>
      </div>
    </div>
  </div>
</template>
