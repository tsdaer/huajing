<script setup lang="ts">
import { onMounted, reactive, ref, watch } from "vue";
import { api } from "../api";
import { cardGeneration, importOpen } from "../cards";
import {
  closeNewSession,
  loadSessions,
  newSessionOpen,
  openNewSession,
  preferredCard,
  selectedSession,
  selectSession,
} from "../sessions";
import type { CardSummary, Persona, ScriptSummary, WorldSummary } from "../types";
import ErrorToast from "../components/ErrorToast.vue";
import EmptyState from "../components/EmptyState.vue";
import Icon from "../components/Icon.vue";
import { avatarStyle, initial } from "../avatar";
import SessionView from "./SessionView.vue";

// 会话页只放聊天本体：
// 会话列表在侧栏「会话」子菜单里，新建按钮在顶栏标题旁，这里负责承载新建弹窗。

const cards = ref<CardSummary[]>([]);
const personas = ref<Persona[]>([]);
const scripts = ref<ScriptSummary[]>([]);
const worlds = ref<WorldSummary[]>([]);
const error = ref("");
const newEl = ref<HTMLDialogElement | null>(null);

/** 角色阵容（M5.3）：卡片点选，保序——首个是主角色（默认发言人） */
const cast = ref<string[]>([]);

const form = reactive({
  /** 设定集（世界）："" = 缺省 default；主角色变更时预填其卡声明的世界（若真实存在） */
  world: "",
  persona: "",
  /** 剧本模板（M4.1）：选中即预填起因/天/时间/地点，模板里的实体状态随会话生效 */
  script: "",
  /** 第几天（M3.7：空 = 从世界时钟出发，回写是 max 不会拉低世界） */
  day: null as number | null,
  clock: "",
  place: "",
  premise: "",
});

onMounted(async () => {
  void loadSessions();
  await loadCardsAndPersonas();
  void loadWorlds();
  void loadScripts();
  // 从别处（资产页「用它开一场」等）带着打开标记进来：dialog 引用此刻才就绪，补一次打开
  if (newSessionOpen.value && !newEl.value?.open) newEl.value?.showModal();
});

async function loadCardsAndPersonas() {
  try {
    [cards.value, personas.value] = await Promise.all([api.listCards(), api.listPersonas()]);
    // 指定卡的优先级高于缺省第一张（资产页「用它开一场」直达）
    if (preferredCard.value && cards.value.some((c) => c.dir_name === preferredCard.value)) {
      cast.value = [preferredCard.value];
    } else if (cast.value.length === 0 && cards.value.length > 0) {
      cast.value = [cards.value[0].dir_name];
    }
  } catch {
    /* 卡/人格读取失败不阻塞弹窗 */
  } finally {
    preferredCard.value = "";
  }
}

async function loadWorlds() {
  try {
    worlds.value = await api.listWorlds();
  } catch {
    worlds.value = [];
  }
}

async function loadScripts() {
  try {
    scripts.value = await api.listScripts();
  } catch {
    scripts.value = [];
  }
}

/** 阵容卡片点选：进了就摘、没进就追加（保序） */
function toggleCast(dir: string) {
  const i = cast.value.indexOf(dir);
  if (i >= 0) cast.value.splice(i, 1);
  else cast.value.push(dir);
}

/** 设为主角色：挪到阵容首位（默认发言人 + 开场白归她） */
function promoteCast(dir: string) {
  const i = cast.value.indexOf(dir);
  if (i > 0) {
    cast.value.splice(i, 1);
    cast.value.unshift(dir);
  }
}

function castName(dir: string): string {
  return cards.value.find((c) => c.dir_name === dir)?.name || dir;
}

/** 主角色变更：预填该卡声明的世界（只在世界真实存在时生效，否则保持缺省） */
watch(
  () => cast.value[0],
  async (main) => {
    if (!main) return;
    try {
      const d = await api.getCard(main);
      const w = d.card.world?.trim() || "";
      form.world = worlds.value.some((x) => x.name === w) ? w : "";
    } catch {
      /* 卡读取失败不动 world */
    }
  },
);

