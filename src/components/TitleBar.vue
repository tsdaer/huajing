<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import Icon from "./Icon.vue";
import {
  closeWindow,
  isTauri,
  isWindowMaximized,
  minimizeWindow,
  onWindowResized,
  toggleMaximizeWindow,
} from "../window";

// 无边框窗口的自定义标题栏：整条可拖拽，右侧是窗口按钮。
// 颜色全部走主题令牌，因此换主题时标题栏与内容一致。

const maximized = ref(false);
let unlisten: (() => void) | null = null;

async function sync() {
  maximized.value = await isWindowMaximized();
}

onMounted(async () => {
  if (!isTauri) return;
  await sync();
  unlisten = await onWindowResized(sync);
});

onUnmounted(() => unlisten?.());
</script>

<template>
  <div
    class="flex h-9 flex-none items-center gap-2 border-b border-base-300 bg-base-200 pr-1.5 pl-3 select-none"
    data-tauri-drag-region
  >
    <span class="flex items-center gap-2" data-tauri-drag-region>
      <span class="flex size-5 items-center justify-center rounded-selector bg-primary/15 text-primary">
        <Icon name="sparkle" :size="12" />
      </span>
      <span class="text-xs font-medium tracking-[0.2em] text-base-content/70">化境</span>
    </span>

    <!-- 拖拽区（双击 = 最大化/还原） -->
    <span class="h-full flex-1" data-tauri-drag-region @dblclick="toggleMaximizeWindow"></span>

    <div v-if="isTauri" class="flex items-center gap-0.5">
      <button class="btn btn-square btn-ghost btn-xs" aria-label="最小化" @click="minimizeWindow">
        <Icon name="minimize" :size="14" />
      </button>
      <button
        class="btn btn-square btn-ghost btn-xs"
        :aria-label="maximized ? '还原' : '最大化'"
        @click="toggleMaximizeWindow"
      >
        <Icon :name="maximized ? 'restore' : 'maximize'" :size="13" />
      </button>
      <button
        class="btn btn-square btn-ghost btn-xs hover:bg-error hover:text-error-content"
        aria-label="关闭"
        @click="closeWindow"
      >
        <Icon name="close" :size="15" />
      </button>
    </div>
  </div>
</template>
