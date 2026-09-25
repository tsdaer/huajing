<script setup lang="ts">
// 记忆条目的统一画法（房间图 / 时间线 / 最近记忆共用）
// 点击 → 发 jump(turn)：跳回产生这条记忆的原文轮次（M3.0 ② 溯源跳转）
import Icon from "../Icon.vue";
import type { InspectorMemory } from "../../types";
import { fixed2, pct, storyStamp } from "./util";

withDefaults(defineProps<{ m: InspectorMemory; dense?: boolean }>(), { dense: false });
const emit = defineEmits<{ jump: [turn: number] }>();
</script>

<template>
  <li
    class="rounded-box flex cursor-pointer flex-col gap-1 bg-base-100 px-2 py-1.5 transition-colors hover:bg-base-200/80 focus-visible:bg-base-200/80"
    role="button"
    tabindex="0"
    :title="`跳到第 ${m.turn} 轮原文`"
    @click="emit('jump', m.turn)"
    @keydown.enter="emit('jump', m.turn)"
  >
    <p class="m-0 text-left text-xs leading-relaxed break-words">{{ m.content }}</p>
    <div class="flex flex-wrap items-center gap-x-1.5 gap-y-1 text-[10px] text-base-content/45">
      <span class="badge badge-xs badge-ghost gap-0.5 font-mono">
        第 {{ m.turn }} 轮<Icon name="undo" :size="10" class="opacity-60" />
      </span>
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