/** 选剧本：预填起因/天/时间/地点（用户仍可改；实体作用域状态由后端随会话生效） */
watch(
  () => form.script,
  async (name) => {
    if (!name) return;
    try {
      const tpl = await api.getScript(name);
      form.premise = tpl.premise || form.premise;
      const bb = tpl.blackboard;
      if (bb) {
        if (bb.day >= 1) form.day = bb.day;
        if (bb.clock) form.clock = bb.clock;
        if (bb.place) form.place = bb.place;
      }
    } catch {
      /* 模板读取失败不阻塞向导 */
    }
  },
);

// 导入完成（应用级弹窗发出）：卡片清单变了，新建会话的下拉要跟上
watch(cardGeneration, () => void loadCardsAndPersonas());

/** 顶栏按钮与原生 dialog 双向同步：Esc、点遮罩关闭时也要把状态收回来 */
watch(newSessionOpen, (open) => {
  const el = newEl.value;
  if (!el) return;
  if (open && !el.open) el.showModal();
  else if (!open && el.open) el.close();
});

async function create() {
  const main = cast.value[0];
  if (!main) {
    error.value = "请先点选一张角色卡作为主角色。";
    return;
  }
  try {
    const meta = await api.newSession({
      character: main,
      characters: cast.value.length > 1 ? cast.value : undefined,
      world: form.world || undefined,
      persona: form.persona || undefined,
      script: form.script || undefined,
      day: form.day || undefined,
      clock: form.clock || undefined,
      place: form.place || undefined,
      premise: form.premise || undefined,
    });
    closeNewSession();
    await loadSessions();
    selectSession(meta.id);
    error.value = "";
  } catch (e) {
    error.value = String(e);
  }
}
</script>

