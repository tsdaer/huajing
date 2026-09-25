<script setup lang="ts">
import { computed } from "vue";
import Icon from "../Icon.vue";
import { kindLabel } from "./kinds";
import type { InspTab } from "./tabs";
import type { HookReport, MemRecord, TimelineEntry } from "../../types";

// 卡内可观测面（M1.6）：角色私有 state、长期记忆写入流、类型化事件流 + 界面事件。
// 纯展示组件：数据进、刷新动作出；数据由会话视图（聊天流）与检查器抽屉提供。

const props = defineProps<{
  /** 当前页签（抽屉的 tablist 决定；这里只渲染卡内状态/记忆/事件流三个页签） */
  tab: InspTab;
  cardState: Record<string, unknown>;
  memory: MemRecord[];
  lastReport: HookReport | null;
  hookEvents: { kind: string; value: string; turn: number }[];
  hookLogs: string[];
  timeline: TimelineEntry[] | null;
  timelineLoading: boolean;
  timelineError: string;
}>();

const emit = defineEmits<{
  refreshCard: [];
  refreshTimeline: [];
  clearEvents: [];
}>();

/** 卡内状态行（顶层键值；嵌套对象折叠成 JSON 单行） */
const stateRows = computed(() =>
  Object.entries(props.cardState).map(([k, v]) => ({
    key: k,
    value: typeof v === "object" && v !== null ? JSON.stringify(v) : String(v),
  })),
);

/** 记忆流按时间倒序（最新在前） */
const memoryRows = computed(() => [...props.memory].reverse());

