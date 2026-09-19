<script setup lang="ts">
// 宫殿面板（M2.8 · 设计 §5.5）：房间图 / 时间线 / 关联图 + 最近记忆（每条可溯源轮次）
import { ref } from "vue";
import type { InspectorPalace } from "../../types";
import MemoryRow from "./MemoryRow.vue";

defineProps<{ palace: InspectorPalace }>();

const view = ref<"rooms" | "timeline" | "graph">("rooms");
const VIEWS = [
  { id: "rooms", label: "房间图" },
  { id: "timeline", label: "时间线" },
  { id: "graph", label: "关联图" },
] as const;
</script>

<template>
  <p v-if="palace.count === 0" class="m-0 text-xs text-base-content/50">
    宫殿还是空的——聊过几轮、跑过一次总结之后，这里才有她记得的事。
  </p>

  <template v-else>
    <div class="grid grid-cols-3 gap-2">
      <div class="rounded-box bg-base-200 px-2.5 py-2">
        <p class="m-0 text-[11px] text-base-content/45">记忆</p>
        <p class="m-0 text-lg leading-tight font-semibold">{{ palace.count }}</p>
      </div>
      <div class="rounded-box bg-base-200 px-2.5 py-2">
        <p class="m-0 text-[11px] text-base-content/45">房间</p>
        <p class="m-0 text-lg leading-tight font-semibold">{{ palace.rooms.length }}</p>
      </div>
      <div class="rounded-box bg-base-200 px-2.5 py-2">
        <p class="m-0 text-[11px] text-base-content/45">关联节点</p>
        <p class="m-0 text-lg leading-tight font-semibold">{{ palace.graph.nodes.length }}</p>
      </div>
    </div>

    <!-- 三视图：房间图 / 时间线 / 关联图（列表 + badge，够看即可） -->
    <div role="tablist" class="tabs tabs-box tabs-xs">
      <button
        v-for="v in VIEWS"
        :key="v.id"
        role="tab"
        class="tab"
        :class="{ 'tab-active': view === v.id }"
        @click="view = v.id"
      >
        {{ v.label }}
      </button>
    </div>

    <!-- 房间图：一个故事地点 = 一间房 -->
    <section v-if="view === 'rooms'" class="flex flex-col gap-2">
      <div v-for="room in palace.rooms" :key="room.place" class="flex flex-col gap-1.5">
        <div class="flex items-center gap-2">
          <span class="min-w-0 flex-1 truncate text-xs font-medium">{{ room.place }}</span>
          <span class="badge badge-xs badge-soft font-mono">{{ room.count }} 条</span>
        </div>
        <ul class="m-0 flex list-none flex-col gap-1 p-0">
          <MemoryRow v-for="m in room.top" :key="m.id" :m="m" dense />
        </ul>
      </div>
      <p v-if="!palace.rooms.length" class="m-0 text-xs text-base-content/50">
        还没有带地点的记忆，房间图是空的（时间线里仍然查得到）。
      </p>
    </section>

    <!-- 时间线走廊：按故事天分桶 -->
    <section v-else-if="view === 'timeline'" class="flex flex-col gap-2">
      <div v-for="bucket in palace.timeline" :key="bucket.label" class="flex flex-col gap-1.5">
        <div class="flex items-center gap-2">
          <span class="flex-1 text-xs font-medium">{{ bucket.label }}</span>
          <span class="badge badge-xs badge-soft font-mono">{{ bucket.count }} 条</span>
        </div>
        <ul class="m-0 flex list-none flex-col gap-1 p-0">
          <MemoryRow v-for="m in bucket.top" :key="m.id" :m="m" dense />
        </ul>
      </div>
      <p v-if="!palace.timeline.length" class="m-0 text-xs text-base-content/50">时间线上还没有记忆。</p>
    </section>

    <!-- 关联图：节点是 link 标签，边是同一条记忆里的共现 -->
    <section v-else class="flex flex-col gap-2">
      <div v-if="palace.graph.nodes.length" class="flex flex-wrap gap-1">
        <span v-for="n in palace.graph.nodes" :key="n" class="badge badge-sm badge-soft badge-primary">{{ n }}</span>
      </div>
      <p v-else class="m-0 text-xs text-base-content/50">还没有关联标签。</p>
      <div v-if="palace.graph.edges.length" class="flex flex-col gap-1">
        <p class="m-0 text-[11px] text-base-content/45">共现边（{{ palace.graph.edges.length }}）</p>
        <div
          v-for="(edge, i) in palace.graph.edges"
          :key="edge[0] + edge[1] + i"
          class="rounded-box flex items-center gap-2 bg-base-200 px-2.5 py-1.5 text-[11px]"
        >
          <span class="min-w-0 flex-1 truncate">{{ edge[0] }} — {{ edge[1] }}</span>
          <span class="badge badge-xs badge-ghost flex-none font-mono">×{{ edge[2] }}</span>
        </div>
      </div>
      <p v-else class="m-0 text-[11px] text-base-content/50">还没有共现边。</p>
    </section>

    <!-- 最近记忆：内容 + 故事时刻 + 显著度 + 情绪 + 溯源轮次 -->
    <section class="flex flex-col gap-1.5">
      <p class="m-0 text-xs text-base-content/50">最近记忆（{{ palace.recent.length }} 条，新在前）</p>
      <ul v-if="palace.recent.length" class="m-0 flex list-none flex-col gap-1.5 p-0">
        <MemoryRow v-for="m in palace.recent" :key="m.id" :m="m" />
      </ul>
      <p v-else class="m-0 text-xs text-base-content/50">还没有记忆对象。</p>
    </section>
  </template>
</template>
