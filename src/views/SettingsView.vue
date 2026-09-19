<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { api } from "../api";
import type { Persona, Provider, Settings } from "../types";
import ErrorToast from "../components/ErrorToast.vue";
import Icon from "../components/Icon.vue";

// ---------- 接入点（providers.toml · 设计 §11）----------

const providers = ref<Provider[]>([]);
const personas = ref<Persona[]>([]);
const settings = ref<Settings | null>(null);
const error = ref("");
const editing = ref(false);

const confirmEl = ref<HTMLDialogElement | null>(null);
const pendingDelete = ref<Provider | null>(null);

const blank = (): Provider => ({
  name: "",
  base_url: "https://api.deepseek.com/v1",
  api_key: "",
  model: "",
  temperature: 0.8,
  role: "chat",
});

const draft = reactive<Provider>(blank());

async function refresh() {
  error.value = "";
  try {
    [providers.value, personas.value, settings.value] = await Promise.all([
      api.listProviders(),
      api.listPersonas(),
      api.getSettings(),
    ]);
    wizardDone.value = settings.value.wizard_done;
    // 全新用户：直接落在「填 key」这一步，省掉找入口的时间
    if (needsSetup.value) startCreate();
  } catch (e) {
    error.value = String(e);
  }
}

onMounted(refresh);

function startCreate() {
  Object.assign(draft, blank());
  editing.value = true;
}

// ---------- 首启向导（M1.9：装好 → 填 key → 开聊 ≤ 3 分钟）----------
const wizardDone = ref(false);

/** 还缺一个能用的接入点：没有任何接入点，或现有接入点都没有 key（Ollama 除外） */
const needsSetup = computed(() => {
  if (wizardDone.value) return false;
  if (providers.value.length === 0) return true;
  return providers.value.every((p) => !usable(p));
});

/** 本地服务（Ollama 等）不需要 key */
function usable(p: Provider): boolean {
  const local = /localhost|127\.0\.0\.1|0\.0\.0\.0/.test(p.base_url);
  return local || p.api_key.trim().length > 0;
}

/** 常见接入点预设：一键填好 Base URL 与模型名，用户只补 key */
const PRESETS: Array<{ label: string; hint: string; preset: Provider }> = [
  {
    label: "DeepSeek",
    hint: "api.deepseek.com · deepseek-chat",
    preset: {
      name: "deepseek",
      base_url: "https://api.deepseek.com/v1",
      api_key: "",
      model: "deepseek-chat",
      temperature: 0.8,
      role: "chat",
    },
  },
  {
    label: "Ollama（本地）",
    hint: "localhost:11434 · 无需 key",
    preset: {
      name: "ollama",
      base_url: "http://localhost:11434/v1",
      api_key: "",
      model: "qwen2.5:7b",
      temperature: 0.8,
      role: "chat",
    },
  },
];

function usePreset(preset: Provider) {
  Object.assign(draft, JSON.parse(JSON.stringify(preset)) as Provider);
  editing.value = true;
}

/** 走完向导：记住「别再提示」（设置随 settings.toml 持久化） */
async function finishWizard() {
  wizardDone.value = true;
  if (!settings.value) return;
  try {
    settings.value = await api.saveSettings({ ...settings.value, wizard_done: true });
  } catch (e) {
    error.value = String(e);
  }
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
    if (draft.api_key.trim() || usable(draft)) void finishWizard();
  } catch (e) {
    error.value = String(e);
  }
}

function remove(p: Provider) {
  pendingDelete.value = p;
  confirmEl.value?.showModal();
}

async function confirmRemove() {
  const p = pendingDelete.value;
  confirmEl.value?.close();
  pendingDelete.value = null;
  if (!p) return;
  try {
    providers.value = await api.deleteProvider(p.name);
    if (editing.value && draft.name === p.name) editing.value = false;
    error.value = "";
  } catch (e) {
    error.value = String(e);
  }
}

// 界面主题（基础主题 / 预设 / 自定义令牌）见「主题」页。
</script>

