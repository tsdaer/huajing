<script setup lang="ts">
// 素材规格化向导（M3.9 · 设计 §6.7）：wiki 角色页粘贴导入 → 十分钟产出可开聊角色。
// 四步对应管线的八步：①②清洗分段（确定性）→ ③分节分类（LLM）→ ④⑤机械映射+语义归纳
// → ⑥⑦草稿包审阅 + ⑧切入点向导（commit 落盘：卡 + 正史增量 + 世界线）。
import { computed, ref } from "vue";
import { api } from "../api";
import type {
  IngestCommitReport,
  IngestPack,
  IngestPrep,
  WorldbookReport,
} from "../types";
import ErrorToast from "../components/ErrorToast.vue";
import Icon from "../components/Icon.vue";

const TAGS = [
  { value: "infobox", label: "信息框" },
  { value: "intro", label: "简介" },
  { value: "history", label: "经历" },
  { value: "relations", label: "关系" },
  { value: "dialogue_scene", label: "对话场景" },
  { value: "quote_table", label: "台词表" },
  { value: "mechanics", label: "机制（过滤）" },
  { value: "trivia", label: "考据" },
  { value: "unknown", label: "待定" },
];

const step = ref(1);
const error = ref("");
const busy = ref("");
const world = ref("default");
const nameHint = ref("");
const rawText = ref("");

const prep = ref<IngestPrep | null>(null);
const tags = ref<Record<string, string>>({});
const pack = ref<IngestPack | null>(null);

const chosenDay = ref(1);
const overwriteWorldline = ref(false);
const setWorldDay = ref(false);
const report = ref<IngestCommitReport | null>(null);

const tagCount = computed(() => Object.keys(tags.value).length);

async function doPrepare() {
  if (rawText.value.trim().length < 30) {
    error.value = "素材太短——粘贴完整的 wiki 角色页或剧情记录再试";
    return;
  }
  busy.value = "prepare";
  error.value = "";
  try {
    prep.value = await api.ingestPrepare(world.value, rawText.value);
    tags.value = {};
    step.value = 2;
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = "";
  }
}

async function doClassify() {
  if (!prep.value) return;
  busy.value = "classify";
  error.value = "";
  try {
    const list = await api.ingestClassify(prep.value.sections);
    const map: Record<string, string> = {};
    for (const s of prep.value.sections) {
      map[s.id] = list.find((t) => t.id === s.id)?.tag ?? "unknown";
    }
    tags.value = map;
    step.value = 3;
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = "";
  }
}

async function doExtract() {
  if (!prep.value) return;
  busy.value = "extract";
  error.value = "";
  try {
    const tagList = Object.entries(tags.value).map(([id, tag]) => ({ id, tag }));
    pack.value = await api.ingestExtract(
      world.value,
      nameHint.value.trim() || null,
      prep.value.sections,
      tagList,
      prep.value.spoilers,
    );
    chosenDay.value = pack.value.canon_points[0]?.day ?? 1;
    step.value = 4;
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = "";
  }
}

async function doCommit() {
  if (!pack.value) return;
  busy.value = "commit";
  error.value = "";
  try {
    report.value = await api.ingestCommit(
      world.value,
      pack.value,
      chosenDay.value,
      overwriteWorldline.value,
      setWorldDay.value,
    );
    step.value = 5;
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = "";
  }
}

function reset() {
  step.value = 1;
  prep.value = null;
  pack.value = null;
  report.value = null;
  rawText.value = "";
  nameHint.value = "";
}

// ---------- 展示辅助 ----------

/** 嵌套 facts → 点分路径列表（审阅可逐条看引源） */
function flattenFacts(obj: Record<string, unknown>, prefix = ""): Array<{ path: string; value: unknown }> {
  const out: Array<{ path: string; value: unknown }> = [];
  for (const [k, v] of Object.entries(obj)) {
    const path = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === "object" && !Array.isArray(v)) {
      out.push(...flattenFacts(v as Record<string, unknown>, path));
    } else {
      out.push({ path, value: v });
    }
  }
  return out;
}

/** 事实按顶层键分组：审阅页从 JSON dump 变成按主题分组的小卡 */
const factGroups = computed(() => {
  if (!pack.value) return [];
  const groups = new Map<string, Array<{ path: string; value: unknown }>>();
  for (const f of flattenFacts(pack.value.entity.facts)) {
    const top = f.path.split(".")[0] ?? f.path;
    if (!groups.has(top)) groups.set(top, []);
    groups.get(top)!.push(f);
  }
  return [...groups.entries()];
});

