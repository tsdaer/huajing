<script setup lang="ts">
// 宫殿面板（M2.8 · 设计 §5.5）：房间图 / 时间线 / 关联图 + 最近记忆（每条可溯源轮次）
// M4.4 睡眠整理：「已归档」过滤看被合并稿替代的旧记忆（可溯源）+「睡眠整理」按钮
//（两段式确认——归档不复活是单向操作，按钮先变「确认」再执行）
import { computed, ref } from "vue";
import Icon from "../Icon.vue";
import type { ConsolidateReport, InspectorPalace } from "../../types";
import { api } from "../../api";
import MemoryRow from "./MemoryRow.vue";

const props = defineProps<{ palace: InspectorPalace; sessionId: string }>();
const emit = defineEmits<{ jump: [turn: number]; refresh: [] }>();

const view = ref<"rooms" | "timeline" | "graph">("rooms");
const VIEWS = [
  { id: "rooms", label: "房间图" },
  { id: "timeline", label: "时间线" },
  { id: "graph", label: "关联图" },
] as const;

// ---------- 已归档过滤（决断 5：归档不复活，但全量保留可溯源） ----------
// 计数用后端的全量字段；列表仍取「最近记忆」窗口（全量溯源走事件流页签）
const showArchived = ref(false);
const archivedCount = computed(() => props.palace.archivedCount ?? props.palace.recent.filter((m) => m.archived).length);
const visibleRecent = computed(() =>
  props.palace.recent.filter((m) => (showArchived.value ? m.archived : !m.archived)),
);

// ---------- 睡眠整理（手动；确认后执行——未配 util 档时后端给可读提示且零改动） ----------
const arming = ref(false);
const running = ref(false);
const armTimer = ref<number | undefined>(undefined);
const result = ref<{ ok: boolean; text: string } | null>(null);

function armConsolidate() {
  if (arming.value) {
    void doConsolidate();
    return;
  }
  arming.value = true;
  armTimer.value = window.setTimeout(() => (arming.value = false), 5000);
}

async function doConsolidate() {
  window.clearTimeout(armTimer.value);
  arming.value = false;
  running.value = true;
  result.value = null;
  try {
    const report: ConsolidateReport = await api.consolidateNow(props.sessionId, () => {});
    if (report.groups === 0) {
      result.value = { ok: true, text: "没有可整理的低显著度相似记忆——宫殿已经够干净了。" };
    } else if (report.merged === 0) {
      result.value = { ok: false, text: `分组 ${report.groups} 组但全部放弃：${report.skipped.join("；")}` };
    } else {
      result.value = {
        ok: true,
        text: `整理完成：${report.merged}/${report.groups} 组落了合并稿，${report.archived} 条旧记忆已归档（jsonl 全量保留，点「已归档」可溯源）。`,
      };
    }
    emit("refresh"); // 整理落盘后重拉检查器数据——面板不能一直显示整理前的旧投影
  } catch (e) {
    result.value = { ok: false, text: String(e) };
  } finally {
    running.value = false;
  }
}
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
          <MemoryRow v-for="m in room.top" :key="m.id" :m="m" dense @jump="emit('jump', $event)" />
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
          <MemoryRow v-for="m in bucket.top" :key="m.id" :m="m" dense @jump="emit('jump', $event)" />
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

    <!-- 最近记忆：现行 / 已归档 过滤 + 内容 + 故事时刻 + 显著度 + 情绪 + 溯源轮次 -->
    <section class="flex flex-col gap-1.5">
      <div class="flex items-center gap-2">
        <p class="m-0 flex-1 text-xs text-base-content/50">
          {{ showArchived ? `已归档（${archivedCount} 条，可溯源不参与召回）` : `最近记忆（${visibleRecent.length} 条，新在前）` }}
        </p>
        <div class="join">
          <button class="btn join-item btn-xs" :class="{ 'btn-active': !showArchived }" @click="showArchived = false">
            现行
          </button>
          <button
            class="btn join-item btn-xs"
            :class="{ 'btn-active': showArchived }"
            :disabled="archivedCount === 0"
            @click="showArchived = true"
          >
            已归档{{ archivedCount ? ` ${archivedCount}` : "" }}
          </button>
        </div>
      </div>
      <ul v-if="visibleRecent.length" class="m-0 flex list-none flex-col gap-1.5 p-0">
        <MemoryRow v-for="m in visibleRecent" :key="m.id" :m="m" @jump="emit('jump', $event)" />
      </ul>
      <p v-else-if="showArchived" class="m-0 text-xs text-base-content/50">还没有归档的记忆——跑一次睡眠整理就有了。</p>
      <p v-else class="m-0 text-xs text-base-content/50">还没有记忆对象。</p>
    </section>

    <!-- 睡眠整理：低显著相似记忆合并成摘要记忆，原记忆归档退出召回（决断 5：归档不复活） -->
    <section class="flex flex-col gap-1.5 rounded-box bg-base-200 p-3">
      <div class="flex items-center gap-2">
        <p class="m-0 flex-1 text-[11px] leading-relaxed text-base-content/55">
          睡眠整理把低显著度的相似记忆合并成一段摘要记忆（进场景卷摘要），原记忆归档退出召回——
          <span class="text-warning">归档不复活</span>，但全量保留可随时溯源。
        </p>
        <button
          class="btn btn-sm flex-none"
          :class="arming ? 'btn-warning' : ''"
          :disabled="running"
          :title="arming ? '再点一次确认执行' : '低显著相似记忆 → 合并稿 + 归档'"
          @click="armConsolidate"
        >
          <span v-if="running" class="loading loading-spinner loading-xs"></span>
          <Icon v-else name="moon" :size="13" />
          {{ running ? "整理中…" : arming ? "确认整理？" : "睡眠整理" }}
        </button>
      </div>
      <p v-if="result" class="m-0 text-[11px]" :class="result.ok ? 'text-success' : 'text-error'">
        {{ result.text }}
      </p>
    </section>
  </template>
</template>
