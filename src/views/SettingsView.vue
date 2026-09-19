<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { api } from "../api";
import type { Persona, Provider, Settings } from "../types";

// ---------- 接入点（providers.json · 设计 §11）----------

const providers = ref<Provider[]>([]);
const personas = ref<Persona[]>([]);
const settings = ref<Settings | null>(null);
const error = ref("");
const editing = ref(false);

const blank = (): Provider => ({
  name: "",
  base_url: "https://api.deepseek.com/v1",
  api_key: "",
  model: "",
  temperature: 0.8,
  role: "chat",
});

const draft = reactive<Provider>(blank());

const roleBadge = computed(
  () => (p: Provider) => (p.role === "util" ? "工具档" : "主对话"),
);

async function refresh() {
  error.value = "";
  try {
    [providers.value, personas.value, settings.value] = await Promise.all([
      api.listProviders(),
      api.listPersonas(),
      api.getSettings(),
    ]);
  } catch (e) {
    error.value = String(e);
  }
}

onMounted(refresh);

function startCreate() {
  Object.assign(draft, blank());
  editing.value = true;
}

function startEdit(p: Provider) {
  Object.assign(draft, JSON.parse(JSON.stringify(p)) as Provider);
  editing.value = true;
}

async function save() {
  if (!draft.name.trim() || !draft.base_url.trim() || !draft.model.trim()) {
    error.value = "名称、Base URL、模型为必填项。";
    return;
  }
  try {
    providers.value = await api.saveProvider({ ...draft });
    editing.value = false;
    error.value = "";
  } catch (e) {
    error.value = String(e);
  }
}

async function remove(p: Provider) {
  if (!window.confirm(`删除接入点「${p.name}」？`)) return;
  try {
    providers.value = await api.deleteProvider(p.name);
    if (editing.value && draft.name === p.name) editing.value = false;
    error.value = "";
  } catch (e) {
    error.value = String(e);
  }
}
</script>

<template>
  <main class="page">
    <p v-if="error" class="error">{{ error }}</p>

    <!-- 接入点 -->
    <section class="block">
      <header class="block-head">
        <h2>接入点</h2>
        <button class="btn accent" @click="startCreate" v-if="!editing">＋ 新增</button>
      </header>

      <p v-if="providers.length === 0 && !editing" class="empty">
        还没有接入点。新增一个 OpenAI 兼容接入点（DeepSeek / GLM / Ollama 均可），
        或参考 <code>DataHub/providers.example.toml</code>。
      </p>

      <ul class="plist">
        <li v-for="p in providers" :key="p.name" class="prow">
          <div class="pmain">
            <span class="pname">{{ p.name }}</span>
            <span class="badge" :class="{ util: p.role === 'util' }">{{ roleBadge(p) }}</span>
            <span class="pmodel">{{ p.model }} @ {{ p.base_url }}</span>
          </div>
          <div class="pops">
            <span class="temp">T={{ p.temperature }}</span>
            <button class="btn" @click="startEdit(p)">编辑</button>
            <button class="btn danger" @click="remove(p)">删除</button>
          </div>
        </li>
      </ul>

      <!-- 新增 / 编辑表单（本地明文存储，API key 直接可见编辑） -->
      <form v-if="editing" class="pform" @submit.prevent="save">
        <h3>{{ providers.some((p) => p.name === draft.name) ? "编辑接入点" : "新增接入点" }}</h3>
        <label class="field">
          <span>名称 *</span>
          <input v-model="draft.name" placeholder="如 deepseek" :disabled="providers.some((p) => p.name === draft.name)" />
        </label>
        <label class="field">
          <span>Base URL *</span>
          <input v-model="draft.base_url" placeholder="https://api.deepseek.com/v1" />
        </label>
        <label class="field">
          <span>API Key</span>
          <input v-model="draft.api_key" placeholder="sk-…（本地明文保存，请自行保管）" />
        </label>
        <label class="field">
          <span>模型 *</span>
          <input v-model="draft.model" placeholder="deepseek-chat" />
        </label>
        <label class="field half">
          <span>温度</span>
          <input v-model.number="draft.temperature" type="number" min="0" max="2" step="0.1" />
        </label>
        <label class="field half">
          <span>用途</span>
          <select v-model="draft.role">
            <option value="chat">chat · 主对话</option>
            <option value="util">util · 总结/捕获（便宜档）</option>
          </select>
        </label>
        <div class="form-ops">
          <button class="btn accent" type="submit">保存</button>
          <button class="btn" type="button" @click="editing = false">取消</button>
        </div>
      </form>
    </section>

    <!-- 用户人格（只读；编辑入口后续里程碑补） -->
    <section class="block">
      <header class="block-head">
        <h2>用户人格</h2>
      </header>
      <ul class="plist">
        <li v-for="p in personas" :key="p.name" class="prow plain">
          <div class="pmain">
            <span class="pname">{{ p.name }}</span>
            <span class="pmodel">{{ p.description }}</span>
          </div>
        </li>
      </ul>
    </section>

    <!-- 全局设置（只读展示；可编辑项随界面完善逐步开放） -->
    <section class="block" v-if="settings">
      <header class="block-head">
        <h2>全局设置</h2>
      </header>
      <div class="kv">
        <span class="k">语言</span><span>{{ settings.locale }}</span>
        <span class="k">主题</span><span>{{ settings.theme }}</span>
        <span class="k">叙事模式</span><span>{{ settings.narrative_mode }}</span>
      </div>
    </section>
  </main>
