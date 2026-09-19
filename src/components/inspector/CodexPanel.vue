<script setup lang="ts">
// 设定集面板（M2.8 · 设计 §6）：世界名 + 实体清单（类型/生命周期/one-liner/anchors）+ 揭示集
import type { InspectorCodex } from "../../types";
import { statusClass, statusLabel } from "./util";

defineProps<{ codex: InspectorCodex; known: string[]; activeEntities: string[] }>();
</script>

<template>
  <section class="flex flex-col gap-1.5">
    <div class="flex items-center gap-2">
      <span class="min-w-0 flex-1 truncate text-sm font-semibold">
        {{ codex.world || "默认世界" }}
      </span>
      <span class="badge badge-sm badge-soft font-mono">{{ codex.count }} 个实体</span>
    </div>
    <div v-if="activeEntities.length" class="flex flex-wrap items-center gap-1">
      <span class="text-[11px] text-base-content/45">上轮激活</span>
      <span v-for="id in activeEntities" :key="id" class="badge badge-xs badge-ghost font-mono">{{ id }}</span>
    </div>
  </section>

  <section class="flex flex-col gap-2">
    <p class="m-0 text-xs text-base-content/50">实体（草稿不进注入，只有正史参与激活）</p>
    <div v-for="e in codex.entities" :key="e.id" class="rounded-box flex flex-col gap-1 bg-base-200 p-2.5">
      <div class="flex flex-wrap items-center gap-1.5">
        <span class="min-w-0 flex-1 truncate text-sm font-medium">{{ e.name }}</span>
        <span class="badge badge-xs badge-soft badge-neutral">{{ e.type }}</span>
        <span class="badge badge-xs" :class="statusClass(e.status)">{{ statusLabel(e.status) }}</span>
      </div>
      <p v-if="e.oneLiner" class="m-0 text-[11px] break-words text-base-content/60">{{ e.oneLiner }}</p>
      <p v-if="e.anchors.length" class="m-0 text-[10px] break-words text-base-content/40">
        anchors：{{ e.anchors.join(" · ") }}
      </p>
      <p class="m-0 font-mono text-[10px] text-base-content/35">{{ e.id }}</p>
    </div>
    <p v-if="!codex.entities.length" class="m-0 text-xs text-base-content/50">
      这个世界还没有实体——导入世界书或让总结管线提案之后就有了。
    </p>
  </section>

  <section class="flex flex-col gap-1.5">
    <p class="m-0 text-xs text-base-content/50">揭示集（角色已经知道的秘密）</p>
    <div v-if="known.length" class="flex flex-wrap gap-1">
      <span v-for="k in known" :key="k" class="badge badge-sm badge-soft badge-secondary font-mono">{{ k }}</span>
    </div>
    <p v-else class="m-0 text-xs text-base-content/50">还没有揭示的秘密。</p>
  </section>
</template>