/** 组内显示用短路径（顶层前缀已在分组标题上） */
function shortPath(path: string): string {
  const i = path.indexOf(".");
  return i === -1 ? path : path.slice(i + 1);
}

function fmtValue(v: unknown): string {
  if (Array.isArray(v)) return v.map((x) => String(x)).join("；");
  if (v === null || v === undefined) return "";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

function sectionTitle(id: string): string {
  const s = prep.value?.sections.find((x) => x.id === id);
  return s ? `【${s.title}】` : `【${id}】`;
}

// ---------- ST 世界书导入（M2.2 欠账补课） ----------

const wbJson = ref("");
const wbBook = ref("");
const wbReport = ref<WorldbookReport | null>(null);
const wbBusy = ref(false);

async function doImportWorldbook() {
  if (!wbJson.value.trim()) {
    error.value = "先粘贴世界书 JSON";
    return;
  }
  wbBusy.value = true;
  error.value = "";
  try {
    wbReport.value = await api.importWorldbook(
      world.value,
      wbJson.value,
      null,
      wbBook.value.trim() || null,
    );
    wbJson.value = "";
  } catch (e) {
    error.value = String(e);
  } finally {
    wbBusy.value = false;
  }
}

const promptsOpen = ref(false);
const promptsText = ref("");

async function showPrompts() {
  try {
    promptsText.value = await api.ingestPrompts();
    promptsOpen.value = true;
  } catch (e) {
    error.value = String(e);
  }
}

async function copyPrompts() {
  try {
    await navigator.clipboard.writeText(promptsText.value);
  } catch {
    /* 剪贴板权限被拒：文本已在弹窗里可手动全选复制 */
  }
}
</script>

<template>
  <div class="h-full overflow-y-auto p-4 lg:p-6">
    <ErrorToast :message="error" @dismiss="error = ''" />

    <div class="mx-auto flex w-full max-w-[900px] flex-col gap-4">
      <div class="flex items-center justify-between gap-3">
        <h1 class="m-0 text-lg font-semibold">素材规格化</h1>
        <button class="btn btn-ghost btn-xs" @click="showPrompts">
          <Icon name="copy" :size="13" />手动模式提示词（P0–P11）
        </button>
      </div>

      <!-- 步骤条 -->
      <ul class="steps steps-horizontal w-full text-xs">
        <li class="step" :class="step >= 1 && 'step-primary'">粘贴素材</li>
        <li class="step" :class="step >= 2 && 'step-primary'">分节分类</li>
        <li class="step" :class="step >= 3 && 'step-primary'">语义归纳</li>
        <li class="step" :class="step >= 4 && 'step-primary'">审阅与切入点</li>
        <li class="step" :class="step >= 5 && 'step-primary'">落盘</li>
      </ul>

      <!-- ① 粘贴素材 -->
      <section v-if="step === 1" class="card card-border bg-base-100">
        <div class="card-body gap-3 p-5">
          <h2 class="card-title gap-2 text-sm font-medium">
            <Icon name="database" :size="16" class="text-base-content/45" />
            粘贴 wiki 角色页 / 剧情记录 / 台词集
          </h2>
          <div class="flex flex-wrap gap-3">
            <label class="form-control w-40">
              <span class="label-text pb-1 text-xs text-base-content/60">世界</span>
              <input v-model="world" class="input input-sm input-bordered w-full" placeholder="default" />
            </label>
            <label class="form-control w-52">
              <span class="label-text pb-1 text-xs text-base-content/60">角色名（可选，默认取信息框）</span>
              <input v-model="nameHint" class="input input-sm input-bordered w-full" placeholder="爱莉希雅" />
            </label>
          </div>
          <textarea
            v-model="rawText"
            class="textarea textarea-bordered min-h-56 w-full font-mono text-xs leading-relaxed"
            placeholder="把 wiki 页全文（含信息框/经历/台词表/黑幕剧透段）粘贴到这里……"
          ></textarea>
          <div class="flex items-center justify-between">
            <p class="m-0 text-[11px] text-base-content/45">
              剧透标记（黑幕/spoiler/heimu）会自动收为秘密候选；机制数值面板会被过滤；全部产物先审后写。
            </p>
            <button class="btn btn-primary btn-sm flex-none" :disabled="!!busy" @click="doPrepare">
              <Icon name="refresh" :size="14" :class="busy === 'prepare' && 'animate-spin'" />
              {{ busy === "prepare" ? "清洗中…" : "清洗并分段" }}
            </button>
          </div>
        </div>
      </section>

      <!-- ② 分节分类 -->
      <section v-else-if="step === 2 && prep" class="card card-border bg-base-100">
        <div class="card-body gap-3 p-5">
          <h2 class="card-title gap-2 text-sm font-medium">
            <Icon name="layers" :size="16" class="text-base-content/45" />
            分节分类
            <span class="badge badge-ghost badge-sm">{{ prep.sections.length }} 节</span>
          </h2>
          <p class="m-0 text-xs text-base-content/50">
            每节打一个标签；机制数据不参与后续归纳，拿不准的归「待定」（宁漏勿错）。
          </p>
          <div class="flex flex-col gap-2">
            <div
              v-for="s in prep.sections"
              :key="s.id"
              class="flex items-start gap-3 rounded-box border border-base-200 p-3"
            >
              <div class="min-w-0 flex-1">
                <div class="flex items-baseline gap-2">
                  <span class="text-xs font-medium">{{ s.title }}</span>
                  <span class="font-mono text-[10px] text-base-content/35">{{ s.id }}</span>
                  <span class="text-[10px] text-base-content/35">{{ s.text.length }} 字</span>
                </div>
                <p class="m-0 mt-1 line-clamp-2 text-[11px] leading-relaxed text-base-content/50">
                  {{ s.text.slice(0, 120) }}{{ s.text.length > 120 ? "…" : "" }}
                </p>
              </div>
              <select v-model="tags[s.id]" class="select select-bordered select-xs w-28 flex-none">
                <option v-for="t in TAGS" :key="t.value" :value="t.value">{{ t.label }}</option>
              </select>
            </div>
          </div>
          <div v-if="prep.spoilers.length" class="rounded-box border border-warning/40 bg-warning/5 p-3 text-xs">
            <span class="font-medium">剧透候选 {{ prep.spoilers.length }} 条：</span>
            {{ prep.spoilers.map((s) => s.slice(0, 40)).join("；") }}
          </div>
          <div class="flex justify-between">
            <button class="btn btn-ghost btn-sm" @click="step = 1">上一步</button>
            <button class="btn btn-primary btn-sm" :disabled="!!busy" @click="doClassify">
              <Icon name="sparkle" :size="14" :class="busy === 'classify' && 'animate-pulse'" />
              {{ busy === "classify" ? "分类中…" : "自动分类（LLM）" }}
            </button>
          </div>
        </div>
      </section>

      <!-- ③ 归纳（进度页）+ ④ 审阅入口 -->
      <section v-else-if="step === 3 && prep" class="card card-border bg-base-100">
        <div class="card-body gap-3 p-5">
          <h2 class="card-title gap-2 text-sm font-medium">
            <Icon name="cpu" :size="16" class="text-base-content/45" />
            语义归纳
          </h2>
          <ul class="m-0 flex list-none flex-col gap-1.5 p-0 text-xs text-base-content/60">
            <li class="flex items-center gap-1.5">
              <Icon name="check" :size="13" class="flex-none text-success" />
              分节完成（{{ tagCount }} 节已标）
            </li>
            <li class="flex items-center gap-1.5">
              <Icon name="dot" :size="13" class="flex-none text-base-content/30" />
              秘密与生命周期（P3）——从经历/关系/对话场景提取
            </li>
            <li class="flex items-center gap-1.5">
              <Icon name="dot" :size="13" class="flex-none text-base-content/30" />
              描写四法（P4）——台词与叙述归纳语言/外貌/动作/心理外化
            </li>
            <li class="flex items-center gap-1.5">
              <Icon name="dot" :size="13" class="flex-none text-base-content/30" />
              倾向性（P5）——动机/需要/价值观/气质
            </li>
            <li class="flex items-center gap-1.5">
              <Icon name="dot" :size="13" class="flex-none text-base-content/30" />
              事件年表与世界线（P6）——经历章节 → 事件实体 + 阶段弧
            </li>
            <li class="flex items-center gap-1.5">
              <Icon name="dot" :size="13" class="flex-none text-base-content/30" />
              关系网（P7）· 示例对话（P8）
            </li>
          </ul>
          <div class="flex justify-between">
            <button class="btn btn-ghost btn-sm" @click="step = 2">上一步</button>
            <button class="btn btn-primary btn-sm" :disabled="!!busy" @click="doExtract">
              <Icon name="bolt" :size="14" :class="busy === 'extract' && 'animate-pulse'" />
              {{ busy === "extract" ? "归纳中…（约 6 次调用，稍候）" : "开始归纳" }}
            </button>
          </div>
        </div>
      </section>

      <!-- ④ 草稿包审阅 + 切入点 -->
      <template v-else-if="step === 4 && pack">
        <section class="card card-border bg-base-100">
          <div class="card-body gap-3 p-5">
            <h2 class="card-title gap-2 text-sm font-medium">
              <Icon name="users" :size="16" class="text-base-content/45" />
              角色实体 ·
              <span class="font-mono text-xs font-normal text-base-content/50">{{ pack.char_id }}</span>
            </h2>
            <div class="grid gap-2 text-xs sm:grid-cols-2">
              <label class="form-control">
                <span class="label-text pb-1 text-base-content/60">一句话介绍（≤40 字含辨识点）</span>
                <input v-model="pack.entity.one_liner" class="input input-sm input-bordered" />
              </label>
              <div class="form-control">
                <span class="label-text pb-1 text-base-content/60">别名</span>
                <span class="rounded-box border border-base-200 px-2 py-1.5">{{ pack.entity.aliases.join("、") || "—" }}</span>
              </div>
            </div>
            <div class="flex flex-col gap-2.5">
              <div
                v-for="[group, facts] in factGroups"
                :key="group"
                class="overflow-hidden rounded-box border border-base-200"
              >
                <div class="flex items-center gap-2 border-b border-base-200 bg-base-200/50 px-3 py-1.5">
                  <span class="badge badge-xs badge-soft font-mono">{{ group }}</span>
                  <span class="text-[10px] text-base-content/40">{{ facts.length }} 条</span>
                </div>
                <div class="flex flex-col divide-y divide-base-200/70">
                  <div
                    v-for="f in facts"
                    :key="f.path"
                    class="flex items-start gap-2 px-3 py-1.5 text-xs"
                  >
                    <span
                      class="w-28 flex-none truncate font-mono text-[10px] text-base-content/45"
                      :title="f.path"
                    >{{ shortPath(f.path) }}</span>
                    <span class="min-w-0 flex-1 break-words">{{ fmtValue(f.value) || "—" }}</span>
                    <span
                      v-if="pack.entity.sources[f.path]"
                      class="flex-none text-[10px] text-info/70"
                      :title="pack.entity.sources[f.path].quote"
                    >
                      {{ sectionTitle(pack.entity.sources[f.path].section) }}
                    </span>
                  </div>
                </div>
              </div>
            </div>
            <div v-if="pack.entity.relations.length" class="text-xs">
              <span class="text-base-content/60">关系：</span>
              {{ pack.entity.relations.map((r) => `${r.kind}→${r.to}`).join("；") }}
            </div>
            <div v-if="pack.lifecycle" class="text-xs text-base-content/60">
              生命周期：{{ pack.lifecycle.status }}
              <span v-if="pack.lifecycle.at_day">（第 {{ pack.lifecycle.at_day }} 天生效）</span>
              <span v-if="pack.lifecycle.note"> · {{ pack.lifecycle.note }}</span>
            </div>
          </div>
        </section>

        <section v-if="pack.secrets.length" class="card card-border bg-base-100">
          <div class="card-body gap-2 p-5">
            <h2 class="card-title gap-2 text-sm font-medium">
              <Icon name="search" :size="16" class="text-base-content/45" />
              秘密候选
              <span class="text-xs font-normal text-base-content/50">切入点之前的秘密只有本人知道</span>
            </h2>
            <div
              v-for="s in pack.secrets"
              :key="s.key"
              class="flex items-start gap-2 rounded-box border border-base-200 px-3 py-2 text-xs"
            >
              <input v-model="s.include" type="checkbox" class="checkbox checkbox-sm mt-0.5" />
              <div class="min-w-0 flex-1">
                <div>{{ s.content }}</div>
                <div class="mt-0.5 text-[10px] text-base-content/40">
                  {{ s.origin === "spoiler" ? "剧透标记" : "归纳" }}
                  <template v-if="s.revealed_by"> · 揭示于「{{ s.revealed_by }}」</template>
                  <template v-if="s.source"> · {{ sectionTitle(s.source.section) }}</template>
                </div>
              </div>
            </div>
          </div>
        </section>

        <section class="grid gap-4 lg:grid-cols-2">
          <!-- 卡侧 -->
          <div class="card card-border bg-base-100">
            <div class="card-body gap-2.5 p-5">
              <h2 class="card-title gap-2 text-sm font-medium">
                <Icon name="chat" :size="16" class="text-base-content/45" />
                卡（card.lua）
              </h2>
              <label class="form-control">
                <span class="label-text pb-1 text-xs text-base-content/60">开场白 first_mes</span>
                <textarea v-model="pack.card.first_mes" class="textarea textarea-bordered min-h-16 text-xs"></textarea>
              </label>
              <label class="form-control">
                <span class="label-text pb-1 text-xs text-base-content/60">剧本场景 scenario</span>
                <textarea v-model="pack.card.scenario" class="textarea textarea-bordered min-h-16 text-xs"></textarea>
              </label>
              <label class="form-control">
                <span class="label-text pb-1 text-xs text-base-content/60">性格 personality</span>
                <textarea v-model="pack.card.personality" class="textarea textarea-bordered min-h-16 text-xs"></textarea>
              </label>
              <div class="text-xs">
                <span class="text-base-content/60">示例对话 {{ pack.card.example_dialogue.length }} 组：</span>
                <div
                  v-for="(t, i) in pack.card.example_dialogue"
                  :key="i"
                  class="mt-1 rounded-box border border-base-200 px-2 py-1.5 text-[11px]"
                >
                  <span class="badge badge-ghost badge-xs">{{ t.tag || "未标" }}</span>
                  {{ t.messages.map((m) => `${m.role === "user" ? "我" : "她"}：${m.content}`).join(" / ").slice(0, 90) }}
                </div>
              </div>
            </div>
          </div>

          <!-- 世界线 + 事件 + 占位 + 史变 -->
          <div class="flex flex-col gap-4">
            <div v-if="pack.worldline" class="card card-border bg-base-100">
              <div class="card-body gap-2 p-5">
                <h2 class="card-title gap-2 text-sm font-medium">
                  <Icon name="clock" :size="16" class="text-base-content/45" />
                  世界线候选
                  <span class="text-xs font-normal text-base-content/50">{{ pack.worldline.premise }}</span>
                </h2>
                <div
                  v-for="st in pack.worldline.stages"
                  :key="st.id"
                  class="flex items-center gap-2 text-xs"
                >
                  <input v-model="st.include" type="checkbox" class="checkbox checkbox-sm" />
                  <span class="font-mono text-[10px] text-base-content/40">{{ st.id }}</span>
                  <span class="badge badge-ghost badge-xs">第 {{ st.day }} 天</span>
                  <span class="min-w-0 flex-1 truncate">{{ st.directive }}</span>
                </div>
              </div>
            </div>
            <div v-if="pack.events.length || pack.others.length" class="card card-border bg-base-100">
              <div class="card-body gap-2 p-5">
                <h2 class="card-title gap-2 text-sm font-medium">
                  <Icon name="layers" :size="16" class="text-base-content/45" />
                  事件与占位实体
                </h2>
                <div v-for="e in [...pack.events, ...pack.others]" :key="e.id" class="flex items-center gap-2 text-xs">
                  <input v-model="e.include" type="checkbox" class="checkbox checkbox-sm" />
                  <span class="font-mono text-[10px] text-base-content/40">{{ e.id }}</span>
                  <span class="min-w-0 flex-1 truncate">{{ e.one_liner || `（占位 · ${e.name}）` }}</span>
                </div>
              </div>
            </div>
            <div v-if="pack.versions.length" class="card card-border bg-base-100">
              <div class="card-body gap-2 p-5">
                <h2 class="card-title gap-2 text-sm font-medium">
                  <Icon name="refresh" :size="16" class="text-base-content/45" />
                  史变候选
                </h2>
                <div v-for="(v, i) in pack.versions" :key="i" class="flex items-center gap-2 text-xs">
                  <input v-model="v.include" type="checkbox" class="checkbox checkbox-sm" />
                  <span class="badge badge-ghost badge-xs">第 {{ v.day }} 天</span>
                  <span class="font-mono text-[10px] text-base-content/40">{{ v.facet }}</span>
                  <span class="min-w-0 flex-1 truncate">{{ fmtValue(v.value) }}</span>
                </div>
              </div>
            </div>
          </div>
        </section>

        <!-- 待定 + 质检 -->
        <section v-if="pack.pending.length || pack.qc.length" class="card card-border bg-base-100">
          <div class="card-body gap-2 p-5">
            <h2 class="card-title gap-2 text-sm font-medium">
              <Icon name="dots" :size="16" class="text-base-content/45" />
              待定与质检（{{ pack.pending.length + pack.qc.length }}）
            </h2>
            <div v-for="(p, i) in pack.pending" :key="`p${i}`" class="text-xs">
              <span class="badge badge-ghost badge-xs">{{ p.title }}</span>
              {{ p.detail }}
            </div>
            <div
              v-for="(q, i) in pack.qc"
              :key="`q${i}`"
              class="flex items-start gap-1.5 rounded-box px-2 py-1 text-xs"
              :class="q.severity === 'warn' ? 'bg-warning/10 text-warning' : 'text-base-content/60'"
            >
              <Icon :name="q.severity === 'warn' ? 'warning' : 'info'" :size="13" class="mt-0.5 flex-none" />
              <span>{{ q.at }}：{{ q.problem }}</span>
            </div>
          </div>
        </section>

        <!-- 切入点向导 -->
        <section class="card card-border border-primary/40 bg-primary/5">
          <div class="card-body gap-3 p-5">
            <h2 class="card-title gap-2 text-sm font-medium">
              <Icon name="play" :size="16" class="text-primary" />
              剧情切入点
            </h2>
            <p class="m-0 text-xs text-base-content/60">
              选一个可扮演的时间点：该时点未被揭示的秘密只有她本人知道；已死亡之后的时点会以
              「记忆体/残留」前提开场。之后新建会话时「第几天」填这个数即可从这里开局。
            </p>
            <div class="flex flex-col gap-2">
              <!-- E4：同一天可以有多个切入点（收束点/死亡点），按名取键 -->
              <label
                v-for="p in pack.canon_points"
                :key="p.name"
                class="flex cursor-pointer items-start gap-3 rounded-box border bg-base-100 px-3 py-2.5 text-xs"
                :class="chosenDay === p.day ? 'border-primary' : 'border-base-200'"
              >
                <input v-model="chosenDay" type="radio" :value="p.day" class="radio radio-primary radio-sm mt-0.5" />
                <div class="min-w-0 flex-1">
                  <div class="flex items-baseline gap-2">
                    <span class="font-medium">{{ p.name }}</span>
                    <span class="badge badge-ghost badge-xs">第 {{ p.day }} 天</span>
                    <span v-if="p.after_death" class="badge badge-warning badge-xs">死亡之后</span>
                  </div>
                  <div class="mt-0.5 text-[11px] text-base-content/50">{{ p.note }}</div>
                  <div v-if="p.premise" class="mt-0.5 text-[11px] text-warning/80">{{ p.premise }}</div>
                </div>
              </label>
            </div>
            <div class="flex flex-wrap gap-4 text-xs">
              <label class="flex cursor-pointer items-center gap-2">
                <input v-model="overwriteWorldline" type="checkbox" class="checkbox checkbox-sm" />
                覆盖已有世界主线（worldline.lua）
              </label>
              <label class="flex cursor-pointer items-center gap-2">
                <input v-model="setWorldDay" type="checkbox" class="checkbox checkbox-sm" />
                把世界时钟拨到切入点
              </label>
            </div>
            <div class="flex justify-between">
              <button class="btn btn-ghost btn-sm" @click="step = 3">上一步</button>
              <button class="btn btn-primary btn-sm" :disabled="!!busy" @click="doCommit">
                <Icon name="check" :size="14" />
                {{ busy === "commit" ? "落盘中…" : `确认落盘（切入点：第 ${chosenDay} 天）` }}
              </button>
            </div>
          </div>
        </section>
      </template>

      <!-- ⑤ 落盘报告 -->
      <section v-else-if="step === 5 && report" class="card card-border border-success/40 bg-success/5">
        <div class="card-body gap-3 p-5">
          <h2 class="card-title gap-2 text-sm font-medium">
            <Icon name="check" :size="16" class="text-success" />
            已落盘 · 可以开聊了
          </h2>
          <ul class="m-0 flex list-none flex-col gap-1 p-0 text-xs text-base-content/70">
            <li>卡：<code class="font-mono">{{ report.cardPath }}</code></li>
            <li>正史实体：{{ report.entitiesWritten.join("、") || "（无）" }}</li>
            <li>世界主线：{{ report.worldlineWritten ? "已写入" : "未写入（已有主线且未勾选覆盖）" }}</li>
            <li>切入点：第 {{ report.canonDay }} 天——新建会话时「第几天」填这个数</li>
          </ul>
          <div v-if="report.skipped.length" class="rounded-box border border-warning/40 bg-warning/5 p-3 text-xs">
            <div v-for="(s, i) in report.skipped" :key="i" class="flex items-start gap-1.5">
              <Icon name="skip" :size="13" class="mt-0.5 flex-none text-warning" />
              <span>{{ s }}</span>
            </div>
          </div>
          <div v-if="report.warnings.length" class="rounded-box border border-base-200 p-3 text-xs text-base-content/60">
            <div v-for="(w, i) in report.warnings" :key="i" class="flex items-start gap-1.5">
              <Icon name="info" :size="13" class="mt-0.5 flex-none" />
              <span>{{ w }}</span>
            </div>
          </div>
          <div class="flex gap-2">
            <button class="btn btn-primary btn-sm" @click="reset">再导入一份</button>
          </div>
        </div>
      </section>

      <!-- ST 世界书导入 -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-3 p-5">
          <h2 class="card-title gap-2 text-sm font-medium">
            <Icon name="database" :size="16" class="text-base-content/45" />
            ST 世界书导入（JSON → note 实体）
          </h2>
          <p class="m-0 text-xs text-base-content/50">
            SillyTavern 世界书逐条转为 note 实体：标题→name、关键词→别名（提及激活）、
            正文→注入文本；禁用条目按草稿收档。order/sticky/cooldown 留档不执行。
          </p>
          <div class="flex flex-wrap gap-3">
            <label class="form-control w-40">
              <span class="label-text pb-1 text-xs text-base-content/60">导入到世界</span>
              <input v-model="world" class="input input-sm input-bordered w-full" />
            </label>
            <label class="form-control w-52">
              <span class="label-text pb-1 text-xs text-base-content/60">书名（可选，用于实体 id）</span>
              <input v-model="wbBook" class="input input-sm input-bordered w-full" placeholder="夜城设定" />
            </label>
          </div>
          <textarea
            v-model="wbJson"
            class="textarea textarea-bordered min-h-24 w-full font-mono text-xs"
            placeholder='{"entries": { "0": { "key": ["夜市"], "content": "…", "comment": "夜市" } }}'
          ></textarea>
          <div class="flex items-center justify-between">
            <div v-if="wbReport" class="text-xs text-base-content/60">
              新增 {{ wbReport.imported }} · 禁用 {{ wbReport.disabled }} · 跳过 {{ wbReport.skipped }}
            </div>
            <span v-else></span>
            <button class="btn btn-sm" :disabled="wbBusy" @click="doImportWorldbook">
              {{ wbBusy ? "导入中…" : "导入世界书" }}
            </button>
          </div>
        </div>
      </section>
    </div>

    <!-- P0–P11 提示词套件（手动模式） -->
    <dialog class="modal" :open="promptsOpen" @click.self="promptsOpen = false">
      <div class="modal-box max-w-3xl">
        <div class="flex items-center justify-between">
          <h3 class="m-0 text-sm font-medium">P0–P11 提示词套件（手动模式）</h3>
          <div class="flex gap-2">
            <button class="btn btn-ghost btn-xs" @click="copyPrompts">
              <Icon name="copy" :size="13" />复制全文
            </button>
            <button class="btn btn-ghost btn-xs" @click="promptsOpen = false">
              <Icon name="close" :size="13" />
            </button>
          </div>
        </div>
        <pre class="mt-3 max-h-[60vh] overflow-auto whitespace-pre-wrap text-[11px] leading-relaxed">{{ promptsText }}</pre>
      </div>
      <form method="dialog" class="modal-backdrop"><button>close</button></form>
    </dialog>
  </div>
</template>
