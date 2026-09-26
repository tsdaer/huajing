<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { api } from "../api";
import Icon from "./Icon.vue";
import EmptyState from "./EmptyState.vue";
import CardStatePanel from "./inspector/CardStatePanel.vue";
import StatePathPanel from "./inspector/StatePathPanel.vue";
import ThreadsPanel from "./inspector/ThreadsPanel.vue";
import PsychePanel from "./inspector/PsychePanel.vue";
import PalacePanel from "./inspector/PalacePanel.vue";
import CodexPanel from "./inspector/CodexPanel.vue";
import SummaryPanel from "./inspector/SummaryPanel.vue";
import WorldPanel from "./inspector/WorldPanel.vue";
import { INSP_TABS, M2_TABS, type InspTab } from "./inspector/tabs";
import type {
  HookReport,
  InspectorData,
  MemRecord,
  PromptAssembly,
  TimelineEntry,
  WorldlineView,
} from "../types";

// 记忆检查器抽屉（M1.6 四个可观测面 + M2.8 六面板 + M3.4 导演 + M3.7 世界 + M3.11 视角）。
// 检查器自己的状态与动作（全量投影/事件流/世界主线/收件箱/手动开收线/总结）都收在这里；
// 聊天侧只接线：轮末刷新走 refreshAfterRound（defineExpose），跳转/预览/卡内刷新走事件。

const props = defineProps<{
  sessionId: string;
  /** 会话归属的世界名（世界时钟校准要写对这个世界，B7） */
  world?: string | null;
  /** 阵容（多角色时显示视角选择器） */
  speakers: string[];
  cardName: string;
  /** 注入层页签的数据源（「最近一次发送」或干跑预览；聊天侧持有） */
  assembly: PromptAssembly | null;
  assemblySource: "last" | "preview";
  /** 卡内可观测面（聊天流持有：钩子报告与刷新都从那边来） */
  cardState: Record<string, unknown>;
  memory: MemRecord[];
  lastReport: HookReport | null;
  hookEvents: { kind: string; value: string; turn: number }[];
  hookLogs: string[];
}>();

const emit = defineEmits<{
  /** 宫殿记忆溯源跳转：跳回产生这条记忆的原文轮次（翻页在会话视图做） */
  jump: [turn: number];
  /** 注入层「刷新」：聊天侧重跑一次干跑预览 */
  preview: [];
  /** 总结/开收线成功后：组装按视角重读（「最近一次发送」优先），会话视图接线 */
  refreshAssembly: [];
  /** 命令失败上报全局错误条 */
  error: [message: string];
  /** 卡内状态/记忆刷新（数据在聊天流侧） */
  refreshCard: [];
  /** 界面事件清空（数据在聊天流侧） */
  clearEvents: [];
}>();

/** 检查器视角（M3.11）：以谁的视角看（角色目录名；"" = 主角色），会话视图共用 */
const inspView = defineModel<string>("view", { default: "" });

/** 当前页签（会话视图持有：抽屉关闭再开不丢页签） */
const inspTab = defineModel<InspTab>("tab", { required: true });

const inspTabLabel = computed(() => INSP_TABS.find((t) => t.id === inspTab.value)?.label ?? "");

// 切到事件流/导演页签时按需拉一次类型化事件流（M3.0 ④ / M3.4）；世界页签拉世界主线（M3.7）
watch(inspTab, (tab) => {
  if (tab === "events" || tab === "director") ensureTimeline();
  if (tab === "world") ensureWorldline();
});

// ---------- 全量投影（M2.8 六面板共享） ----------

/** 记忆检查器全量投影（状态树 / 剧情线 / 心理 / 宫殿 / 设定集 / 摘要与收件箱） */
const inspector = ref<InspectorData | null>(null);
const inspLoading = ref(false);
const inspError = ref("");

/** 拉一次检查器数据；失败只在面板里提示，不打断聊天（浏览器 mock 未覆盖该命令时也走这里） */
async function loadInspector() {
  inspLoading.value = true;
  inspError.value = "";
  try {
    inspector.value = await api.inspectorData(props.sessionId, inspView.value || undefined);
  } catch (e) {
    inspError.value = String(e);
  } finally {
    inspLoading.value = false;
  }
}

