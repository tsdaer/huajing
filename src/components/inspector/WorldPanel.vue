<script setup lang="ts">
// 世界面板（M3.7 · 设计 §6.6）：世界主线阶段、世界时钟与跨会话回写、世界级线。
// 世界是会话的母层：这里看到的就是 B1 时代行与 B2 世界段注入的同一份数据。
import { computed, ref } from "vue";
import type { WorldlineView } from "../../types";

const props = defineProps<{ world: WorldlineView; busy?: boolean }>();
const emit = defineEmits<{ refresh: []; calibrate: [day: number] }>();

const dayInput = ref("");

const threadStateLabel: Record<string, string> = {
  active: "进行中",
  resolved: "已了结",
  abandoned: "已搁置",
  declared: "大势将至（声明未开）",
};

const stageChain = computed(() => props.world.path.join(" → "));

function calibrate() {
  const day = Number(dayInput.value);
  if (Number.isFinite(day) && day >= 1) {
    emit("calibrate", Math.floor(day));
    dayInput.value = "";
  }
}
</script>

<template>
  <!-- 无主线：可选层缺席，世界时钟仍在走 -->
  <section v-if="!world.configured" class="flex flex-col gap-1.5">
    <p class="m-0 text-xs text-base-content/50">
      这个世界还没有主线（codex/&lt;世界&gt;/worldline.lua 可选）——纯日常照常运转，
      世界时钟照样跨会话持久。
    </p>
  </section>

  <template v-else>
    <section class="rounded-box flex flex-col gap-1 bg-base-200 p-2.5">
      <div class="flex flex-wrap items-center gap-1.5">
        <span class="badge badge-xs badge-soft badge-accent font-mono">世界主线</span>
        <span class="min-w-0 flex-1 truncate text-sm font-semibold">{{ world.id }}</span>
      </div>
      <p v-if="world.premise" class="m-0 text-[11px] break-words text-base-content/60">
        {{ world.premise }}
      </p>
      <p class="m-0 text-xs">
        当前阶段：<span class="font-semibold">{{ world.stage }}</span>
        <span v-if="stageChain" class="ml-1 font-mono text-[10px] text-base-content/40">{{ stageChain }}</span>
      </p>
      <p v-if="world.stage_directive" class="m-0 text-[11px] break-words text-base-content/60">
        {{ world.stage_directive }}
      </p>
      <p v-if="world.era" class="m-0 text-[10px] break-words text-base-content/40">
        B1 时代行：{{ world.era }}
      </p>
    </section>
  </template>

  <section class="rounded-box flex flex-col gap-1.5 bg-base-200 p-2.5">
    <div class="flex items-center justify-between gap-2">
      <span class="text-xs text-base-content/50">世界时钟（跨会话 max 回写）</span>
      <span class="font-mono text-sm font-semibold">第 {{ world.world_day }} 天</span>
    </div>
    <p class="m-0 text-[11px] text-base-content/45">
      本会话故事时钟：第 {{ world.session_day }} 天 ·
      {{ world.updated_by ? `最近推进：${world.updated_by}` : "尚无回写记录" }}
    </p>
    <div class="flex items-center gap-1.5">
      <input
        v-model="dayInput"
        type="number"
        min="1"
        placeholder="第 N 天"
        class="input input-xs input-bordered w-20 font-mono"
      />
      <button class="btn btn-ghost btn-xs" :disabled="busy" @click="calibrate">
        校准
      </button>
      <span class="text-[10px] text-base-content/35">flashback 布景/纠偏用；不会影响本会话</span>
    </div>
  </section>

  <section class="flex flex-col gap-1.5">
    <p class="m-0 text-xs text-base-content/50">世界级线（scope=world · 任何会话可推进）</p>
    <div v-for="t in world.world_threads" :key="String(t.id)" class="rounded-box flex flex-col gap-0.5 bg-base-200 p-2.5">
      <div class="flex flex-wrap items-center gap-1.5">
        <span class="min-w-0 flex-1 truncate text-sm font-medium">{{ t.title ?? t.id }}</span>
        <span class="badge badge-xs badge-soft badge-secondary">
          {{ threadStateLabel[String(t.state)] ?? String(t.state) }}
        </span>
      </div>
      <p v-if="t.cause" class="m-0 text-[11px] break-words text-base-content/60">{{ t.cause }}</p>
      <p class="m-0 font-mono text-[10px] text-base-content/35">{{ t.id }}</p>
    </div>
    <p v-if="!world.world_threads.length" class="m-0 text-xs text-base-content/50">
      还没有世界级线——主线阶段推进时可能开出一条压着整个世界的线。
    </p>
  </section>
</template>
