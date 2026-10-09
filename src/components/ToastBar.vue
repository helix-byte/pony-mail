<script setup lang="ts">
import { useMailStore } from "../stores/mail";

const store = useMailStore();

const styles: Record<string, string> = {
  info: "border-sky-500/30 bg-sky-500/10 text-sky-100",
  ok: "border-emerald-500/30 bg-emerald-500/10 text-emerald-100",
  error: "border-rose-500/30 bg-rose-500/10 text-rose-100",
};
</script>

<template>
  <div
    class="pointer-events-none fixed bottom-6 left-1/2 z-[60] flex w-full max-w-lg -translate-x-1/2 flex-col gap-2 px-4"
  >
    <TransitionGroup
      enter-active-class="transition duration-200 ease-out"
      enter-from-class="translate-y-2 opacity-0"
      leave-active-class="transition duration-150 ease-in"
      leave-to-class="opacity-0"
    >
      <div
        v-for="t in store.toasts"
        :key="t.id"
        class="pointer-events-auto rounded-lg border px-4 py-2.5 text-[13px] leading-relaxed shadow-2xl backdrop-blur-sm"
        :class="styles[t.kind]"
      >
        {{ t.text }}
      </div>
    </TransitionGroup>
  </div>
</template>