/** 切视角：检查器重拉（注入层/卡内状态跟着换人，由会话视图接线） */
watch(inspView, () => {
  void loadInspector();
});

/** 打开抽屉/切到检查器页时拉一次；已经有数据就复用缓存，重拉交给「刷新」 */
function ensureInspector() {
  if (!inspector.value && !inspLoading.value) void loadInspector();
}

/** 预算账目里该层的裁剪说明（M2.7 · 设计 §4.2）；没被动过则 undefined */
function layerTrimmed(id: string, name: string): string | undefined {
  return props.assembly?.budget?.layers.find((u) => u.id === id && u.name === name)?.trimmed;
}

// ---------- 收件箱 / 总结 ----------

/** 正在确认/否决的提案 id */
const decidingId = ref("");

/** 收件箱：确认 / 否决一条提案，动作进事件流，成功后刷新全量视图 */
async function decideProposal(id: string, accept: boolean, note?: string) {
  decidingId.value = id;
  try {
    await api.decideProposal(props.sessionId, id, accept, note);
    await loadInspector();
  } catch (e) {
    emit("error", String(e));
  } finally {
    decidingId.value = "";
  }
}

/** 收件箱批量处理（M3.8 · DoD 8）：全部确认 / 全部否决 */
const decidingAll = ref(false);
async function decideAllProposals(accept: boolean) {
  decidingAll.value = true;
  try {
    await api.decideAllProposals(props.sessionId, accept);
    await loadInspector();
  } catch (e) {
    emit("error", String(e));
  } finally {
    decidingAll.value = false;
  }
}

/** 「立即总结」的进行态与结果（总结可能较慢） */
const summarizing = ref(false);
const summaryResult = ref("");

/** 手动触发一次总结（正常路径是消息滑出窗口后自动触发） */
async function summarizeNow() {
  if (summarizing.value) return;
  summarizing.value = true;
  summaryResult.value = "";
  emit("error", "");
  try {
    summaryResult.value = await api.summarizeNow(props.sessionId);
    await loadInspector();
    emit("refreshAssembly");
  } catch (e) {
    emit("error", String(e));
  } finally {
    summarizing.value = false;
  }
}

// ---------- 剧情线手动开/收线（M3.0 ① · 设计 §8.3） ----------

const threadBusy = ref(false);

async function openThread(draft: { title: string; cause: string; actors: string[]; importance: number }) {
  if (threadBusy.value) return;
  threadBusy.value = true;
  emit("error", "");
  try {
    await api.openThread(props.sessionId, draft.title, draft.cause, draft.actors, draft.importance);
    await loadInspector();
    emit("refreshAssembly"); // 线影响 B1/C1 组装
  } catch (e) {
    emit("error", String(e));
  } finally {
    threadBusy.value = false;
  }
}

async function resolveThreadCmd(id: string, outcome: string) {
  if (threadBusy.value) return;
  threadBusy.value = true;
  emit("error", "");
  try {
    await api.resolveThread(props.sessionId, id, outcome);
    await loadInspector();
    emit("refreshAssembly"); // 收线三件事都会改变组装（B1「了结未远」/ C1 清空/转移）
  } catch (e) {
    emit("error", String(e));
  } finally {
    threadBusy.value = false;
  }
}

// ---------- 类型化事件流（M3.0 ④）+ 导演面板（M3.4） ----------

const timeline = ref<TimelineEntry[] | null>(null);
const timelineLoading = ref(false);
const timelineError = ref("");

async function loadTimeline() {
  timelineLoading.value = true;
  timelineError.value = "";
  try {
    timeline.value = await api.sessionTimeline(props.sessionId);
  } catch (e) {
    timelineError.value = String(e);
  } finally {
    timelineLoading.value = false;
  }
}

