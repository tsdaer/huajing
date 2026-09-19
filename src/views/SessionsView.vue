<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import { api } from "../api";
import type { CardSummary, Persona, SessionMeta } from "../types";
import SessionView from "./SessionView.vue";

// M1.5 将升级为完整新建向导与聊天界面；当前先提供
// 列表 + 最小新建表单 + 会话详情（黑板编辑 / 记忆检查器 / 消息只读）。

const sessions = ref<SessionMeta[]>([]);
const cards = ref<CardSummary[]>([]);
const personas = ref<Persona[]>([]);
const selected = ref<SessionMeta | null>(null);
const creating = ref(false);
const error = ref("");

const form = reactive({
  character: "",
  persona: "",
  day: 1,
  clock: "",
  place: "",
  premise: "",
});

async function refresh() {
  error.value = "";
  try {
    sessions.value = await api.listSessions();
  } catch (e) {
    error.value = String(e);
  }
}

onMounted(async () => {
  await refresh();
  try {
    [cards.value, personas.value] = await Promise.all([api.listCards(), api.listPersonas()]);
    if (!form.character && cards.value.length > 0) {
      form.character = cards.value[0].dir_name;
    }
  } catch {
    /* 卡/人格读取失败不阻塞列表 */
  }
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
    creating.value = false;
    await refresh();
    selected.value = sessions.value.find((s) => s.id === meta.id) ?? meta;
    error.value = "";
  } catch (e) {
    error.value = String(e);
  }
}

async function open(s: SessionMeta) {
  selected.value = s;
}

function fmtDate(iso: string): string {
  return iso.replace("T", " ").replace("Z", "");
}
</script>

<template>
  <main class="page">
    <p v-if="error" class="error">{{ error }}</p>

    <div class="cols">
      <!-- 左：会话列表 + 新建 -->
      <section class="col">
        <header class="col-head">
          <h2>会话</h2>
          <button class="btn accent" @click="creating = !creating">
            {{ creating ? "收起" : "＋ 新建" }}
          </button>
        </header>

        <form v-if="creating" class="newform" @submit.prevent="create">
          <label class="field">
            <span>角色卡 *</span>
            <select v-model="form.character">
              <option v-for="c in cards" :key="c.dir_name" :value="c.dir_name">
                {{ c.name }}{{ c.degraded ? "（降级）" : "" }}
              </option>
            </select>
          </label>
          <label class="field">
            <span>用户人格</span>
            <select v-model="form.persona">
              <option value="">（不使用）</option>
              <option v-for="p in personas" :key="p.name" :value="p.name">{{ p.name }}</option>
            </select>
          </label>
          <div class="row2">
            <label class="field">
              <span>第几天</span>
              <input v-model.number="form.day" type="number" min="1" />
            </label>
            <label class="field">
              <span>时间（HH:MM）</span>
              <input v-model="form.clock" placeholder="21:30" />
            </label>
          </div>
          <label class="field">
            <span>地点</span>
            <input v-model="form.place" placeholder="图书馆自习区" />
          </label>
          <label class="field">
            <span>起因（premise，可空）</span>
            <input v-model="form.premise" placeholder="闭馆前的一小时" />
          </label>
          <div class="form-ops">
            <button class="btn accent" type="submit">创建</button>
          </div>
        </form>

        <ul class="slist">
          <li
            v-for="s in sessions"
            :key="s.id"
            :class="{ active: selected?.id === s.id }"
            @click="open(s)"
          >
            <div class="sname">{{ s.characters.join(" × ") }}</div>
            <div class="smeta">
              {{ fmtDate(s.created_at) }}{{ s.persona ? ` · ${s.persona}` : "" }}
            </div>
          </li>
          <li v-if="sessions.length === 0" class="empty">还没有会话，点「＋ 新建」开一场。</li>
        </ul>
      </section>

      <!-- 右：选中会话详情 -->
      <section class="col wide">
        <SessionView v-if="selected" :meta="selected" />
        <p v-else class="placeholder">选择或创建一个会话，查看黑板与记忆检查器。</p>
      </section>
    </div>
  </main>
</template>

<style scoped>
.page {
  flex: 1;
  width: min(1080px, 100%);
  margin: 0 auto;
  padding: 20px 16px 40px;
}
.error {
  margin: 0 0 12px;
  padding: 10px 14px;
  border-radius: 8px;
  background: rgba(200, 80, 80, 0.15);
  color: #e0a0a0;
  font-size: 13px;
}
.cols {
  display: flex;
  gap: 16px;
  align-items: flex-start;
}
.col {
  background: var(--hj-panel);
  border-radius: 12px;
  padding: 14px 16px;
  flex: 0 0 280px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.col.wide {
  flex: 1;
  min-width: 0;
}
.col-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.col-head h2 {
  margin: 0;
  font-size: 16px;
}
.placeholder {
  color: var(--hj-dim);
  font-size: 13px;
  text-align: center;
  margin: 40px 0;
}

.newform {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 12px;
  border-radius: 8px;
  background: var(--hj-bg);
}
.row2 {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
}
.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--hj-dim);
}
.field input,
.field select {
  background: var(--hj-panel);
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 6px;
  color: var(--hj-fg);
  padding: 6px 10px;
  font-size: 13px;
}
.form-ops {
  display: flex;
  justify-content: flex-end;
}

.slist {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
  max-height: 60vh;
  overflow-y: auto;
}
.slist li {
  padding: 10px 12px;
  border-radius: 8px;
  background: var(--hj-bg);
  cursor: pointer;
}
.slist li.active {
  outline: 1px solid var(--hj-accent);
}
.slist li.empty {
  color: var(--hj-dim);
  font-size: 13px;
  cursor: default;
}
.sname {
  font-size: 14px;
  font-weight: 600;
}
.smeta {
  color: var(--hj-dim);
  font-size: 11px;
  margin-top: 2px;
  font-family: Consolas, monospace;
}

@media (max-width: 760px) {
  .cols {
    flex-direction: column;
  }
  .col {
    flex: none;
    width: 100%;
  }
}
</style>
