<script setup lang="ts">
// 心理面板（M2.8 · 设计 §9）：内心摘要 + 情绪槽（强度/来源）+ 意图 + 衰减轨迹
import type { InspectorPsyche } from "../../types";
import { fixed2, pct } from "./util";

defineProps<{ psyche: InspectorPsyche }>();

/** 迷你柱高度：0 也留一线，别让「已经衰减到很小」看起来像没有 */
function barHeight(intensity: number): string {
  return `${Math.max(6, pct(intensity))}%`;
}

/** 轨迹是否空（空态判断） */
function hasTrail(ticks: unknown[]): boolean {
  return ticks.length > 0;
}
</script>

<template>
  <p
    v-if="!psyche.summary && !psyche.affects.length && !psyche.intents.length"
    class="m-0 text-xs text-base-content/50"
  >
    还没有心理活动记录（卡片没用 api.psyche，或还没聊到有情绪的一轮）。
  </p>

  <template v-else>
    <section class="flex flex-col gap-1.5">
      <div class="flex items-center justify-between gap-2">
        <p class="m-0 text-xs text-base-content/50">内心摘要（B5 注入的一行）</p>
        <span v-if="psyche.auto_emotion" class="badge badge-xs badge-soft badge-secondary">
          自动表情 · {{ psyche.auto_emotion }}
        </span>
      </div>
      <p
        v-if="psyche.summary"
        class="m-0 rounded-box border border-base-300 bg-base-200 px-2.5 py-2 font-mono text-xs leading-relaxed break-words"
      >
        {{ psyche.summary }}
      </p>
      <p v-else class="m-0 text-xs text-base-content/50">内心此刻是空的。</p>
    </section>

    <section class="flex flex-col gap-2">
      <p class="m-0 text-xs text-base-content/50">情绪槽（{{ psyche.affects.length }}）</p>
      <div v-for="a in psyche.affects" :key="a.name" class="rounded-box flex flex-col gap-1.5 bg-base-200 p-3">
        <div class="flex items-center gap-2">
          <span class="min-w-0 flex-1 truncate text-sm font-medium">{{ a.name }}</span>
          <span class="flex-none font-mono text-xs text-base-content/60">{{ fixed2(a.intensity) }}</span>
        </div>
        <progress class="progress progress-secondary h-1.5 w-full" :value="pct(a.intensity)" max="100"></progress>
        <p class="m-0 text-[11px] break-words text-base-content/50">
          来源：{{ a.source || "（未记来源）" }} · 起于第 {{ a.since_turn }} 轮
        </p>
        <ul v-if="a.history.length" class="m-0 flex list-none flex-wrap gap-1 p-0">
          <li
            v-for="h in a.history"
            :key="h.turn"
            class="badge badge-xs badge-ghost font-mono"
            :title="`第 ${h.turn} 轮的采样`"
          >
            第{{ h.turn }}轮 {{ fixed2(h.intensity) }}
          </li>
        </ul>
      </div>
      <p v-if="!psyche.affects.length" class="m-0 text-xs text-base-content/50">情绪槽是空的。</p>
    </section>

    <section class="flex flex-col gap-2">
      <p class="m-0 text-xs text-base-content/50">意图（{{ psyche.intents.length }}）</p>
      <div v-for="it in psyche.intents" :key="it.name" class="rounded-box flex flex-col gap-1.5 bg-base-200 p-3">
        <div class="flex items-center gap-2">
          <span class="min-w-0 flex-1 truncate text-sm font-medium">{{ it.name }}</span>
          <span class="flex-none font-mono text-xs text-base-content/60">{{ fixed2(it.strength) }}</span>
        </div>
        <progress class="progress progress-accent h-1.5 w-full" :value="pct(it.strength)" max="100"></progress>
        <p class="m-0 flex flex-wrap items-center gap-1.5 text-[11px] text-base-content/50">
          <span>起于第 {{ it.since_turn }} 轮</span>
          <span v-if="it.linked_thread" class="badge badge-xs badge-soft badge-primary font-mono">
            绑定线 {{ it.linked_thread }}
          </span>
          <span v-else>未外化为剧情线</span>
        </p>
      </div>
      <p v-if="!psyche.intents.length" class="m-0 text-xs text-base-content/50">没有在意的意图。</p>
    </section>

    <section class="flex flex-col gap-2">
      <p class="m-0 text-xs text-base-content/50">衰减轨迹（最近的采样，左旧右新）</p>
      <div v-for="[name, ticks] in psyche.trail" :key="name" class="flex items-center gap-2">
        <span class="w-16 flex-none truncate text-xs" :title="name">{{ name }}</span>
        <div v-if="hasTrail(ticks)" class="flex h-6 min-w-0 flex-1 items-end gap-0.5">
          <span
            v-for="(tick, i) in ticks"
            :key="tick.turn + '-' + i"
            class="min-h-[2px] w-1.5 flex-none rounded-xs bg-primary/60"
            :style="{ height: barHeight(tick.intensity) }"
            :title="`第 ${tick.turn} 轮 · ${fixed2(tick.intensity)}`"
          ></span>
        </div>
        <span v-else class="m-0 flex-1 text-[11px] text-base-content/40">还没有采样</span>
        <span class="flex-none font-mono text-[10px] text-base-content/40">
          {{ ticks.length ? fixed2(ticks[ticks.length - 1].intensity) : "-" }}
        </span>
      </div>
      <p v-if="!psyche.trail.length" class="m-0 text-xs text-base-content/50">
        还没有轨迹——情绪至少跨过一轮才会有采样。
      </p>
    </section>
  </template>
</template>
