<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { openUrl } from "@tauri-apps/plugin-opener";

import { useMailStore } from "../stores/mail";
import { avatarColor, avatarText, formatDateTime, formatSize } from "../utils/format";
import { buildMailSrcdoc, hasDarkMediaQuery } from "../utils/mailHtml";

const store = useMailStore();

const iframeRef = ref<HTMLIFrameElement | null>(null);
const frameHeight = ref(0);

const detail = computed(() => store.detail);

/** 系统深色偏好（响应中途切换）。 */
const darkPref = window.matchMedia("(prefers-color-scheme: dark)");
const isDarkPref = ref(darkPref.matches);
const onDarkPrefChange = (e: MediaQueryListEvent) => {
  isDarkPref.value = e.matches;
};

/** 当前邮件是否处于强制深色转换状态：深色偏好且邮件无自带深色适配。 */
const forceDarkActive = computed(
  () => isDarkPref.value && !!store.detail && !hasDarkMediaQuery(store.detail.bodyHtml ?? ""),
);

/** 阅读区「查看原版外观」：为 true 时绕过强制深色转换，按原版渲染。 */
const showOriginal = ref(false);
// 切换到另一封邮件时回到默认（深色适配）渲染。
watch(
  () => store.detail,
  () => {
    showOriginal.value = false;
  },
);

/**
 * 邮件正文用 sandbox iframe 渲染：
 * 不授予 allow-scripts，即使清洗层漏掉脚本也无法执行，也就拿不到 Tauri 能力。
 * 文档构造（含深色模式防护）见 utils/mailHtml.ts 的 buildMailSrcdoc。
 */