function fmtTs(ts: number): string {
  if (!ts) return "";
  const d = new Date(ts * 1000);
  return `${d.getMonth() + 1}/${d.getDate()} ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

function fmtValue(v: unknown): string {
  if (v === null || v === undefined) return "nil";
  return typeof v === "object" ? JSON.stringify(v) : String(v);
}
</script>

<template>
  <!-- 卡内状态：角色私有 state（state.json，hook 每轮维护） -->
  <template v-if="tab === 'state'">
    <div class="flex items-center justify-between gap-2">
      <p class="m-0 text-xs text-base-content/50">
        卡片私有状态（state.json）· 重启不丢
      </p>
      <button class="btn btn-ghost btn-xs flex-none" @click="emit('refreshCard')">
        <Icon name="refresh" :size="13" />刷新
      </button>
    </div>
    <dl v-if="stateRows.length" class="m-0 flex flex-col gap-2">
      <div
        v-for="row in stateRows"
        :key="row.key"
        class="rounded-box flex items-center justify-between gap-3 bg-base-200 px-3 py-2"
      >
        <dt class="font-mono text-xs text-base-content/60">{{ row.key }}</dt>
        <dd class="m-0 truncate text-sm font-medium" :title="row.value">{{ row.value }}</dd>
      </div>
    </dl>
    <p v-else class="m-0 text-xs text-base-content/50">这张卡没有 state（静态卡）。</p>
    <p v-if="lastReport && !lastReport.ran" class="m-0 text-[11px] text-base-content/40">
      卡片未定义 on_message 钩子，状态不随对话变化。
    </p>
  </template>

  <!-- 卡内记忆：api.memory.set 的写入流（palace.jsonl） -->
  <template v-else-if="tab === 'memory'">
    <div class="flex items-center justify-between gap-2">
      <p class="m-0 text-xs text-base-content/50">
        卡内长期记忆写入（palace.jsonl）· 召回在 M2 接记忆宫殿
      </p>
      <button class="btn btn-ghost btn-xs flex-none" @click="emit('refreshCard')">
        <Icon name="refresh" :size="13" />刷新
      </button>
    </div>
    <ul v-if="memoryRows.length" class="m-0 flex list-none flex-col gap-2 p-0">
      <li
        v-for="(rec, i) in memoryRows"
        :key="`${rec.ts}-${rec.key}-${i}`"
        class="rounded-box flex items-center justify-between gap-3 bg-base-200 px-3 py-2"
      >
        <div class="min-w-0">
          <p class="m-0 truncate font-mono text-xs">{{ rec.key }}</p>
          <p class="m-0 text-[11px] text-base-content/45">
            第 {{ rec.turn }} 轮 · {{ rec.source }} · {{ fmtTs(rec.ts) }}
          </p>
        </div>
        <span class="badge badge-sm badge-soft flex-none">{{ fmtValue(rec.value) }}</span>
      </li>
    </ul>
    <p v-else class="m-0 text-xs text-base-content/50">还没有记忆写入。</p>
  </template>

  <!-- 事件流：类型化事件流（session_timeline）+ 界面事件（api.ui.emit）+ 卡内错误 -->
  <template v-else>
    <div class="flex items-center justify-between gap-2">
      <p class="m-0 text-xs text-base-content/50">
        类型化事件流（messages.jsonl · 最新在前）
      </p>
      <button class="btn btn-ghost btn-xs flex-none" :disabled="timelineLoading" @click="emit('refreshTimeline')">
        <span v-if="timelineLoading" class="loading loading-spinner loading-xs"></span>
        <Icon v-else name="refresh" :size="13" />刷新
      </button>
    </div>
    <div v-if="timelineError" role="alert" class="alert alert-error alert-soft py-2 text-xs break-words">
      {{ timelineError }}
    </div>
    <p v-else-if="!timeline" class="m-0 text-xs text-base-content/50">读取中…</p>
    <ul v-else-if="timeline.length" class="m-0 flex list-none flex-col gap-1 p-0">
      <li
        v-for="e in timeline"
        :key="e.seq"
        class="rounded-box flex items-center gap-2 bg-base-200 px-2.5 py-1.5 text-xs"
      >
        <span class="badge badge-xs badge-soft flex-none font-mono">{{ e.kind }}</span>
        <span class="min-w-0 flex-1 break-words">{{ e.brief }}</span>
        <span class="ml-auto flex-none font-mono text-[10px] text-base-content/35">
          #{{ e.seq }} · 第 {{ e.turn }} 轮
        </span>
      </li>
    </ul>
    <p v-else class="m-0 text-xs text-base-content/50">事件流还是空的。</p>

    <div class="collapse collapse-arrow rounded-box bg-base-200">
      <input type="checkbox" />
      <div class="collapse-title min-h-0 px-3 py-2 text-xs">
        界面事件（api.ui.emit · {{ hookEvents.length }}）
      </div>
      <div class="collapse-content px-3">
        <div class="mb-1 flex justify-end">
          <button class="btn btn-ghost btn-xs" @click="emit('clearEvents')">清空</button>
        </div>
        <ul v-if="hookEvents.length" class="m-0 flex list-none flex-col gap-1.5 p-0">
          <li
            v-for="(ev, i) in hookEvents"
            :key="`${ev.turn}-${ev.kind}-${ev.value}-${i}`"
            class="rounded-box flex items-center gap-2 bg-base-100 px-2.5 py-1.5 text-xs"
          >
            <span class="badge badge-xs badge-soft badge-primary">{{ kindLabel(ev.kind) }}</span>
            <span class="truncate">{{ ev.value }}</span>
            <span class="ml-auto flex-none text-[11px] text-base-content/40">第 {{ ev.turn }} 轮</span>
          </li>
        </ul>
        <p v-else class="m-0 text-xs text-base-content/50">还没有界面事件。</p>
      </div>
    </div>

    <template v-if="hookLogs.length">
      <p class="m-0 mt-1 text-xs text-base-content/50">卡内错误（不打断对话）</p>
      <div
        v-for="(log, i) in hookLogs"
        :key="i"
        class="alert alert-error alert-soft py-2 text-xs whitespace-pre-wrap"
      >
        {{ log }}
      </div>
    </template>
  </template>
</template>