<template>
  <div class="h-full overflow-y-auto p-4 lg:p-6">
    <ErrorToast :message="error" @dismiss="error = ''" />

    <div class="mx-auto flex w-full max-w-[900px] flex-col gap-4">
      <!-- 首启向导：三步把第一次对话跑起来（M1.9 ≤3 分钟目标） -->
      <section v-if="needsSetup" class="card card-border border-primary/40 bg-primary/5">
        <div class="card-body gap-3 p-5">
          <h2 class="card-title gap-2 text-sm font-medium">
            <Icon name="sparkle" :size="16" class="text-primary" />
            三步开始：填一个接入点就能开聊
          </h2>
          <ol class="m-0 flex list-none flex-col gap-1.5 p-0 text-xs text-base-content/70">
            <li>① 选一家服务（或直接用本地 Ollama）——下面是常用预设</li>
            <li>② 填入 API key（本地服务不用填），保存</li>
            <li>③ 回到「会话」新建一场，就能聊了</li>
          </ol>
          <div class="flex flex-wrap items-center gap-2">
            <button
              v-for="p in PRESETS"
              :key="p.label"
              class="btn btn-sm"
              :class="editing && draft.base_url === p.preset.base_url ? 'btn-primary' : ''"
              @click="usePreset(p.preset)"
            >
              {{ p.label }}
              <span class="text-[11px] font-normal opacity-60">{{ p.hint }}</span>
            </button>
            <button class="btn btn-ghost btn-sm" @click="finishWizard">我知道了，别再提示</button>
          </div>
          <p class="m-0 text-[11px] text-base-content/45">
            配置明文存放在 <code class="font-mono">DataHub/providers.toml</code>，也可以直接编辑该文件。
          </p>
        </div>
      </section>

      <!-- 接入点 -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-4 p-5">
          <div class="flex items-start justify-between gap-3">
            <div>
              <h2 class="card-title gap-2 text-sm font-medium">
                <Icon name="bolt" :size="16" class="text-base-content/45" />
                接入点
              </h2>
              <p class="mt-1 mb-0 text-xs text-base-content/50">
                OpenAI 兼容协议（DeepSeek / GLM / Ollama 均可），明文存在
                <code class="font-mono">DataHub/providers.toml</code>。
              </p>
            </div>
            <button v-if="!editing" class="btn btn-primary btn-sm flex-none" @click="startCreate">
              <Icon name="plus" :size="15" />新增
            </button>
          </div>

          <ul v-if="providers.length > 0" class="list p-0">
            <li
              v-for="p in providers"
              :key="p.name"
              class="list-row items-center rounded-box bg-base-200 px-4 py-3"
            >
              <span class="flex size-9 items-center justify-center rounded-box bg-primary/15 text-primary">
                <Icon name="bolt" :size="16" />
              </span>
              <div class="min-w-0">
                <div class="flex flex-wrap items-center gap-2">
                  <span class="text-sm font-medium">{{ p.name }}</span>
                  <span
                    class="badge badge-sm badge-soft"
                    :class="p.role === 'util' ? 'badge-info' : 'badge-primary'"
                  >
                    {{ p.role === "util" ? "工具档" : "主对话" }}
                  </span>
                  <span class="badge badge-sm badge-ghost font-mono">T={{ p.temperature }}</span>
                </div>
                <p class="mt-0.5 mb-0 truncate font-mono text-[11px] text-base-content/50">
                  {{ p.model }} @ {{ p.base_url }}
                </p>
              </div>
              <div class="flex items-center justify-end gap-1">
                <button class="btn btn-ghost btn-xs" @click="startEdit(p)">
                  <Icon name="edit" :size="13" />编辑
                </button>
                <button class="btn btn-ghost btn-xs text-error" @click="remove(p)">
                  <Icon name="trash" :size="13" />删除
                </button>
              </div>
            </li>
          </ul>

          <p v-else-if="!editing" class="m-0 text-sm text-base-content/50">
            还没有接入点。新增一个，或用
            <code class="font-mono text-primary">DataHub/providers.example.toml</code> 作模板。
          </p>

          <!-- 新增 / 编辑表单（API key 本地明文可见编辑） -->
          <form v-if="editing" class="flex flex-col gap-3 rounded-box bg-base-200 p-4" @submit.prevent="save">
            <h3 class="m-0 text-sm font-medium">
              {{ providers.some((p) => p.name === draft.name) ? "编辑接入点" : "新增接入点" }}
            </h3>
            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <div>
                <label class="label" for="p-name">名称 *</label>
                <input
                  id="p-name"
                  class="input input-sm w-full"
                  v-model="draft.name"
                  placeholder="如 deepseek"
                  :disabled="providers.some((p) => p.name === draft.name)"
                />
              </div>
              <div>
                <label class="label" for="p-model">模型 *</label>
                <input id="p-model" class="input input-sm w-full" v-model="draft.model" placeholder="deepseek-chat" />
              </div>
            </div>
            <div>
              <label class="label" for="p-base">Base URL *</label>
              <input
                id="p-base"
                class="input input-sm w-full"
                v-model="draft.base_url"
                placeholder="https://api.deepseek.com/v1"
              />
            </div>
            <div>
              <label class="label" for="p-key">API Key</label>
              <input
                id="p-key"
                class="input input-sm w-full"
                v-model="draft.api_key"
                placeholder="sk-…（本地明文保存，请自行保管）"
              />
            </div>
            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <div>
                <label class="label" for="p-temp">温度</label>
                <input
                  id="p-temp"
                  class="input input-sm w-full"
                  v-model.number="draft.temperature"
                  type="number"
                  min="0"
                  max="2"
                  step="0.1"
                />
              </div>
              <div>
                <label class="label" for="p-role">用途</label>
                <select id="p-role" class="select select-sm w-full" v-model="draft.role">
                  <option value="chat">chat · 主对话</option>
                  <option value="util">util · 总结/捕获（便宜档）</option>
                </select>
              </div>
            </div>
            <div class="flex justify-end gap-2">
              <button class="btn btn-primary btn-sm" type="submit">保存</button>
              <button class="btn btn-ghost btn-sm" type="button" @click="editing = false">取消</button>
            </div>
          </form>
        </div>
      </section>

      <!-- 用户人格（只读；编辑入口后续里程碑补） -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-4 p-5">
          <div>
            <h2 class="card-title gap-2 text-sm font-medium">
              <Icon name="users" :size="16" class="text-base-content/45" />
              用户人格
            </h2>
            <p class="mt-1 mb-0 text-xs text-base-content/50">
              位于 <code class="font-mono">DataHub/personas/</code>，开场后由角色卡按需读取。
            </p>
          </div>
          <ul v-if="personas.length > 0" class="list p-0">
            <li
              v-for="p in personas"
              :key="p.name"
              class="list-row items-center rounded-box bg-base-200 px-4 py-3"
            >
              <span class="flex size-9 items-center justify-center rounded-box bg-base-content/10 text-base-content/70">
                <Icon name="users" :size="16" />
              </span>
              <div class="min-w-0">
                <p class="m-0 text-sm font-medium">{{ p.name }}</p>
                <p class="mt-0.5 mb-0 text-xs text-base-content/50">{{ p.description }}</p>
              </div>
            </li>
          </ul>
          <p v-else class="m-0 text-sm text-base-content/50">还没有人格文件。</p>
        </div>
      </section>

      <!-- 全局设置（只读展示；可编辑项随界面完善逐步开放） -->
      <section v-if="settings" class="card card-border bg-base-100">
        <div class="card-body gap-4 p-5">
          <h2 class="card-title gap-2 text-sm font-medium">
            <Icon name="database" :size="16" class="text-base-content/45" />
            全局设置
          </h2>
          <ul class="list p-0">
            <li class="list-row items-center rounded-box bg-base-200 px-4 py-3">
              <span class="text-sm text-base-content/55">语言</span>
              <span class="text-right text-sm font-medium">{{ settings.locale }}</span>
            </li>
            <li class="list-row items-center rounded-box bg-base-200 px-4 py-3">
              <span class="text-sm text-base-content/55">叙事模式</span>
              <span class="text-right text-sm font-medium">{{ settings.narrative_mode }}</span>
            </li>
            <li class="list-row items-center rounded-box bg-base-200 px-4 py-3">
              <span class="text-sm text-base-content/55">会话风格</span>
              <span class="text-right text-sm font-medium">{{ settings.theme }}</span>
            </li>
          </ul>
        </div>
      </section>
    </div>

    <!-- 删除确认 -->
    <dialog ref="confirmEl" class="modal">
      <div class="modal-box">
        <h3 class="text-base font-semibold">删除接入点</h3>
        <p class="mt-2 text-sm text-base-content/70">
          确认删除「{{ pendingDelete?.name ?? "" }}」？这会直接改写
          <code class="font-mono">DataHub/providers.toml</code>，无法撤销。
        </p>
        <div class="modal-action">
          <button class="btn btn-sm" @click="confirmEl?.close()">取消</button>
          <button class="btn btn-error btn-sm" @click="confirmRemove">删除</button>
        </div>
      </div>
      <form method="dialog" class="modal-backdrop">
        <button>关闭</button>
      </form>
    </dialog>
  </div>
</template>