/** 事件流页签展开时拉一次（聊天推进后想看新的，点「刷新」） */
function ensureTimeline() {
  if (timeline.value === null && !timelineLoading.value) void loadTimeline();
}

/** 导演面板的数据源：事件流里的调度史（发言权打分的依据逐条可查，DoD 1） */
const directorHistory = computed(() => (timeline.value ?? []).filter((e) => e.kind === "director"));

// ---------- 世界主线与世界时钟（M3.7 · 设计 §6.6） ----------

const worldline = ref<WorldlineView | null>(null);
const worldlineBusy = ref(false);
const worldlineError = ref("");

async function loadWorldline() {
  worldlineBusy.value = true;
  worldlineError.value = "";
  try {
    worldline.value = await api.worldlineView(props.sessionId);
  } catch (e) {
    worldlineError.value = String(e);
  } finally {
    worldlineBusy.value = false;
  }
}

function ensureWorldline() {
  if (!worldline.value && !worldlineBusy.value) void loadWorldline();
}

/** 手动校准世界时钟（flashback 布景 / 纠偏） */
async function calibrateWorld(day: number) {
  worldlineBusy.value = true;
  try {
    // B7：空 world 会被后端落到 "default"，而面板显示的是会话自己的世界——传对归属
    await api.worldSetClock(props.world ?? "", day);
    await loadWorldline();
  } catch (e) {
    worldlineError.value = String(e);
  } finally {
    worldlineBusy.value = false;
  }
}

// ---------- 会话切换与轮末刷新（聊天流收尾接线） ----------

/** 换会话：检查器/事件流/世界主线数据作废（下次打开抽屉再拉） */
function resetForSession() {
  inspector.value = null;
  inspError.value = "";
  summaryResult.value = "";
  timeline.value = null;
  timelineError.value = "";
  worldline.value = null;
  worldlineError.value = "";
}

/** 刚聊完一轮：状态树/剧情线/心理/宫殿多半都变了，抽屉开着就顺手重拉；
 *  事件流/导演页签在开时也重拉，新事件即时可见 */
function refreshAfterRound() {
  void loadInspector();
  if (inspTab.value === "events" || inspTab.value === "director") void loadTimeline();
}

