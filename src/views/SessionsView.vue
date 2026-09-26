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
import type { CardSummary, Persona, ScriptSummary } from "../types";
import ErrorToast from "../components/ErrorToast.vue";
import EmptyState from "../components/EmptyState.vue";
import Icon from "../components/Icon.vue";
import SessionView from "./SessionView.vue";

// 会话页只放聊天本体：
// 会话列表在侧栏「会话」子菜单里，新建按钮在顶栏标题旁，这里负责承载新建弹窗。

const cards = ref<CardSummary[]>([]);
const personas = ref<Persona[]>([]);
const scripts = ref<ScriptSummary[]>([]);
const error = ref("");
const newEl = ref<HTMLDialogElement | null>(null);

const form = reactive({
  character: "",
  /** 群聊阵容（M3.1）：勾选的目录名；空 = 只用主角色 */
  extra: [] as string[],
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
  void loadScripts();
  // 从别处（资产页「用它开一场」等）带着打开标记进来：dialog 引用此刻才就绪，补一次打开
  if (newSessionOpen.value && !newEl.value?.open) newEl.value?.showModal();
});

async function loadCardsAndPersonas() {
  try {
    [cards.value, personas.value] = await Promise.all([api.listCards(), api.listPersonas()]);
    // 指定卡的优先级高于缺省第一张（资产页「用它开一场」直达）
    if (preferredCard.value && cards.value.some((c) => c.dir_name === preferredCard.value)) {
      form.character = preferredCard.value;
    } else if (!form.character && cards.value.length > 0) {
      form.character = cards.value[0].dir_name;
    }
  } catch {
    /* 卡/人格读取失败不阻塞弹窗 */
  } finally {
    preferredCard.value = "";
  }
}

async function loadScripts() {
  try {
    scripts.value = await api.listScripts();
  } catch {
    scripts.value = [];
  }
}

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

/** 主角色切换时把它从「同台」勾选里摘掉（不能自己陪自己） */
watch(
  () => form.character,
  (main) => {
    form.extra = form.extra.filter((d) => d !== main);
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
  if (!form.character) {
    error.value = "请选择角色卡。";
    return;
  }
  try {
    const characters = [form.character, ...form.extra];
    const meta = await api.newSession({
      character: form.character,
      characters: characters.length > 1 ? characters : undefined,
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

    <!-- 新建会话弹窗（由顶栏按钮打开） -->
    <dialog ref="newEl" class="modal" @close="closeNewSession">
      <div class="modal-box max-w-lg">
        <h3 class="text-base font-semibold">新建会话</h3>
        <p class="mt-1 text-xs text-base-content/50">选角色卡与用户人格，设定开场的时间与地点。</p>

        <form class="mt-4 flex flex-col gap-3" @submit.prevent="create">
          <div>
            <label class="label" for="new-character">角色卡 *</label>
            <select id="new-character" class="select select-sm w-full" v-model="form.character">
              <option v-for="c in cards" :key="c.dir_name" :value="c.dir_name">
                {{ c.name }}{{ c.degraded ? "（降级）" : "" }}
              </option>
            </select>
          </div>
          <!-- 群聊阵容（M3.1）：勾选同台角色——多角色会话按角色隔离组装 -->
          <div v-if="cards.length > 1">
            <span class="label">同台角色（群聊，可多选）</span>
            <div class="flex flex-wrap gap-x-4 gap-y-1 rounded-box bg-base-200 px-3 py-2">
              <label
                v-for="c in cards.filter((x) => x.dir_name !== form.character)"
                :key="c.dir_name"
                class="flex cursor-pointer items-center gap-1.5 text-xs"
              >
                <input type="checkbox" class="checkbox checkbox-xs" :value="c.dir_name" v-model="form.extra" />
                {{ c.name }}{{ c.degraded ? "（降级）" : "" }}
              </label>
            </div>
          </div>
          <div>
            <label class="label" for="new-persona">用户人格</label>
            <select id="new-persona" class="select select-sm w-full" v-model="form.persona">
              <option value="">（不使用）</option>
              <option v-for="p in personas" :key="p.name" :value="p.name">{{ p.name }}</option>
            </select>
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
            <button class="btn btn-primary btn-sm" type="submit">创建</button>
          </div>
        </form>
      </div>
      <form method="dialog" class="modal-backdrop">
        <button>关闭</button>
      </form>
    </dialog>
  </div>
</template>