const srcdoc = computed(() => {
  const d = store.detail;
  if (!d) return "";
  const body = d.bodyHtml?.trim() ? d.bodyHtml : "<p style='color:#71717a'>（这封邮件没有可显示的正文）</p>";
  return buildMailSrcdoc(body, { forceDark: !showOriginal.value });
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

/**
 * 正文里的链接由外层沙箱接管打开：
 * iframe 的 sandbox 未授予 allow-popups，`target="_blank"` 的新窗口请求
 * 会被浏览器静默丢弃，Tauri / WebView2 也不会代开系统浏览器，
 * 因此在文档上委托监听点击，命中 http(s) / mailto 链接时阻止默认行为，
 * 交给系统默认应用（浏览器 / 邮件客户端）打开。
 */
function bindFrameLinks() {
  const doc = iframeRef.value?.contentDocument;
  if (!doc) return;
  doc.addEventListener("click", (e) => {
    // e.target 是 iframe realm 的元素，父窗口侧的 `instanceof Element` 判定会失效，
    // 所以用 closest 做鸭子类型探测。
    const anchor = (e.target as Element | null)?.closest?.("a") ?? null;
    const href = anchor?.getAttribute("href")?.trim() ?? "";
    if (!/^(https?|mailto):/i.test(href)) return;
    e.preventDefault();
    void openUrl(href).catch(() => store.toast("打开链接失败，请检查系统默认应用", "error"));
  });
  // iframe 内的右键事件不会冒泡到父页面，需在帧文档上单独接管。
  doc.addEventListener("contextmenu", onFrameContextMenu);
}

function onFrameLoad() {
  bindFrameLinks();
  measure();
}

// ------------------------------------------------------------------
// 正文区右键菜单：「查看原版外观」/「恢复深色显示」
// ------------------------------------------------------------------

/**
 * 仅在邮件正被强制深色转换时接管右键并弹出菜单（浅色偏好或邮件
 * 自带深色适配时无内容可切换，右键保持原样不弹菜单）。
 */
const menu = reactive({ visible: false, x: 0, y: 0 });
const menuEl = ref<HTMLElement | null>(null);
/** 最近一次右键的视口坐标：重新定位时始终以它为锚点。 */
const menuAnchor = { x: 0, y: 0 };
/** 菜单与视口边缘需要保留的安全边距（px）。 */
const MENU_MARGIN = 8;

/** 以右键锚点放置菜单：水平超出右缘时向左收回；垂直放不下时翻到光标上方。 */
function placeMenuAt(h: number, w: number) {
  let x = menuAnchor.x;
  if (x + w + MENU_MARGIN > window.innerWidth) {
    x = Math.max(MENU_MARGIN, window.innerWidth - w - MENU_MARGIN);
  }
  let y = menuAnchor.y;
  if (y + h + MENU_MARGIN > window.innerHeight) {
    const above = menuAnchor.y - h;
    y = above >= MENU_MARGIN ? above : Math.max(MENU_MARGIN, window.innerHeight - h - MENU_MARGIN);
  }
  menu.x = x;
  menu.y = y;
}

/** 按菜单真实渲染高度校准位置（打开时先按估算高度粗定位）。 */
function placeMenu() {
  const el = menuEl.value;
  if (!el) return;
  placeMenuAt(el.offsetHeight, el.offsetWidth);
}

function openMenuAt(x: number, y: number) {
  menuAnchor.x = x;
  menuAnchor.y = y;
  // 单项菜单的基础高度约 40px、宽度对应 w-44；渲染后 placeMenu 再按真实值校准。
  placeMenuAt(40, 176);
  menu.visible = true;
  void nextTick(placeMenu);
}

function closeMenu() {
  menu.visible = false;
}

/** 正文区（含空白处）右键。 */
function onPaneContextMenu(e: MouseEvent) {
  if (!forceDarkActive.value) return;
  e.preventDefault();
  openMenuAt(e.clientX, e.clientY);
}

/** iframe 文档（邮件内容）内右键：帧内坐标换算到父页面视口。 */
function onFrameContextMenu(e: MouseEvent) {
  if (!forceDarkActive.value) return;
  e.preventDefault();
  const rect = iframeRef.value?.getBoundingClientRect();
  if (!rect) return;
  openMenuAt(rect.left + e.clientX, rect.top + e.clientY);
}

function onToggleOriginal() {
  showOriginal.value = !showOriginal.value;
  closeMenu();
}

function onMenuKeydown(e: KeyboardEvent) {
  if (e.key === "Escape") closeMenu();
}

onMounted(() => {
  darkPref.addEventListener("change", onDarkPrefChange);
  // 点击 / 缩放 / Esc 都应关闭菜单；菜单内部按钮的 click 经 @click.stop 不外泄。
  window.addEventListener("click", closeMenu);
  window.addEventListener("resize", closeMenu);
  window.addEventListener("keydown", onMenuKeydown);
});

onBeforeUnmount(() => {
  darkPref.removeEventListener("change", onDarkPrefChange);
  window.removeEventListener("click", closeMenu);
  window.removeEventListener("resize", closeMenu);
  window.removeEventListener("keydown", onMenuKeydown);
});

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
            <div v-if="store.tagsOf(detail.header.id).length" class="mt-1.5 flex flex-wrap gap-1.5">
              <button
                v-for="t in store.tagsOf(detail.header.id)"
                :key="t.id"
                class="inline-flex items-center gap-1 rounded-full px-2 py-[2px] text-[10.5px] font-medium transition hover:opacity-80"
                :style="{ background: `${t.color}26`, color: t.color }"
                title="点击移除该标签"
                @click="store.toggleMessageTag(detail.header.id, t.id)"
              >
                <span class="h-1.5 w-1.5 shrink-0 rounded-full" :style="{ background: t.color }" />
                {{ t.name }}
                <svg class="h-2.5 w-2.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3">
                  <path d="M6 6l12 12M18 6L6 18" stroke-linecap="round" />
                </svg>
              </button>
            </div>
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
      <div class="min-h-0 flex-1 overflow-y-auto" @scroll="closeMenu">
        <!-- AI 摘要 -->
        <div class="px-6 pt-4">
          <div
            v-if="!store.summary && !store.summaryLoading && !store.summaryPending && !store.summaryError"
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
                  :disabled="store.summaryLoading || store.summaryPending"
                  @click="store.summarize(true)"
                >
                  重新生成
                </button>
              </div>
            </div>

            <p v-if="store.summaryLoading || store.summaryPending" class="mt-2.5 flex items-center gap-2 text-[12.5px] text-zinc-400">
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
        <div class="p-6" @contextmenu="onPaneContextMenu">
          <iframe
            ref="iframeRef"
            class="mail-frame"
            sandbox="allow-same-origin"
            :srcdoc="srcdoc"
            :style="{ height: (frameHeight || 560) + 'px' }"
            @load="onFrameLoad"
          />
        </div>
      </div>
    </template>

    <!-- 正文区右键菜单：仅在强制深色转换可切换时弹出 -->
    <Teleport to="body">
      <div
        v-if="menu.visible"
        ref="menuEl"
        class="fixed z-50 w-44 rounded-lg border border-line bg-panel py-1 shadow-2xl shadow-black/50"
        :style="{ left: `${menu.x}px`, top: `${menu.y}px` }"
        @click.stop
        @contextmenu.stop.prevent
      >
        <button
          class="flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-[12.5px] text-zinc-300 transition hover:bg-hover"
          @click="onToggleOriginal"
        >
          <svg class="h-3.5 w-3.5 shrink-0 text-zinc-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
            <circle cx="12" cy="12" r="9" />
            <path d="M12 3a9 9 0 0 0 0 18z" fill="currentColor" stroke="none" />
          </svg>
          {{ showOriginal ? "恢复深色显示" : "查看原版外观" }}
        </button>
      </div>
    </Teleport>
  </section>
</template>
