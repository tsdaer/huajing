<script setup lang="ts">
import { onMounted, reactive, ref, watch } from "vue";
import { api } from "../api";
import {
  closeNewSession,
  loadSessions,
  newSessionOpen,
  openNewSession,
  selectedSession,
  selectSession,
} from "../sessions";
import type { CardSummary, Persona } from "../types";
import ErrorToast from "../components/ErrorToast.vue";
import Icon from "../components/Icon.vue";
import SessionView from "./SessionView.vue";

// 会话页只放聊天本体：
// 会话列表在侧栏「会话」子菜单里，新建按钮在顶栏标题旁，这里负责承载新建弹窗。

const cards = ref<CardSummary[]>([]);
const personas = ref<Persona[]>([]);
const error = ref("");
const newEl = ref<HTMLDialogElement | null>(null);

const form = reactive({
  character: "",
  persona: "",
  day: 1,
  clock: "",
  place: "",
  premise: "",
});

onMounted(async () => {
  void loadSessions();
  try {
    [cards.value, personas.value] = await Promise.all([api.listCards(), api.listPersonas()]);
    if (!form.character && cards.value.length > 0) {
      form.character = cards.value[0].dir_name;
    }
  } catch {
    /* 卡/人格读取失败不阻塞弹窗 */
  }
});

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
    const meta = await api.newSession({
      character: form.character,
      persona: form.persona || undefined,
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
      <SessionView v-if="selectedSession" :meta="selectedSession" />

      <div
        v-else
        class="card card-dash flex flex-1 flex-col items-center justify-center gap-2 bg-base-100 p-8 text-center"
      >
        <Icon name="sparkle" :size="24" class="text-base-content/25" />
        <p class="m-0 text-sm text-base-content/50">挑一场戏，接着往下演。</p>
        <p class="m-0 text-xs text-base-content/40">在左侧「会话」里选一场，或者新建一场。</p>
        <button class="btn btn-primary btn-sm" @click="openNewSession">
          <Icon name="plus" :size="15" />新建会话
        </button>
      </div>
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
          <div>
            <label class="label" for="new-persona">用户人格</label>
            <select id="new-persona" class="select select-sm w-full" v-model="form.persona">
              <option value="">（不使用）</option>
              <option v-for="p in personas" :key="p.name" :value="p.name">{{ p.name }}</option>
            </select>
          </div>
          <div class="grid grid-cols-2 gap-3">
            <div>
              <label class="label" for="new-day">第几天</label>
              <input id="new-day" class="input input-sm w-full" v-model.number="form.day" type="number" min="1" />
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
