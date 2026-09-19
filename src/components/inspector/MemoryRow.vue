<script setup lang="ts">
// 记忆条目的统一画法（房间图 / 时间线 / 最近记忆共用）
import type { InspectorMemory } from "../../types";
import { fixed2, pct, storyStamp } from "./util";

withDefaults(defineProps<{ m: InspectorMemory; dense?: boolean }>(), { dense: false });
</script>

<template>
  <li class="rounded-box flex flex-col gap-1 bg-base-100 px-2 py-1.5">
    <p class="m-0 text-xs leading-relaxed break-words">{{ m.content }}</p>
    <div class="flex flex-wrap items-center gap-x-1.5 gap-y-1 text-[10px] text-base-content/45">
      <span class="badge badge-xs badge-ghost font-mono">第 {{ m.turn }} 轮</span>
      <span>{{ storyStamp(m) }}</span>
      <span v-if="m.place" class="truncate">· {{ m.place }}</span>
      <span v-if="m.emotion" class="badge badge-xs badge-soft badge-secondary">{{ m.emotion }}</span>
      <span v-if="!dense" class="font-mono">显著度 {{ fixed2(m.salience) }}</span>
      <span v-if="!dense" class="ml-auto truncate font-mono text-base-content/35">{{ m.source }}</span>
    </div>
    <progress
      v-if="!dense"
      class="progress progress-accent h-0.5 w-full"
      :value="pct(m.salience)"
      max="100"
    ></progress>
  </li>
</template>
