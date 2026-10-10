<script setup lang="ts">
import { onMounted } from "vue";

import AccountDialog from "./components/AccountDialog.vue";
import AccountSidebar from "./components/AccountSidebar.vue";
import ComposeDialog from "./components/ComposeDialog.vue";
import MessageList from "./components/MessageList.vue";
import MessageReader from "./components/MessageReader.vue";
import SettingsDialog from "./components/SettingsDialog.vue";
import ToastBar from "./components/ToastBar.vue";
import { useMailStore } from "./stores/mail";

const store = useMailStore();

onMounted(() => {
  void store.loadAccounts();
  // AI 设置（含「同步后自动整理」开关）在启动时并入缓存。
  void store.loadAiSettings();
  // 实时收信：接入新邮件与监听状态事件（此后常驻，无需重复调用）。
  void store.initRealtime();
});
</script>

<template>
  <div class="flex h-full w-full overflow-hidden bg-shell text-zinc-200">
    <AccountSidebar />
    <MessageList />
    <MessageReader />
  </div>

  <ComposeDialog v-if="store.ui.composeOpen" />
  <AccountDialog v-if="store.ui.accountOpen" />
  <SettingsDialog v-if="store.ui.settingsOpen" />
  <ToastBar />
</template>
