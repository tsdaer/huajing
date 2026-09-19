<script setup lang="ts">
// 状态路径面板（M2.8 · 设计 §7.4）：路径条 + directive + recall/reveal + 校验告警 + 转移历史
import Icon from "../Icon.vue";
import type { InspectorStateTree, InspectorTransition } from "../../types";

defineProps<{ tree: InspectorStateTree | null; transitions: InspectorTransition[] }>();

/** 路径条的一行文字（转移历史里的 from/to） */
function pathLine(p: string[]): string {
  return p.length ? p.join(" → ") : "（无）";
}
</script>

<template>
  <p v-if="!tree" class="m-0 text-xs text-base-content/50">
    这张卡没有状态树（静态卡），状态路径一路为空。
  </p>

  <template v-else>
    <section class="flex flex-col gap-1.5">
      <p class="m-0 text-xs text-base-content/50">活跃路径（根 → 叶）</p>
      <div v-if="tree.path.length" class="flex flex-wrap items-center gap-1">
        <template v-for="(s, i) in tree.path" :key="s + i">
          <Icon v-if="i" name="chevron" :size="10" class="flex-none text-base-content/30" />
          <span
            class="badge badge-sm max-w-full truncate"
            :class="i === tree.path.length - 1 ? 'badge-primary' : 'badge-soft'"
            >{{ s }}</span
          >
        </template>
      </div>
      <p v-else class="m-0 text-xs text-base-content/50">
        路径为空（叶状态没在树上声明）。根：{{ tree.root || "（未声明）" }}
      </p>
      <p v-if="tree.states.length" class="m-0 text-[11px] text-base-content/40">
        树上共 {{ tree.states.length }} 个状态：{{ tree.states.join(" · ") }}
      </p>
    </section>

    <section class="flex flex-col gap-1.5">
      <p class="m-0 text-xs text-base-content/50">当前 directive（根→叶拼接）</p>
      <pre
        v-if="tree.directive"
        class="m-0 rounded-box border border-base-300 bg-base-200 p-2.5 font-mono text-xs leading-relaxed break-words whitespace-pre-wrap"
        >{{ tree.directive }}</pre
      >
      <p v-else class="m-0 text-xs text-base-content/50">这条路线上没有写 directive。</p>
    </section>

    <section class="flex flex-col gap-1.5">
      <p class="m-0 text-xs text-base-content/50">recall（召回提示）</p>
      <div v-if="tree.recall.length" class="flex flex-wrap gap-1">
        <span v-for="r in tree.recall" :key="r" class="badge badge-sm badge-soft badge-accent">{{ r }}</span>
      </div>
      <p v-else class="m-0 text-xs text-base-content/50">当前状态没有召回提示。</p>
    </section>

    <section class="flex flex-col gap-1.5">
      <p class="m-0 text-xs text-base-content/50">reveal（揭示集）</p>
      <div v-if="tree.reveal.length" class="flex flex-wrap gap-1">
        <span v-for="r in tree.reveal" :key="r" class="badge badge-sm badge-soft badge-secondary">{{ r }}</span>
      </div>
      <p v-else class="m-0 text-xs text-base-content/50">当前状态没有揭示集。</p>
    </section>

    <template v-if="tree.warnings.length">
      <div
        v-for="w in tree.warnings"
        :key="w"
        role="alert"
        class="alert alert-error alert-soft py-1.5 text-xs break-words"
      >
        {{ w }}
      </div>
    </template>

    <section class="flex flex-col gap-1.5">
      <p class="m-0 text-xs text-base-content/50">转移历史（最近 {{ transitions.length }} 条，新在前）</p>
      <ul v-if="transitions.length" class="m-0 flex list-none flex-col gap-1.5 p-0">
        <li
          v-for="(t, i) in transitions"
          :key="t.ts + '-' + i"
          class="rounded-box flex flex-col gap-1 bg-base-200 px-2.5 py-2"
        >
          <div class="flex items-center gap-1.5">
            <span class="badge badge-xs badge-ghost font-mono">第 {{ t.turn }} 轮</span>
            <span class="flex-1 truncate text-[11px] text-base-content/60" :title="pathLine(t.to)">
              {{ pathLine(t.from) }} → {{ pathLine(t.to) }}
            </span>
          </div>
          <p class="m-0 text-[11px] break-words text-base-content/50">{{ t.reason || "（未记原因）" }}</p>
        </li>
      </ul>
      <p v-else class="m-0 text-xs text-base-content/50">还没有转移记录（一直停在初始状态）。</p>
    </section>
  </template>
</template>