defineExpose({ ensureInspector, resetForSession, refreshAfterRound });
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4">
    <!-- 检查器页签：M1.6 的四个可观测面 + M2.8 的六个记忆检查器面板 -->
    <div role="tablist" class="tabs tabs-border tabs-xs flex-none">
      <button
        v-for="t in INSP_TABS"
        :key="t.id"
        role="tab"
        class="tab gap-1"
        :class="{ 'tab-active': inspTab === t.id }"
        @click="inspTab = t.id"
      >
        <Icon :name="t.icon" :size="12" class="opacity-70" />
        {{ t.label }}
      </button>
    </div>

    <!-- M3.11 多角色检查器：以谁的视角看（状态路径/心理/宫殿/揭示集/注入层都跟着换人） -->
    <div v-if="speakers.length > 1" class="flex flex-none items-center gap-1.5">
      <span class="text-[11px] text-base-content/45">视角</span>
      <div role="tablist" class="tabs tabs-box tabs-xs">
        <button
          role="tab"
          class="tab tooltip tooltip-right"
          :class="{ 'tab-active': !inspView }"
          data-tip="主角色（缺省视角）"
          @click="inspView = ''"
        >
          主角色
        </button>
        <button
          v-for="dir in speakers"
          :key="dir"
          role="tab"
          class="tab"
          :class="{ 'tab-active': inspView === dir }"
          @click="inspView = dir"
        >
          {{ dir }}
        </button>
      </div>
    </div>

    <template v-if="inspTab === 'layers'">
      <div class="flex items-center justify-between gap-2">
        <p v-if="assembly" class="m-0 text-xs text-base-content/50">
          {{ inspView ? `预览 · 视角：${inspView}` : assemblySource === "last" ? "最近一次发送" : "预览 · 干跑" }}
        </p>
        <button class="btn btn-ghost btn-xs flex-none" @click="emit('preview')">
          <Icon name="refresh" :size="13" />刷新
        </button>
      </div>

      <template v-if="assembly">
        <!-- 预算总账（M2.7 · 设计 §4.2）：输入预算 / 实际占用 / 逐层用量 -->
        <div v-if="assembly.budget" class="rounded-box bg-base-200 px-3 py-2.5">
          <div class="flex items-center justify-between gap-2">
            <span class="text-[11px] text-base-content/45">输入预算（上下文 × 75%）</span>
            <span class="font-mono text-[11px] tabular-nums text-base-content/60">
              {{ assembly.budget.used_tokens }} / {{ assembly.budget.input_tokens }}
            </span>
          </div>
          <progress
            class="progress progress-primary mt-1.5 h-1.5 w-full"
            :value="assembly.budget.used_tokens"
            :max="assembly.budget.input_tokens || 1"
          ></progress>
          <div class="mt-2.5 flex flex-col gap-2">
            <div v-for="u in assembly.budget.layers" :key="u.id + u.name" class="flex flex-col gap-1">
              <div class="flex items-center gap-2 text-[11px]">
                <span
                  class="badge badge-xs badge-soft font-mono"
                  :class="u.trimmed ? 'badge-warning' : 'badge-primary'"
                  >{{ u.id }}</span
                >
                <span class="flex-1 truncate text-base-content/60">{{ u.name }}</span>
                <span v-if="u.trimmed" class="badge badge-xs badge-soft badge-warning">被裁</span>
                <span class="font-mono text-[11px] tabular-nums text-base-content/45">{{ u.tokens }} / {{ u.limit }}</span>
              </div>
              <progress
                class="progress h-1 w-full"
                :class="u.trimmed ? 'progress-warning' : 'progress-primary'"
                :value="u.tokens"
                :max="u.limit || 1"
              ></progress>
              <p v-if="u.trimmed" class="m-0 text-[11px] text-warning">{{ u.trimmed }}</p>
            </div>
          </div>
        </div>

        <div class="grid grid-cols-3 gap-2">
          <div class="rounded-box bg-base-200 px-3 py-2">
            <p class="m-0 text-[11px] text-base-content/45">注入层</p>
            <p class="m-0 text-lg leading-tight font-semibold">{{ assembly.layers.length }}</p>
          </div>
          <div class="rounded-box bg-base-200 px-3 py-2">
            <p class="m-0 text-[11px] text-base-content/45">估算 token</p>
            <p class="m-0 text-lg leading-tight font-semibold">{{ assembly.total_tokens }}</p>
          </div>
          <div class="rounded-box bg-base-200 px-3 py-2">
            <p class="m-0 text-[11px] text-base-content/45">消息数</p>
            <p class="m-0 text-lg leading-tight font-semibold">{{ assembly.messages.length }}</p>
          </div>
        </div>

        <div
          v-for="l in assembly.layers"
          :key="l.id + l.name"
          class="collapse collapse-arrow rounded-box bg-base-200"
        >
          <input type="checkbox" />
          <div class="collapse-title flex min-h-0 items-center gap-2.5 px-3 py-2 text-[13px]">
            <span class="badge badge-xs badge-soft badge-primary font-mono">{{ l.id }}</span>
            <span class="flex-1 truncate">{{ l.name }}</span>
            <!-- 被预算裁/截断过的层标出来（M2.7；说明在预算总账里） -->
            <span
              v-if="layerTrimmed(l.id, l.name)"
              class="badge badge-xs badge-soft badge-warning"
              :title="layerTrimmed(l.id, l.name)"
              >被裁</span
            >
            <span class="font-mono text-[11px] tabular-nums text-base-content/45">≈{{ l.tokens }}</span>
          </div>
          <div class="collapse-content px-3">
            <pre class="m-0 rounded-box border border-base-300 bg-base-100 p-2.5 text-xs leading-relaxed break-words whitespace-pre-wrap">{{ l.content }}</pre>
            <!-- 逐卡激活原因（设计 §6.11：每张注入卡都说得清「它为什么在这里」） -->
            <ul v-if="l.sources?.length" class="m-0 mt-1.5 flex list-none flex-col gap-1 p-0">
              <li
                v-for="s in l.sources"
                :key="s"
                class="rounded-box bg-base-200 px-2 py-1 font-mono text-[11px] text-base-content/60"
              >
                {{ s }}
              </li>
            </ul>
          </div>
        </div>
      </template>
      <EmptyState
        v-else
        icon="layers"
        title="尚无组装数据"
        desc="发一轮消息后，这里就有「这轮注入了什么」的完整预览。"
      />
    </template>

    <!-- M2.8 记忆检查器面板组：一份 inspectorData 喂六个页签（设计 §14） -->
    <template v-else-if="M2_TABS.includes(inspTab)">
      <div class="flex items-center justify-between gap-2">
        <p class="m-0 min-w-0 truncate text-xs text-base-content/50">
          {{ inspTabLabel }} · {{ inspector?.character || cardName }}
        </p>
        <button class="btn btn-ghost btn-xs flex-none" :disabled="inspLoading" @click="loadInspector">
          <span v-if="inspLoading" class="loading loading-spinner loading-xs"></span>
          <Icon v-else name="refresh" :size="13" />刷新
        </button>
      </div>

      <!-- 读失败只在这里提示：不打断聊天（浏览器 mock 未覆盖该命令时也走这里） -->
      <div v-if="inspError" role="alert" class="alert alert-error alert-soft py-2 text-xs break-words">
        {{ inspError }}
      </div>
      <div v-if="inspLoading" class="flex flex-col gap-3 rounded-box bg-base-200 p-4">
        <div class="skeleton h-3.5 w-40"></div>
        <div class="skeleton h-3 w-full"></div>
        <div class="skeleton h-3 w-4/5"></div>
        <div class="skeleton h-3 w-3/5"></div>
      </div>
      <EmptyState
        v-else-if="!inspector"
        icon="cpu"
        title="还没有检查器数据"
        desc="聊过一轮之后这里就有货；也可以点下面的按钮重拉。"
      >
        <button class="btn btn-primary btn-sm" :disabled="inspLoading" @click="loadInspector">
          <Icon name="refresh" :size="14" />重新读取
        </button>
      </EmptyState>

      <template v-else>
        <StatePathPanel
          v-if="inspTab === 'statetree'"
          :tree="inspector.stateTree"
          :transitions="inspector.transitions"
        />
        <ThreadsPanel
          v-else-if="inspTab === 'threads'"
          :threads="inspector.threads"
          :busy="threadBusy"
          @open="openThread"
          @resolve="resolveThreadCmd"
        />
        <PsychePanel v-else-if="inspTab === 'psyche'" :psyche="inspector.psyche" />
        <PalacePanel v-else-if="inspTab === 'palace'" :palace="inspector.palace" :session-id="sessionId" @jump="(t) => emit('jump', t)" @refresh="() => loadInspector()" />
        <CodexPanel
          v-else-if="inspTab === 'codex'"
          :session-id="sessionId"
          :codex="inspector.codex"
          :known="inspector.known"
          :active-entities="inspector.activeEntities"
          @refresh="loadInspector"
        />
        <SummaryPanel
          v-else
          :session-id="sessionId"
          :summary="inspector.summary"
          :proposals="inspector.proposals"
          :summarizing="summarizing"
          :result="summaryResult"
          :deciding="decidingId"
          :deciding-all="decidingAll"
          @summarize="summarizeNow"
          @decide="decideProposal"
          @decide-all="decideAllProposals"
        />
      </template>
    </template>

    <!-- 世界面板（M3.7 · 设计 §6.6）：世界主线阶段 / 世界时钟 / 世界级线 -->
    <template v-else-if="inspTab === 'world'">
      <div class="flex items-center justify-between gap-2">
        <p class="m-0 text-xs text-base-content/50">世界主线与世界时钟（B1 时代行 / B2 世界段的同一份数据）</p>
        <button class="btn btn-ghost btn-xs flex-none" :disabled="worldlineBusy" @click="loadWorldline">
          <span v-if="worldlineBusy" class="loading loading-spinner loading-xs"></span>
          <Icon v-else name="refresh" :size="13" />刷新
        </button>
      </div>
      <div v-if="worldlineError" role="alert" class="alert alert-error alert-soft py-2 text-xs break-words">
        {{ worldlineError }}
      </div>
      <div v-if="worldlineBusy" class="flex flex-col gap-3 rounded-box bg-base-200 p-4">
        <div class="skeleton h-3.5 w-36"></div>
        <div class="skeleton h-3 w-full"></div>
        <div class="skeleton h-3 w-2/3"></div>
      </div>
      <EmptyState v-else-if="!worldline" icon="globe" title="还没有世界数据" desc="聊过一轮或点「刷新」重拉就有。">
        <button class="btn btn-primary btn-sm" :disabled="worldlineBusy" @click="loadWorldline">
          <Icon name="refresh" :size="14" />重新读取
        </button>
      </EmptyState>
      <WorldPanel v-else :world="worldline" :busy="worldlineBusy" @calibrate="calibrateWorld" />
    </template>

    <!-- 导演面板（M3.4 · 设计 §10.5）：发言权调度史，逐条可查「为何轮到她」 -->
    <template v-else-if="inspTab === 'director'">
      <div class="flex items-center justify-between gap-2">
        <p class="m-0 text-xs text-base-content/50">发言权调度史（导演事件 · 最新在前）</p>
        <button class="btn btn-ghost btn-xs flex-none" :disabled="timelineLoading" @click="loadTimeline">
          <span v-if="timelineLoading" class="loading loading-spinner loading-xs"></span>
          <Icon v-else name="refresh" :size="13" />刷新
        </button>
      </div>
      <p class="m-0 text-[11px] text-base-content/40">
        打分信号：最近提及 · 场景黑板 · 剧情线关联 · 想说话投票 − 冷却。点名直通不经打分。
      </p>
      <div v-if="timelineError" role="alert" class="alert alert-error alert-soft py-2 text-xs break-words">
        {{ timelineError }}
      </div>
      <div v-if="timelineLoading" class="flex flex-col gap-2 rounded-box bg-base-200 p-4">
        <div class="skeleton h-3 w-full"></div>
        <div class="skeleton h-3 w-4/5"></div>
        <div class="skeleton h-3 w-3/5"></div>
      </div>
      <EmptyState v-else-if="!timeline" icon="film" title="还没有调度史" desc="用「导演调度」发一轮，这里就逐条可查「为何轮到她」。">
        <button class="btn btn-primary btn-sm" :disabled="timelineLoading" @click="loadTimeline">
          <Icon name="refresh" :size="14" />重新读取
        </button>
      </EmptyState>
      <ul v-else-if="directorHistory.length" class="m-0 flex list-none flex-col gap-1.5 p-0">
        <li
          v-for="e in directorHistory"
          :key="e.seq"
          class="rounded-box bg-base-200 px-2.5 py-1.5 text-xs"
        >
          <span class="break-words">{{ e.brief }}</span>
          <span class="ml-1 inline-block font-mono text-[10px] text-base-content/35">
            第 {{ e.turn }} 轮
          </span>
        </li>
      </ul>
      <EmptyState
        v-else
        icon="film"
        title="还没有调度记录"
        desc="多角色会话用「导演调度」发一轮就有了。"
      />
    </template>

    <!-- 卡内状态 / 卡内记忆 / 事件流（M1.6 可观测面，数据在聊天流侧） -->
    <CardStatePanel
      v-else
      :tab="inspTab"
      :card-state="cardState"
      :memory="memory"
      :last-report="lastReport"
      :hook-events="hookEvents"
      :hook-logs="hookLogs"
      :timeline="timeline"
      :timeline-loading="timelineLoading"
      :timeline-error="timelineError"
      @refresh-card="emit('refreshCard')"
      @refresh-timeline="loadTimeline"
      @clear-events="emit('clearEvents')"
    />
  </div>
</template>
