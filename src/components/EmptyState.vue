<script setup lang="ts">
import Icon from "./Icon.vue";

// 统一空状态：虚线卡 + 图标 + 标题 + 说明 + 可选动作（slot）。
// 全应用的「还没有数据」都走这一份画法，替代散落各处的纯文本空状态。
withDefaults(defineProps<{ icon?: string; title: string; desc?: string }>(), { icon: "sparkle" });
</script>

<template>
  <div class="card card-dash flex flex-col items-center justify-center gap-1.5 bg-base-100/50 p-8 text-center">
    <span class="flex size-11 items-center justify-center rounded-full bg-base-content/5">
      <Icon :name="icon" :size="22" class="text-base-content/30" />
    </span>
    <p class="m-0 text-sm text-base-content/55">{{ title }}</p>
    <p v-if="desc" class="m-0 max-w-[40ch] text-xs text-base-content/40">{{ desc }}</p>
    <div v-if="$slots.default" class="mt-2 flex items-center gap-2">
      <slot />
    </div>
  </div>
</template>
