<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { api } from "../api";
import { assetTab } from "../assets";
import { cardGeneration, importOpen } from "../cards";
import { loadSessions, preferredCard, selectSession, sessions } from "../sessions";
import type { CardDetail, CardSummary, InspectorEntity, WorldSummary } from "../types";
import EmptyState from "../components/EmptyState.vue";
import ErrorToast from "../components/ErrorToast.vue";
import Icon from "../components/Icon.vue";
import { statusClass, statusLabel } from "../components/inspector/util";

// 资产页：角色与设定集的集中查看入口。
// 角色：卡片墙 + 详情弹窗 + 导入 +「用它开一场」；
// 设定集（M5.2）：世界卡片网格 → 下钻实体清单，按世界分组、不借道会话；
// 补全 / 史变等写操作仍在会话检查器里做，这里给出去的指引。

const emit = defineEmits<{ go: [view: "sessions"] }>();

// ---------- 角色 ----------
const cards = ref<CardSummary[]>([]);
const loading = ref(false);
const error = ref("");

/** 卡目录为空或读取失败时静默 */
async function loadCards() {
  loading.value = true;
  try {
    cards.value = await api.listCards();
  } catch {
    cards.value = [];
  } finally {
    loading.value = false;
  }
}

// 卡片热加载（M1.7）：DataHub 里的卡变了就重扫
watch(cardGeneration, () => void loadCards());

const hooks = computed(() => cards.value.filter((c) => c.has_hooks).length);
const degraded = computed(() => cards.value.filter((c) => c.degraded).length);

function initial(name: string): string {
  return name.trim().slice(0, 1) || "?";
}

/** 「用它开一场」：记下指定卡，跳会话页——新建弹窗会落到该卡 */
function startWith(dirName: string) {
  preferredCard.value = dirName;
  emit("go", "sessions");
}

// ---------- 导出（M4.1 · 三包与 ST 世界书） ----------
const exporting = ref("");
const exportNotice = ref("");

/** 通用导出执行器：成功把产物路径展示成提示，失败进错误提示 */
async function runExport(key: string, fn: () => Promise<{ path: string; name: string }>) {
  exporting.value = key;
  exportNotice.value = "";
  error.value = "";
  try {
    const out = await fn();
    exportNotice.value = `已导出「${out.name}」→ ${out.path}`;
  } catch (e) {
    error.value = String(e);
  } finally {
    exporting.value = "";
  }
}

const exportCard = (dirName: string) =>
  runExport(`card:${dirName}`, () => api.exportCardPack(dirName));
const exportWorld = (world: string) =>
  runExport(`world:${world}`, () => api.exportWorldPack(world));
const exportWorldbookSt = (world: string) =>
  runExport(`wb:${world}`, () => api.exportWorldbookSt(world));

// 卡片详情弹窗（getCard：完整卡面 + 出场状态 + hooks 清单）
const detail = ref<CardDetail | null>(null);
const detailBusy = ref("");

async function openDetail(dirName: string) {
  detailBusy.value = dirName;
  detail.value = null;
  try {
    detail.value = await api.getCard(dirName);
  } catch {
    detail.value = null;
  } finally {
    detailBusy.value = "";
  }
}