<template>
  <div class="h-full min-h-0 p-4 lg:p-6">
    <ErrorToast :message="error" @dismiss="error = ''" />

    <div class="mx-auto flex h-full min-h-0 w-full max-w-[1400px] flex-col">
      <!-- B4：:key 强制按会话重建组件，快速切换时旧实例的迟到写入不可能串台 -->
      <SessionView v-if="selectedSession" :key="selectedSession.id" :meta="selectedSession" />

      <EmptyState
        v-else
        icon="chat"
        title="挑一场戏，接着往下演"
        desc="在左侧「会话」里选一场，或者新建一场。"
        class="flex-1"
      >
        <button class="btn btn-primary btn-sm" @click="openNewSession">
          <Icon name="plus" :size="15" />新建会话
        </button>
        <button class="btn btn-sm" @click="importOpen = true">
          <Icon name="database" :size="15" />导入 ST 卡
        </button>
      </EmptyState>
    </div>

    <!-- 新建会话弹窗（由顶栏按钮打开）：卡片选角 + 阵容排序 + 设定集（M5.3） -->
    <dialog ref="newEl" class="modal" @close="closeNewSession">
      <div class="modal-box max-w-3xl">
        <h3 class="text-base font-semibold">新建会话</h3>
        <p class="mt-1 text-xs text-base-content/50">
          点选角色组阵容（再点主角色卡可加同台），设定启用的世界与开场的时间地点。
        </p>

        <form class="mt-4 flex flex-col gap-3" @submit.prevent="create">
          <!-- 角色阵容：卡片多选（点击进/出阵容），首位 = 主角色 -->
          <div>
            <span class="label">角色阵容 *</span>
            <div class="grid grid-cols-2 gap-2 sm:grid-cols-3">
              <button
                v-for="c in cards"
                :key="c.dir_name"
                type="button"
                class="flex items-center gap-2 rounded-box border px-2.5 py-2 text-left transition-colors"
                :class="
                  cast.includes(c.dir_name)
                    ? 'border-primary bg-primary/10'
                    : 'border-base-300 hover:border-base-content/25'
                "
                :aria-pressed="cast.includes(c.dir_name)"
                @click="toggleCast(c.dir_name)"
              >
                <span
                  class="flex size-8 shrink-0 items-center justify-center rounded-full text-xs font-medium"
                  :style="avatarStyle(c.name)"
                >
                  {{ initial(c.name) }}
                </span>
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-xs font-medium">
                    {{ c.name }}{{ c.degraded ? "（降级）" : "" }}
                  </span>
                  <span class="block text-[10px] text-base-content/45">
                    {{ cast[0] === c.dir_name ? "主角色" : cast.includes(c.dir_name) ? "同台" : "点击选入" }}
                  </span>
                </span>
                <span
                  v-if="cast[0] === c.dir_name"
                  class="badge badge-xs badge-primary"
                  title="主角色：默认发言人与开场白"
                >
                  主
                </span>
              </button>
            </div>
            <!-- 阵容顺序 chips：序号即出场顺序，可提升主角色 / 移出 -->
            <div v-if="cast.length > 0" class="mt-2 flex flex-wrap items-center gap-1.5">
              <span class="text-[11px] text-base-content/45">
                {{ cast.length > 1 ? "出场顺序（群聊）："
                : "阵容：" }}
              </span>
              <span
                v-for="(dir, i) in cast"
                :key="dir"
                class="badge badge-soft badge-sm gap-1"
                :class="i === 0 ? 'badge-primary' : ''"
              >
                {{ i + 1 }}. {{ castName(dir) }}
                <button
                  v-if="i > 0"
                  type="button"
                  class="tooltip tooltip-bottom"
                  data-tip="设为主角色"
                  :aria-label="`设 ${castName(dir)} 为主角色`"
                  @click="promoteCast(dir)"
                >
                  <Icon name="chevron" :size="10" class="rotate-180" />
                </button>
                <button
                  type="button"
                  class="tooltip tooltip-bottom"
                  data-tip="移出阵容"
                  :aria-label="`移出 ${castName(dir)}`"
                  @click="toggleCast(dir)"
                >
                  <Icon name="close" :size="10" />
                </button>
              </span>
            </div>
          </div>

          <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <!-- 设定集（世界）：决定 B3 注入哪个世界的实体与世界时钟基准 -->
            <div>
              <label class="label" for="new-world">
                设定集<span class="ml-1 text-base-content/45">（世界：实体注入与世界时钟的来源）</span>
              </label>
              <select id="new-world" class="select select-sm w-full" v-model="form.world">
                <option value="">（缺省：default）</option>
                <option v-for="w in worlds" :key="w.name" :value="w.name">
                  {{ w.name }} · {{ w.entities }} 实体 · 第{{ w.day }}天{{ w.has_worldline ? " · 有主线" : "" }}
                </option>
              </select>
            </div>
            <div>
              <label class="label" for="new-persona">用户人格</label>
              <select id="new-persona" class="select select-sm w-full" v-model="form.persona">
                <option value="">（不使用）</option>
                <option v-for="p in personas" :key="p.name" :value="p.name">{{ p.name }}</option>
              </select>
            </div>
          </div>
          <!-- 剧本（M4.1）：包导入落进 scripts/ 的会话模板；选中即预填起因与开局局面 -->
          <div v-if="scripts.length > 0">
            <label class="label" for="new-script">
              剧本<span class="ml-1 text-base-content/45">（开局模板：起因 + 初始局面 + 导演树）</span>
            </label>
            <select id="new-script" class="select select-sm w-full" v-model="form.script">
              <option value="">（不用剧本）</option>
              <option v-for="s in scripts" :key="s.name" :value="s.name">
                {{ s.name }}{{ s.has_director ? " · 有导演树" : "" }}
              </option>
            </select>
          </div>
          <div class="grid grid-cols-2 gap-3">
            <div>
              <label class="label" for="new-day">第几天</label>
              <input
                id="new-day"
                class="input input-sm w-full"
                v-model.number="form.day"
                type="number"
                min="1"
                placeholder="续接世界时钟"
              />
            </div>
            <div>
              <label class="label" for="new-clock">时间（HH:MM）</label>
              <input id="new-clock" class="input input-sm w-full" v-model="form.clock" placeholder="21:30" />
            </div>
          </div>
          <div>
            <label class="label" for="new-place">地点</label>
            <input id="new-place" class="input input-sm w-full" v-model="form.place" placeholder="图书馆自习区" />
          </div>
          <div>
            <label class="label" for="new-premise">起因（premise，可空）</label>
            <input
              id="new-premise"
              class="input input-sm w-full"
              v-model="form.premise"
              placeholder="闭馆前的一小时"
            />
          </div>

          <div class="modal-action">
            <button class="btn btn-sm" type="button" @click="closeNewSession">取消</button>
            <button class="btn btn-primary btn-sm" type="submit" :disabled="cast.length === 0">创建</button>
          </div>
        </form>
      </div>
      <form method="dialog" class="modal-backdrop">
        <button>关闭</button>
      </form>
    </dialog>
  </div>
</template>
