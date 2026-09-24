<script setup lang="ts">
// 摘要与收件箱（M2.8 · 设计 §5.3 / §6.9）：滚动摘要 + 立即总结 + 提案确认/否决
// M3.8：diff 双源呈现（正史现值 vs 提案值）、来源徽标（管线/补全/暂定）、批量处理、
// 可选的 LLM 语义矛盾检测（§6.8-3：结论随确认时的备注留档进事件流）
import { ref } from "vue";
import Icon from "../Icon.vue";
import { api } from "../../api";
import type { InspectorProposal } from "../../types";
import { payloadBrief, payloadFields, proposalKind, proposalStatus, proposalOrigin, originClass, auditFindingLabel } from "./util";

const props = defineProps<{
  sessionId: string;
  summary: string;
  proposals: InspectorProposal[];
  summarizing: boolean;
  result: string;
  deciding: string;
  decidingAll: boolean;
}>();

const emit = defineEmits<{
  summarize: [];
  decide: [id: string, accept: boolean, note?: string];
  decideAll: [accept: boolean];
}>();

/** 提案值的一行文本（对象压 JSON，其余原样） */
function valueText(v: unknown): string {
  if (v === null || v === undefined || v === "") return "（空）";
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

// ---------- 语义矛盾检测（可选，按需触发；结论随决定时的备注留档） ----------
const checkingId = ref("");
const checkResults = ref<Record<string, string[]>>({});
const checkError = ref("");

/** codex 类提案才有「语义检查」——thread/psyche 的对照物不是实体事实 */
function checkable(p: InspectorProposal): boolean {
  return (
    p.status === "propose" &&
    ["new_entity", "new_fact", "fact_change", "relation"].includes(p.kind)
  );
}

async function runCheck(id: string) {
  if (checkingId.value) return;
  checkingId.value = id;
  checkError.value = "";
  try {
    const out = await api.codexSemanticCheck(props.sessionId, id);
    checkResults.value = { ...checkResults.value, [id]: out.contradictions };
  } catch (e) {
    checkError.value = String(e);
  } finally {
    checkingId.value = "";
  }
}

function decideWithNote(id: string, accept: boolean) {
  const found = checkResults.value[id];
  const note = found && found.length ? `语义检查：${found.join("；")}` : undefined;
  emit("decide", id, accept, note);
}
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

    <!-- 批量处理（M3.8 · DoD 8）：只对待处理条数 > 0 时给出 -->
    <div v-if="proposals.some((p) => p.status === 'propose')" class="flex items-center justify-end gap-1.5">
      <span class="text-[11px] text-base-content/40">批量：</span>
      <button
        class="btn btn-xs btn-ghost"
        :disabled="decidingAll"
        @click="emit('decideAll', false)"
      >
        全部否决
      </button>
      <button
        class="btn btn-xs btn-primary btn-soft"
        :disabled="decidingAll"
        @click="emit('decideAll', true)"
      >
        <span v-if="decidingAll" class="loading loading-spinner loading-xs"></span>
        全部确认
      </button>
    </div>

    <div v-for="p in proposals" :key="p.id" class="rounded-box flex flex-col gap-1.5 bg-base-200 p-2.5">
      <div class="flex flex-wrap items-center gap-1.5">
        <span class="badge badge-xs badge-soft badge-primary">{{ proposalKind(p.kind) }}</span>
        <span class="badge badge-xs" :class="originClass(p.origin)" :data-tip="`来源：${p.origin || 'pipeline'}`">
          {{ proposalOrigin(p.origin) }}
        </span>
        <span class="badge badge-xs badge-ghost font-mono">第 {{ p.turn }} 轮</span>
        <span
          class="badge badge-xs"
          :class="p.status === 'propose' ? 'badge-soft badge-warning' : p.status === 'accept' ? 'badge-soft badge-success' : 'badge-soft badge-neutral'"
        >
          {{ proposalStatus(p.status) }}
        </span>
      </div>

      <!-- diff 双源呈现（M3.8）：新事实/改事实给出「正史现值 → 提案值」；其余给单行摘要 -->
      <template v-if="(p.kind === 'new_fact' || p.kind === 'fact_change') && payloadFields(p.payload).facet">
        <p class="m-0 font-mono text-[11px] break-words text-base-content/70">
          {{ payloadFields(p.payload).target }} · {{ payloadFields(p.payload).facet }}
        </p>
        <div v-if="p.currentValue" class="flex flex-col gap-0.5 rounded-box bg-base-300/60 p-2">
          <p class="m-0 text-[11px] break-words">
            <span class="text-base-content/40">现状（{{ p.currentValue.source }}）：</span>
            <span class="text-base-content/60">{{ valueText(p.currentValue.value) }}</span>
          </p>
          <p class="m-0 text-[11px] break-words">
            <span class="text-base-content/40">提案（第 {{ p.turn }} 轮）：</span>
            <span class="text-success">{{ valueText(payloadFields(p.payload).value) }}</span>
          </p>
        </div>
        <p v-else class="m-0 text-[11px] break-words">
          <span class="text-base-content/40">新增：</span>
          <span class="text-success">{{ valueText(payloadFields(p.payload).value) }}</span>
        </p>
      </template>
      <template v-else-if="p.kind === 'relation' && payloadFields(p.payload).to">
        <p class="m-0 text-[11px] break-words">
          <span class="font-mono text-base-content/70">{{ payloadFields(p.payload).target }}</span>
          <span class="text-base-content/40"> 新增关系：</span>
          <span class="text-success">{{ payloadFields(p.payload).relation }} → {{ payloadFields(p.payload).to }}</span>
        </p>
      </template>
      <!-- 关联审计发现（M3.10 · §6.13）：检索层漏了谁/设定缺了什么，附引源可点验 -->
      <template v-else-if="p.kind === 'audit' && payloadFields(p.payload).target">
        <p class="m-0 text-[11px] break-words">
          <span class="text-warning">{{ auditFindingLabel(payloadFields(p.payload).finding) }}</span>
          <span class="text-base-content/40"> · </span>
          <span class="font-mono text-base-content/70">{{ payloadFields(p.payload).target }}</span>
          <template v-if="payloadFields(p.payload).facet">
            <span class="text-base-content/40"> · {{ payloadFields(p.payload).facet }}</span>
          </template>
        </p>
        <p class="m-0 text-[11px] break-words text-base-content/60">
          {{ payloadFields(p.payload).evidence }}
        </p>
        <p class="m-0 text-[11px] text-base-content/40">
          （确认与否决只做记录；给实体补设定请用「设定」页的补全或手动编辑）
        </p>
      </template>
      <template v-else-if="p.origin === 'improv' && payloadFields(p.payload).text">
        <p class="m-0 text-[11px] break-words text-base-content/70">
          「{{ payloadFields(p.payload).text }}」
          <span class="text-base-content/40">（已按暂定注入当轮，确认后成为正史）</span>
        </p>
      </template>
      <p v-else-if="payloadBrief(p.payload)" class="m-0 font-mono text-[11px] leading-relaxed break-words text-base-content/60">
        {{ payloadBrief(p.payload) }}
      </p>
      <p v-else class="m-0 text-[11px] text-base-content/40">（这条提案没有 payload）</p>

      <!-- 语义矛盾检测结果（§6.8-3）：与正史现值并排呈现，决定后随备注留档 -->
      <div v-if="checkResults[p.id]" class="flex flex-col gap-0.5 rounded-box bg-base-300/60 p-2">
        <p v-if="checkResults[p.id].length" class="m-0 text-[11px] break-words text-warning">
          <span class="text-base-content/40">语义矛盾：</span>{{ checkResults[p.id].join("；") }}
        </p>
        <p v-else class="m-0 text-[11px] text-base-content/40">语义检查：未发现矛盾。</p>
      </div>
      <p v-if="checkError" class="m-0 text-[11px] break-words text-error">{{ checkError }}</p>
      <p v-if="p.note" class="m-0 text-[11px] break-words text-base-content/50">备注：{{ p.note }}</p>
      <div v-if="p.status === 'propose'" class="flex items-center justify-end gap-2">
        <button
          v-if="checkable(p)"
          class="btn btn-ghost btn-xs"
          :disabled="checkingId === p.id || Boolean(checkingId)"
          data-tip="可选：让便宜模型对照实体现状判断有没有语义矛盾（§6.8-3）"
          @click="runCheck(p.id)"
        >
          <span v-if="checkingId === p.id" class="loading loading-spinner loading-xs"></span>
          语义检查
        </button>
        <button
          class="btn btn-xs btn-primary"
          :disabled="deciding === p.id || decidingAll"
          @click="decideWithNote(p.id, true)"
        >
          <span v-if="deciding === p.id" class="loading loading-spinner loading-xs"></span>
          确认
        </button>
        <button class="btn btn-xs" :disabled="deciding === p.id || decidingAll" @click="decideWithNote(p.id, false)">
          否决
        </button>
      </div>
    </div>

    <p v-if="!proposals.length" class="m-0 text-xs text-base-content/50">
      收件箱是空的——还没有等待确认的提案。
    </p>
  </section>
</template>