</template>

<style scoped>
.page {
  flex: 1;
  width: min(720px, 100%);
  margin: 0 auto;
  padding: 20px 16px 40px;
  display: flex;
  flex-direction: column;
  gap: 20px;
}
.error {
  margin: 0;
  padding: 10px 14px;
  border-radius: 8px;
  background: rgba(200, 80, 80, 0.15);
  color: #e0a0a0;
  font-size: 13px;
}
.block {
  background: var(--hj-panel);
  border-radius: 12px;
  padding: 16px 20px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.block-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.block-head h2 {
  margin: 0;
  font-size: 16px;
  font-weight: 600;
}
.empty {
  color: var(--hj-dim);
  font-size: 13px;
  margin: 0;
}
code {
  color: var(--hj-accent);
  font-family: Consolas, monospace;
}

.plist {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.prow {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 12px;
  border-radius: 8px;
  background: var(--hj-bg);
}
.pmain {
  display: flex;
  align-items: baseline;
  gap: 10px;
  min-width: 0;
}
.pname {
  font-weight: 600;
}
.badge {
  flex: none;
  font-size: 11px;
  padding: 1px 8px;
  border-radius: 999px;
  background: rgba(212, 161, 94, 0.18);
  color: var(--hj-accent);
}
.badge.util {
  background: rgba(120, 160, 200, 0.18);
  color: #9ab8d8;
}
.pmodel {
  color: var(--hj-dim);
  font-size: 12px;
  font-family: Consolas, monospace;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pops {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: none;
}
.temp {
  color: var(--hj-dim);
  font-size: 12px;
  font-family: Consolas, monospace;
}

.btn {
  border: 1px solid rgba(255, 255, 255, 0.12);
  background: transparent;
  color: var(--hj-fg);
  border-radius: 8px;
  padding: 5px 14px;
  font-size: 13px;
  cursor: pointer;
}
.btn:hover {
  border-color: var(--hj-accent);
}
.btn.accent {
  background: var(--hj-accent);
  border-color: var(--hj-accent);
  color: #1d2026;
}
.btn.accent:hover {
  filter: brightness(1.08);
}
.btn.danger:hover {
  border-color: #c06060;
  color: #d89090;
}

.pform {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
  padding: 14px;
  border-radius: 8px;
  background: var(--hj-bg);
}
.pform h3 {
  grid-column: 1 / -1;
  margin: 0;
  font-size: 14px;
}
.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--hj-dim);
}
.field.half {
  grid-column: span 1;
}
.field input,
.field select {
  background: var(--hj-panel);
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 6px;
  color: var(--hj-fg);
  padding: 7px 10px;
  font-size: 13px;
}
.field input:disabled {
  opacity: 0.55;
}
.form-ops {
  grid-column: 1 / -1;
  display: flex;
  gap: 8px;
  justify-content: flex-end;
}

.kv {
  display: grid;
  grid-template-columns: auto 1fr;
  gap: 8px 16px;
  font-size: 13px;
}
.kv .k {
  color: var(--hj-dim);
}

@media (max-width: 560px) {
  .pform {
    grid-template-columns: 1fr;
  }
  .prow {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
