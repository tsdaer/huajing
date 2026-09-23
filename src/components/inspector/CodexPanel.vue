<script setup lang="ts">
// 设定集面板（M2.8 · 设计 §6）：世界名 + 实体清单（类型/生命周期/one-liner/anchors）+ 揭示集
// + 史变解析预览（M3.7 · 设计 §6.5：按故事时钟回答「第 N 天的事实」）
// M3.8：缺失模板 facet 高亮 +「补全」按钮 → diff 卡片（接受/重写/丢弃）
import { computed, ref } from "vue";
import { api } from "../../api";
import type { CompletionResult, InspectorCodex, ResolvePreview } from "../../types";
import { statusClass, statusLabel } from "./util";

const props = defineProps<{ sessionId: string; codex: InspectorCodex; known: string[]; activeEntities: string[] }>();

const emit = defineEmits<{ refresh: [] }>();

// ---------- 史变解析预览（M3.7）----------
const preview = ref<ResolvePreview | null>(null);
const previewDay = ref("");
const previewBusy = ref(false);
const previewError = ref("");

const lifecycleLabel: Record<string, string> = {
  active: "在场",
  departed: "已离场",
  dead: "已故",
};

/** 有史变或生命周期可讲的实体才进预览（纯静态实体列出来只是噪音） */
const previewEntities = computed(() =>
  (preview.value?.entities ?? []).filter(
    (e) => e.versions.length > 0 || (e.lifecycle["status"] ?? "active") !== "active" || e.status === "retired",
  ),
);

async function loadPreview() {
  previewBusy.value = true;
  previewError.value = "";
  try {
    const day = previewDay.value.trim() === "" ? undefined : Number(previewDay.value);
    preview.value = await api.codexResolvePreview(props.sessionId, day);
  } catch (e) {
    previewError.value = String(e);
  } finally {
    previewBusy.value = false;
  }
}

// ---------- 手动补全（M3.8 · 设计 §6.8-1）----------
const completingId = ref("");
const completion = ref<CompletionResult | null>(null);
const completionBusy = ref(false);
const completionError = ref("");
const applying = ref(false);