/** 出场状态的值：对象压 JSON，其余原样；空值不显示 */
function stateText(v: unknown): string {
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

// ---------- 设定集（M5.2 世界浏览）：世界卡片网格 → 下钻实体清单，不借道会话 ----------
const worlds = ref<WorldSummary[]>([]);
const worldsBusy = ref(false);
/** 空 = 网格视图；非空 = 下钻浏览该世界的实体 */
const selectedWorld = ref("");
const worldEntities = ref<InspectorEntity[]>([]);
const worldBusy = ref(false);
const worldError = ref("");
const query = ref("");

async function loadWorlds() {
  worldsBusy.value = true;
  worldError.value = "";
  try {
    worlds.value = await api.listWorlds();
  } catch (e) {
    worlds.value = [];
    worldError.value = String(e);
  } finally {
    worldsBusy.value = false;
  }
}

/** 下钻某个世界（重按同键 = 刷新） */
async function openWorld(name: string) {
  selectedWorld.value = name;
  worldBusy.value = true;
  worldError.value = "";
  query.value = "";
  try {
    worldEntities.value = await api.codexWorldEntities(name);
  } catch (e) {
    worldEntities.value = [];
    worldError.value = String(e);
  } finally {
    worldBusy.value = false;
  }
}

const filteredEntities = computed(() => {
  const q = query.value.trim().toLowerCase();
  if (!q) return worldEntities.value;
  return worldEntities.value.filter(
    (e) =>
      e.name.toLowerCase().includes(q) ||
      e.id.toLowerCase().includes(q) ||
      e.type.toLowerCase().includes(q) ||
      e.oneLiner.toLowerCase().includes(q),
  );
});

/** 类型徽标中文（与 codex.rs type_cn 同口径） */
function typeCn(ty: string): string {
  const map: Record<string, string> = {
    char: "人",
    place: "地",
    item: "物",
    event: "事",
    org: "组织",
    rule: "规则",
    concept: "概念",
    note: "笔记",
  };
  return map[ty] ?? ty;
}

/** 世界卡片上类型分布的前三名（chips） */
function topTypes(w: WorldSummary): [string, number][] {
  return Object.entries(w.by_type)
    .sort((a, b) => b[1] - a[1])
    .slice(0, 3);
}

/** 去一场同世界会话的检查器做补全 / 史变等写操作（没有同世界会话就落到最近一场） */
function openInSession() {
  const hit =
    sessions.value.find((s) => (s.world || "default") === selectedWorld.value) ??
    sessions.value[0];
  if (!hit) return;
  selectSession(hit.id);
  emit("go", "sessions");
}

onMounted(async () => {
  void loadCards();
  // 世界浏览不依赖会话；会话清单只服务「去会话检查器」的落点
  void loadSessions();
  void loadWorlds();
});
</script>

<template>
  <div class="h-full overflow-y-auto p-4 lg:p-6">
    <div class="mx-auto flex w-full max-w-[1200px] flex-col gap-4">
      <!-- 页签：角色 / 设定集（侧栏子条目与本页共享状态） -->
      <div role="tablist" class="tabs tabs-box tabs-sm w-fit flex-none">
        <button
          role="tab"
          class="tab gap-1.5"
          :class="{ 'tab-active': assetTab === 'characters' }"
          @click="assetTab = 'characters'"
        >
          <Icon name="users" :size="14" />角色
          <span class="badge badge-xs badge-ghost">{{ cards.length }}</span>
        </button>
        <button
          role="tab"
          class="tab gap-1.5"
          :class="{ 'tab-active': assetTab === 'codex' }"
          @click="assetTab = 'codex'"
        >
          <Icon name="book" :size="14" />设定集
        </button>
      </div>

      <!-- 导出结果 / 页面级错误 -->
      <div v-if="exportNotice" class="alert alert-success alert-soft py-2 text-xs" role="status">
        <span class="break-all">{{ exportNotice }}</span>
        <button class="btn btn-ghost btn-xs" @click="exportNotice = ''">知道了</button>
      </div>
      <ErrorToast :message="error" @dismiss="error = ''" />

      <Transition name="fade" mode="out-in">
        <!-- ============ 角色 ============ -->
        <section v-if="assetTab === 'characters'" key="characters" class="flex flex-col gap-4">
          <div class="flex flex-wrap items-center justify-between gap-2">
            <div class="flex flex-wrap items-center gap-2 text-sm font-medium">
              角色卡
              <span v-if="hooks > 0" class="badge badge-sm badge-soft badge-primary">{{ hooks }} 张带 hooks</span>
              <span v-if="degraded > 0" class="badge badge-sm badge-soft badge-error">{{ degraded }} 张降级</span>
            </div>
            <div class="flex items-center gap-1.5">
              <button class="btn btn-ghost btn-sm" :disabled="loading" @click="loadCards">
                <span v-if="loading" class="loading loading-spinner loading-xs"></span>
                <Icon v-else name="refresh" :size="15" />刷新
              </button>
              <button class="btn btn-primary btn-sm" @click="importOpen = true">
                <Icon name="plus" :size="15" />导入角色卡
              </button>
            </div>
          </div>

          <div v-if="cards.length > 0" class="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4">
            <article
              v-for="c in cards"
              :key="c.dir_name"
              v-tilt
              class="card card-border bg-base-100 transition-colors hover:border-primary/40"
            >
              <div class="card-body gap-3 p-5">
                <div class="flex items-start gap-3">
                  <span
                    class="flex size-10 shrink-0 items-center justify-center rounded-box bg-primary/15 text-base font-medium text-primary"
                  >
                    {{ initial(c.name) }}
                  </span>
                  <div class="min-w-0 flex-1">
                    <h3 class="truncate text-sm font-semibold">{{ c.name }}</h3>
                    <p class="truncate font-mono text-[11px] text-base-content/45">{{ c.dir_name }}</p>
                  </div>
                </div>

                <p class="m-0 text-xs text-base-content/50">
                  {{ c.creator ? `作者 ${c.creator}` : "未署名卡片" }}
                </p>

                <div class="flex flex-wrap gap-1">
                  <span v-if="c.degraded" class="badge badge-sm badge-soft badge-error">降级</span>
                  <span v-if="c.has_hooks" class="badge badge-sm badge-soft badge-primary">hooks</span>
                  <span v-for="t in c.tags" :key="t" class="badge badge-sm badge-ghost">{{ t }}</span>
                  <span
                    v-if="!c.degraded && !c.has_hooks && c.tags.length === 0"
                    class="text-[11px] text-base-content/40"
                  >
                    静态卡
                  </span>
                </div>

                <div class="mt-auto flex items-center gap-1.5 pt-1">
                  <button
                    class="btn btn-xs"
                    :disabled="detailBusy === c.dir_name"
                    @click="openDetail(c.dir_name)"
                  >
                    <span v-if="detailBusy === c.dir_name" class="loading loading-spinner loading-xs"></span>
                    <Icon v-else name="eye" :size="13" />详情
                  </button>
                  <button class="btn btn-primary btn-xs flex-1" @click="startWith(c.dir_name)">
                    <Icon name="chat" :size="13" />用它开一场
                  </button>
                  <button
                    class="btn btn-xs tooltip tooltip-left"
                    data-tip="导出角色包（zip，可分享给其他化境用户）"
                    :disabled="exporting === `card:${c.dir_name}`"
                    @click="exportCard(c.dir_name)"
                  >
                    <span v-if="exporting === `card:${c.dir_name}`" class="loading loading-spinner loading-xs"></span>
                    <Icon v-else name="download" :size="13" />包
                  </button>
                </div>
              </div>
            </article>
          </div>

          <EmptyState
            v-else
            icon="users"
            title="还没有角色卡"
            desc="把 SillyTavern 的 PNG / JSON 卡拖进窗口，或放进 DataHub/characters/ 后点刷新。"
          >
            <button class="btn btn-primary btn-sm" @click="importOpen = true">
              <Icon name="plus" :size="15" />导入角色卡
            </button>
          </EmptyState>
        </section>

        <!-- ============ 设定集（M5.2：按世界分组的卡片网格 → 下钻浏览） ============ -->
        <section v-else key="codex" class="flex flex-col gap-4">
          <div v-if="worldError" role="alert" class="alert alert-error alert-soft text-xs break-words">
            {{ worldError }}
          </div>

          <!-- 世界网格 -->
          <template v-if="!selectedWorld">
            <div v-if="worldsBusy" class="card card-border bg-base-100">
              <div class="card-body flex flex-col gap-3 p-5">
                <div class="skeleton h-4 w-44"></div>
                <div class="skeleton h-3 w-full"></div>
                <div class="skeleton h-3 w-4/5"></div>
              </div>
            </div>
            <EmptyState
              v-else-if="worlds.length === 0"
              icon="globe"
              title="还没有世界"
              desc="导入 SillyTavern 世界书，或让总结管线提案之后，世界就会在这里出现。"
            />
            <div v-else class="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-3">
              <article
                v-for="w in worlds"
                :key="w.name"
                v-tilt
                class="card card-border cursor-pointer bg-base-100 transition-colors hover:border-primary/40"
                @click="openWorld(w.name)"
              >
                <div class="card-body flex flex-col gap-3 p-5">
                  <div class="flex items-center gap-2">
                    <Icon name="globe" :size="18" class="flex-none text-primary/70" />
                    <span class="min-w-0 flex-1 truncate text-base font-semibold">{{ w.name }}</span>
                    <span
                      v-if="w.has_worldline"
                      class="badge badge-xs badge-soft badge-secondary tooltip tooltip-left"
                      data-tip="这个世界配置了世界主线（worldline.lua）"
                    >
                      <Icon name="film" :size="10" />主线
                    </span>
                  </div>
                  <div class="flex flex-wrap items-center gap-1.5">
                    <span class="badge badge-sm badge-soft font-mono">{{ w.entities }} 实体</span>
                    <span class="badge badge-sm badge-ghost">第 {{ w.day }} 天</span>
                    <span v-for="[ty, n] in topTypes(w)" :key="ty" class="badge badge-sm badge-ghost">
                      {{ typeCn(ty) }}×{{ n }}
                    </span>
                  </div>
                  <p class="m-0 text-xs text-base-content/45">点开浏览实体；补全 / 史变等写操作在会话检查器。</p>
                </div>
              </article>
            </div>
          </template>

          <!-- 下钻：单世界实体浏览 -->
          <template v-else>
            <div class="flex flex-wrap items-center gap-2">
              <button class="btn btn-ghost btn-sm" @click="selectedWorld = ''">
                <Icon name="chevron" :size="14" class="rotate-180" />全部世界
              </button>
              <Icon name="globe" :size="16" class="flex-none text-base-content/45" />
              <span class="text-sm font-semibold">{{ selectedWorld }}</span>
              <span class="badge badge-sm badge-soft font-mono">{{ worldEntities.length }} 个实体</span>
              <label class="input input-sm ml-auto flex w-full max-w-60 items-center gap-2">
                <Icon name="search" :size="14" class="text-base-content/40" />
                <input v-model="query" type="text" class="grow" placeholder="搜名称 / id / 一句话" />
              </label>
              <button class="btn btn-ghost btn-sm" :disabled="worldBusy" @click="openWorld(selectedWorld)">
                <span v-if="worldBusy" class="loading loading-spinner loading-xs"></span>
                <Icon v-else name="refresh" :size="15" />刷新
              </button>
              <!-- 世界包与 ST 世界书导出（M4.1）：按当前下钻的世界名 -->
              <button
                class="btn btn-ghost btn-sm tooltip tooltip-left"
                data-tip="整包导出这个世界（实体 + 正史增量 + 世界时钟 + 世界线），可导入其他化境"
                :disabled="exporting === `world:${selectedWorld}`"
                @click="exportWorld(selectedWorld)"
              >
                <span v-if="exporting === `world:${selectedWorld}`" class="loading loading-spinner loading-xs"></span>
                <Icon v-else name="download" :size="15" />世界包
              </button>
              <button
                class="btn btn-ghost btn-sm tooltip tooltip-left"
                data-tip="反向导出为 SillyTavern 世界书 JSON（仅静态字段，§6.10）"
                :disabled="exporting === `wb:${selectedWorld}`"
                @click="exportWorldbookSt(selectedWorld)"
              >
                <span v-if="exporting === `wb:${selectedWorld}`" class="loading loading-spinner loading-xs"></span>
                <Icon v-else name="download" :size="15" />ST 世界书
              </button>
              <button
                v-if="sessions.length > 0"
                class="btn btn-ghost btn-sm tooltip tooltip-left"
                data-tip="补全 / 史变预览等写操作在那场会话的检查器里"
                @click="openInSession"
              >
                去会话检查器<Icon name="arrow" :size="14" />
              </button>
            </div>

            <!-- 读取中：骨架占位 -->
            <div v-if="worldBusy" class="card card-border bg-base-100">
              <div class="card-body flex flex-col gap-3 p-5">
                <div class="skeleton h-4 w-44"></div>
                <div class="skeleton h-3 w-full"></div>
                <div class="skeleton h-3 w-4/5"></div>
                <div class="skeleton h-3 w-3/5"></div>
              </div>
            </div>

            <div v-else class="card card-border bg-base-100">
              <div class="card-body flex flex-col gap-3 p-5">
                <p v-if="worldEntities.length > 0" class="m-0 text-xs text-base-content/45">
                  草稿不进注入，只有正史参与激活；retired 留档可查。
                </p>

                <div
                  v-for="e in filteredEntities"
                  :key="e.id"
                  class="rounded-box flex flex-col gap-1 bg-base-200 p-3"
                >
                  <div class="flex flex-wrap items-center gap-1.5">
                    <span class="min-w-0 flex-1 truncate text-sm font-medium">{{ e.name }}</span>
                    <span class="badge badge-xs badge-soft badge-neutral">{{ typeCn(e.type) }}</span>
                    <span class="badge badge-xs" :class="statusClass(e.status)">{{ statusLabel(e.status) }}</span>
                  </div>
                  <p v-if="e.oneLiner" class="m-0 text-xs break-words text-base-content/60">{{ e.oneLiner }}</p>
                  <p v-if="e.anchors.length" class="m-0 text-[11px] break-words text-base-content/40">
                    anchors：{{ e.anchors.join(" · ") }}
                  </p>
                  <p class="m-0 font-mono text-[10px] text-base-content/35">{{ e.id }}</p>
                </div>

                <p
                  v-if="worldEntities.length > 0 && filteredEntities.length === 0"
                  class="m-0 text-xs text-base-content/45"
                >
                  没有匹配「{{ query }}」的实体。
                </p>

                <EmptyState
                  v-if="worldEntities.length === 0"
                  icon="globe"
                  title="这个世界还没有实体"
                  desc="导入世界书，或让总结管线提案之后就有了。"
                />
              </div>
            </div>
          </template>
        </section>
      </Transition>
    </div>

    <!-- 卡片详情弹窗 -->
    <div v-if="detail" class="modal modal-open">
      <div class="modal-box max-w-2xl">
        <div class="flex items-start gap-3">
          <span
            class="flex size-11 shrink-0 items-center justify-center rounded-box bg-primary/15 text-base font-medium text-primary"
          >
            {{ initial(detail.card.name) }}
          </span>
          <div class="min-w-0 flex-1">
            <h3 class="truncate text-base font-semibold">{{ detail.card.name }}</h3>
            <p class="m-0 truncate font-mono text-[11px] text-base-content/45">{{ detail.dir_name }}</p>
          </div>
          <button class="btn btn-circle btn-ghost btn-sm" aria-label="关闭详情" @click="detail = null">
            <Icon name="close" :size="15" />
          </button>
        </div>

        <div class="mt-3 flex flex-wrap items-center gap-1">
          <span v-if="detail.card.world" class="badge badge-sm badge-soft badge-secondary">{{ detail.card.world }}</span>
          <span v-if="detail.card.creator" class="badge badge-sm badge-ghost">作者 {{ detail.card.creator }}</span>
          <span v-if="detail.degraded" class="badge badge-sm badge-soft badge-error" :title="detail.degrade_reason ?? ''">
            降级
          </span>
          <span v-for="t in detail.card.tags" :key="t" class="badge badge-sm badge-ghost">{{ t }}</span>
        </div>

        <div class="mt-4 flex flex-col gap-4 text-sm">
          <div v-if="detail.card.scenario">
            <p class="m-0 text-[11px] font-medium tracking-widest text-base-content/45">场景</p>
            <p class="m-0 mt-1 whitespace-pre-wrap text-xs leading-relaxed text-base-content/80">
              {{ detail.card.scenario }}
            </p>
          </div>
          <div v-if="detail.card.personality">
            <p class="m-0 text-[11px] font-medium tracking-widest text-base-content/45">人设</p>
            <p class="m-0 mt-1 whitespace-pre-wrap text-xs leading-relaxed text-base-content/80">
              {{ detail.card.personality }}
            </p>
          </div>
          <div v-if="detail.card.first_mes">
            <p class="m-0 text-[11px] font-medium tracking-widest text-base-content/45">开场白</p>
            <p class="m-0 mt-1 whitespace-pre-wrap rounded-box bg-base-200 p-3 text-xs leading-relaxed text-base-content/80">
              {{ detail.card.first_mes }}
            </p>
          </div>
          <div v-if="detail.card.example_dialogue.length > 0">
            <p class="m-0 text-[11px] font-medium tracking-widest text-base-content/45">
              示例对话 · {{ detail.card.example_dialogue.length }} 组
            </p>
          </div>
          <div v-if="detail.hook_names.length > 0">
            <p class="m-0 text-[11px] font-medium tracking-widest text-base-content/45">hooks</p>
            <div class="mt-1 flex flex-wrap gap-1">
              <span v-for="h in detail.hook_names" :key="h" class="badge badge-sm badge-soft badge-primary font-mono">
                {{ h }}
              </span>
            </div>
          </div>
          <div v-if="Object.keys(detail.default_state).length > 0">
            <p class="m-0 text-[11px] font-medium tracking-widest text-base-content/45">出场状态</p>
            <div class="mt-1 flex flex-col gap-1">
              <div
                v-for="(v, k) in detail.default_state"
                :key="k"
                class="flex items-baseline gap-2 rounded-box bg-base-200 px-3 py-1.5"
              >
                <span class="flex-none font-mono text-[11px] text-base-content/55">{{ k }}</span>
                <span class="min-w-0 flex-1 truncate font-mono text-[11px] text-base-content/75" :title="stateText(v)">
                  {{ stateText(v) }}
                </span>
              </div>
            </div>
          </div>
        </div>

        <div class="modal-action">
          <button class="btn btn-sm" @click="detail = null">关闭</button>
          <button class="btn btn-primary btn-sm" @click="startWith(detail.dir_name)">
            <Icon name="chat" :size="15" />用它开一场
          </button>
        </div>
      </div>
      <div class="modal-backdrop" @click="detail = null"></div>
    </div>
  </div>
</template>
