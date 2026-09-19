<script setup lang="ts">
// 摘要与收件箱（M2.8 · 设计 §5.3 / §6.9）：滚动摘要 + 立即总结 + 提案确认/否决
import Icon from "../Icon.vue";
import type { InspectorProposal } from "../../types";
import { payloadBrief, proposalKind, proposalStatus } from "./util";

defineProps<{
  summary: string;
  proposals: InspectorProposal[];
  summarizing: boolean;
  result: string;
  deciding: string;
}>();

const emit = defineEmits<{ summarize: []; decide: [id: string, accept: boolean] }>();
</script>

<template>
  <section class="flex flex-col gap-1.5">
    <div class="flex items-center gap-2">
      <p class="m-0 flex-1 text-xs text-base-content/50">滚动摘要（L1 · 滑出窗口的旧剧情梗概）</p>
      <button class="btn btn-ghost btn-xs flex-none" :disabled="summarizing" @click="emit('summarize')">
        <span v-if="summarizing" class="loading loading-spinner loading-xs"></span>
        <Icon v-else name="bolt" :size="13" />
        {{ summarizing ? "总结中…" : "立即总结" }}
      </button>
    </div>
    <pre
      v-if="summary"
      class="m-0 max-h-72 overflow-y-auto rounded-box border border-base-300 bg-base-200 p-2.5 text-xs leading-relaxed break-words whitespace-pre-wrap"
      >{{ summary }}</pre
    >
    <p v-else class="m-0 text-xs text-base-content/50">
      还没有摘要——消息滑出 L0 窗口后会自动总结，也可以点「立即总结」。
    </p>
    <div v-if="result" role="alert" class="alert alert-success alert-soft py-1.5 text-xs break-words">
      {{ result }}
    </div>
  </section>

  <section class="flex flex-col gap-2">
    <div class="flex items-center justify-between gap-2">
      <p class="m-0 text-xs text-base-content/50">设定收件箱（LLM 产物先落草稿，确认后才进注入）</p>
      <span class="badge badge-xs badge-soft font-mono">{{ proposals.length }}</span>
    </div>

    <div v-for="p in proposals" :key="p.id" class="rounded-box flex flex-col gap-1.5 bg-base-200 p-2.5">
      <div class="flex flex-wrap items-center gap-1.5">
        <span class="badge badge-xs badge-soft badge-primary">{{ proposalKind(p.kind) }}</span>
        <span class="badge badge-xs badge-ghost font-mono">第 {{ p.turn }} 轮</span>
        <span
          class="badge badge-xs"
          :class="p.status === 'propose' ? 'badge-soft badge-warning' : p.status === 'accept' ? 'badge-soft badge-success' : 'badge-soft badge-neutral'"
        >
          {{ proposalStatus(p.status) }}
        </span>
      </div>
      <p v-if="payloadBrief(p.payload)" class="m-0 font-mono text-[11px] leading-relaxed break-words text-base-content/60">
        {{ payloadBrief(p.payload) }}
      </p>
      <p v-else class="m-0 text-[11px] text-base-content/40">（这条提案没有 payload）</p>
      <p v-if="p.note" class="m-0 text-[11px] break-words text-base-content/50">备注：{{ p.note }}</p>
      <div v-if="p.status === 'propose'" class="flex items-center justify-end gap-2">
        <button
          class="btn btn-xs btn-primary"
          :disabled="deciding === p.id"
          @click="emit('decide', p.id, true)"
        >
          <span v-if="deciding === p.id" class="loading loading-spinner loading-xs"></span>
          确认
        </button>
        <button class="btn btn-xs" :disabled="deciding === p.id" @click="emit('decide', p.id, false)">
          否决
        </button>
      </div>
    </div>

    <p v-if="!proposals.length" class="m-0 text-xs text-base-content/50">
      收件箱是空的——还没有等待确认的提案。
    </p>
  </section>
</template>