/** 生成值的一行文本（对象压 JSON，其余原样） */
function valueText(v: unknown): string {
  if (v === null || v === undefined || v === "") return "（空）";
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

async function complete(id: string) {
  completingId.value = id;
  completionBusy.value = true;
  completionError.value = "";
  completion.value = null;
  try {
    completion.value = await api.codexComplete(props.sessionId, id);
  } catch (e) {
    completionError.value = String(e);
  } finally {
    completingId.value = "";
    completionBusy.value = false;
  }
}

/** 接受：被驳回的条目不带；propose+accept 落流并物化进正史 */
async function acceptCompletion() {
  if (!completion.value) return;
  const facets: Record<string, unknown> = {};
  for (const item of completion.value.items) {
    if (!item.rejected) facets[item.facet] = item.value;
  }
  applying.value = true;
  completionError.value = "";
  try {
    await api.codexCompleteApply(props.sessionId, completion.value.target, facets, completion.value.note || undefined);
    completion.value = null;
    emit("refresh");
  } catch (e) {
    completionError.value = String(e);
  } finally {
    applying.value = false;
  }
}
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
    <p class="m-0 text-xs text-base-content/50">
      实体（草稿不进注入，只有正史参与激活；retired 留档可查；琥珀色是缺失的模板 facet）
    </p>
    <div v-for="e in codex.entities" :key="e.id" class="rounded-box flex flex-col gap-1 bg-base-200 p-2.5">
      <div class="flex flex-wrap items-center gap-1.5">
        <span class="min-w-0 flex-1 truncate text-sm font-medium">{{ e.name }}</span>
        <span class="badge badge-xs badge-soft badge-neutral">{{ e.type }}</span>
        <span class="badge badge-xs" :class="statusClass(e.status)">{{ statusLabel(e.status) }}</span>
        <button
          v-if="e.missing.length && e.status !== 'retired'"
          class="btn btn-ghost btn-xs"
          :disabled="completionBusy || completingId === e.id"
          :data-tip="`补全缺失的 ${e.missing.length} 项（读取实体 + 关系 + 世界概览）`"
          @click="complete(e.id)"
        >
          <span v-if="completingId === e.id" class="loading loading-spinner loading-xs"></span>
          补全
        </button>
      </div>
      <p v-if="e.oneLiner" class="m-0 text-[11px] break-words text-base-content/60">{{ e.oneLiner }}</p>
      <p v-if="e.anchors.length" class="m-0 text-[10px] break-words text-base-content/40">
        anchors：{{ e.anchors.join(" · ") }}
      </p>
      <div v-if="e.missing.length" class="flex flex-wrap items-center gap-1">
        <span v-for="m in e.missing" :key="m" class="badge badge-xs badge-soft badge-warning font-mono">{{ m }}</span>
      </div>
      <p class="m-0 font-mono text-[10px] text-base-content/35">{{ e.id }}</p>

      <!-- 补全 diff 卡片（M3.8）：生成草稿的逐 facet 呈现，接受/重写/丢弃 -->
      <div
        v-if="completion && completion.target === e.id"
        class="mt-1 flex flex-col gap-1.5 rounded-box border border-base-300 bg-base-100 p-2.5"
      >
        <div class="flex flex-wrap items-center gap-1.5">
          <span class="text-[11px] font-semibold text-base-content/70">补全草稿</span>
          <span v-if="completion.note" class="min-w-0 flex-1 truncate text-[10px] text-base-content/40" :title="completion.note">
            {{ completion.note }}
          </span>
        </div>
        <div v-for="item in completion.items" :key="item.facet" class="flex flex-col gap-0.5 rounded-box bg-base-200 p-2">
          <p class="m-0 font-mono text-[10px] break-words text-base-content/60">{{ item.facet }}</p>
          <p class="m-0 text-[11px] break-words" :class="item.rejected ? 'text-base-content/30 line-through' : 'text-success'">
            {{ valueText(item.value) }}
          </p>
          <p v-if="item.rejected" class="m-0 text-[10px] break-words text-error">{{ item.rejected }}——已剔除</p>
          <p v-else-if="item.warn" class="m-0 text-[10px] break-words text-warning">
            {{ item.warn.detail }}<template v-if="item.warn.current !== undefined">（现值：{{ valueText(item.warn.current) }}）</template>
          </p>
        </div>
        <div class="flex items-center justify-end gap-1.5">
          <button class="btn btn-ghost btn-xs" :disabled="applying || completionBusy" @click="complete(e.id)">重写</button>
          <button class="btn btn-ghost btn-xs" :disabled="applying" @click="completion = null">丢弃</button>
          <button
            class="btn btn-primary btn-xs"
            :disabled="applying || !completion.items.some((i) => !i.rejected)"
            @click="acceptCompletion"
          >
            <span v-if="applying" class="loading loading-spinner loading-xs"></span>
            接受（{{ completion.items.filter((i) => !i.rejected).length }} 项）
          </button>
        </div>
      </div>
    </div>
    <p v-if="!codex.entities.length" class="m-0 text-xs text-base-content/50">
      这个世界还没有实体——导入世界书或让总结管线提案之后就有了。
    </p>
    <div v-if="completionError" role="alert" class="alert alert-error alert-soft py-1.5 text-xs break-words">
      {{ completionError }}
    </div>
  </section>

  <!-- 史变解析预览（M3.7 · 设计 §6.5）：versions 追加不覆盖，按故事时钟解析 -->
  <section class="flex flex-col gap-1.5 border-t border-base-300 pt-2">
    <div class="flex items-center gap-1.5">
      <span class="min-w-0 flex-1 text-xs text-base-content/50">史变预览 · 第 N 天的事实</span>
      <input
        v-model="previewDay"
        type="number"
        min="1"
        placeholder="缺省=当前"
        class="input input-xs input-bordered w-24 font-mono"
      />
      <button class="btn btn-ghost btn-xs flex-none" :disabled="previewBusy" @click="loadPreview">
        <span v-if="previewBusy" class="loading loading-spinner loading-xs"></span>
        <span v-else>解析</span>
      </button>
    </div>
    <div v-if="previewError" role="alert" class="alert alert-error alert-soft py-2 text-xs break-words">
      {{ previewError }}
    </div>
    <p v-if="preview" class="m-0 text-[11px] text-base-content/40">
      第 {{ preview.day }} 天 · {{ previewEntities.length }} 个实体有史变/生命周期可讲
    </p>
    <div v-for="e in previewEntities" :key="e.id" class="rounded-box flex flex-col gap-1 bg-base-200 p-2.5">
      <div class="flex flex-wrap items-center gap-1.5">
        <span class="min-w-0 flex-1 truncate text-sm font-medium">{{ e.name }}</span>
        <span class="badge badge-xs badge-soft">{{ lifecycleLabel[String(e.lifecycle.status)] ?? String(e.lifecycle.status) }}</span>
        <span v-if="e.lifecycle.present === false" class="badge badge-xs badge-ghost">此刻不在场</span>
      </div>
      <p class="m-0 text-[11px] break-words text-base-content/60">{{ e.one_liner }}</p>
      <p v-for="(v, i) in e.versions" :key="i" class="m-0 text-[10px] break-words" :class="v.active ? 'text-success' : 'text-base-content/35'">
        第 {{ v.from_day }} 天起 · {{ v.facet }} = {{ JSON.stringify(v.value) }}<template v-if="v.note">（{{ v.note }}）</template><template v-if="v.active"> · 此刻生效</template>
      </p>
    </div>
    <p v-if="preview && !previewEntities.length" class="m-0 text-xs text-base-content/50">
      这些实体没有史变版本，生命周期也都在场。
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
