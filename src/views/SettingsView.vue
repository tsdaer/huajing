<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { api } from "../api";
import type { DiagRecord, Persona, Provider, ProviderTest, RuntimeInfo, Settings } from "../types";
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
  tools: "off",
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
    proxyDraft.value = settings.value.proxy ?? "";
    runtime.value = await api.runtimeInfo();
    await refreshDiagnostics();
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

// ---------- 运行环境 ----------
const runtime = ref<RuntimeInfo | null>(null);
const diagnostics = ref<DiagRecord[]>([]);

function fmtClock(ts: number): string {
  const d = new Date(ts * 1000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}:${String(d.getSeconds()).padStart(2, "0")}`;
}

async function refreshDiagnostics() {
  try {
    diagnostics.value = await api.recentDiagnostics(60);
  } catch (e) {
    error.value = String(e);
  }
}

const buildStamp = computed(() => {
  const ts = runtime.value?.buildTs;
  if (!ts) return "该构建未提供";
  return new Date(ts * 1000).toLocaleString();
});

// ---------- 出网代理 ----------
const proxyDraft = ref("");
const savingProxy = ref(false);
const proxySaved = ref(false);

/** 运行期捕获分级的中档开关（M3.8 · 设计 §6.8-2） */
const savingMinor = ref(false);
async function setAutoAcceptMinor(on: boolean) {
  if (!settings.value) return;
  savingMinor.value = true;
  try {
    settings.value = await api.saveSettings({ ...settings.value, auto_accept_minor_facts: on });
    error.value = "";
  } catch (e) {
    error.value = String(e);
  } finally {
    savingMinor.value = false;
  }
}

/** 每轮工具调用上限（增强 A5）：空输入 = 回到缺省 6 */
const toolLimitText = ref(String(6));
async function saveToolLimit() {
  if (!settings.value) return;
  const n = parseInt(toolLimitText.value, 10);
  const value = Number.isFinite(n) ? Math.max(1, Math.min(20, n)) : null;
  try {
    settings.value = await api.saveSettings({ ...settings.value, tool_calls_per_turn: value });
    toolLimitText.value = String(value ?? 6);
    error.value = "";
  } catch (e) {
    error.value = String(e);
  }
}

async function saveProxy() {
  if (!settings.value) return;
  savingProxy.value = true;
  try {
    settings.value = await api.saveSettings({
      ...settings.value,
      proxy: proxyDraft.value.trim() || null,
    });
    proxySaved.value = true;
    error.value = "";
  } catch (e) {
    error.value = String(e);
  } finally {
    savingProxy.value = false;
  }
}

// ---------- 连通性自检（一次「为什么发不出去」的诊断）----------
const testing = ref("");
const testResult = ref<{ name: string; result: ProviderTest } | null>(null);

/** 用途档位的中文标签（M3.10 起三档：chat / util / embed） */
function providerRoleLabel(role: string): string {
  if (role === "util") return "工具档";
  if (role === "embed") return "嵌入档";
  return "主对话";
}

/** 用途档位的徽标配色：一眼分清谁干什么 */
function providerBadgeClass(role: string): string {
  if (role === "util") return "badge-info";
  if (role === "embed") return "badge-success";
  return "badge-primary";
}

/** 真的发一条最小请求：能区分 key 无效 / 地址写错 / 出网被拦三类故障 */
async function testProvider(p: Provider) {
  testing.value = p.name;
  testResult.value = null;
  try {
    testResult.value = { name: p.name, result: await api.testProvider({ ...p }) };
  } catch (e) {
    error.value = String(e);
  } finally {
    testing.value = "";
  }
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
      tools: "off",
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
      tools: "on",
    },
  },
  {
    label: "Ollama 嵌入（语义关联）",
    hint: "qwen3-embedding · 需先 ollama pull",
    preset: {
      name: "ollama-embed",
      base_url: "http://localhost:11434/v1",
      api_key: "",
      model: "qwen3-embedding",
      temperature: 0,
      role: "embed",
      tools: "off",
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
            若发消息报「请求失败」，先到下面「出网代理」点保存，再点接入点的「测试」看原因。
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

          <!-- 自检结果：成功给 URL 与耗时，失败给完整原因链与代理线索 -->
          <div
            v-if="testResult"
            class="rounded-box border px-3 py-2.5 text-xs"
            :class="testResult.result.ok ? 'border-success/40 bg-success/5' : 'border-error/40 bg-error/5'"
          >
            <div class="flex items-start justify-between gap-3">
              <p class="m-0 font-medium">
                {{ testResult.name }}：{{ testResult.result.message }}
              </p>
              <button class="btn btn-ghost btn-xs flex-none" @click="testResult = null">关闭</button>
            </div>
            <p class="mt-1 mb-0 font-mono text-[11px] break-all text-base-content/60">
              POST {{ testResult.result.url }} · 模型 {{ testResult.result.model }}
            </p>
            <p
              v-if="testResult.result.detail"
              class="mt-1 mb-0 font-mono text-[11px] break-all text-base-content/50"
            >
              响应：{{ testResult.result.detail }}
            </p>
            <p class="mt-1.5 mb-0 text-[11px] text-base-content/60">
              本次走：{{ testResult.result.proxy_used || "直连（未使用代理）" }}
              <template v-if="testResult.result.proxy.length">
                · 检测到 {{ testResult.result.proxy.join("、") }}
              </template>
            </p>
            <p
              v-if="!testResult.result.ok"
              class="mt-1.5 mb-0 text-[11px] text-base-content/60"
            >
              常见原因：key 无效 · base_url 少写 <code>/v1</code> · 需要代理才能出网 · 服务未启动（本地模型）。
            </p>
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
                    :class="providerBadgeClass(p.role)"
                  >
                    {{ providerRoleLabel(p.role) }}
                  </span>
                  <span class="badge badge-sm badge-ghost font-mono">T={{ p.temperature }}</span>
                </div>
                <p class="mt-0.5 mb-0 truncate font-mono text-[11px] text-base-content/50">
                  {{ p.model }} @ {{ p.base_url }}
                </p>
              </div>
              <div class="flex items-center justify-end gap-1">
                <button
                  class="btn btn-ghost btn-xs"
                  :disabled="testing === p.name"
                  @click="testProvider(p)"
                >
                  <span v-if="testing === p.name" class="loading loading-spinner loading-xs"></span>
                  <Icon v-else name="bolt" :size="13" />测试
                </button>
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
                  <option value="embed">embed · 语义关联（嵌入向量）</option>
                </select>
                <p v-if="draft.role === 'embed'" class="mt-1 mb-0 text-[11px] text-base-content/50">
                  选 Qwen3-Embedding 一类的嵌入模型（如 Ollama 的
                  <code class="font-mono">qwen3-embedding</code>）；不配置 = 语义关联层关闭。
                </p>
              </div>
              <div v-if="draft.role === 'chat'">
                <label class="label" for="p-tools">工具调用</label>
                <select id="p-tools" class="select select-sm w-full" v-model="draft.tools">
                  <option value="off">off · 关闭（纯文本，最稳）</option>
                  <option value="on">on · 开启（模型可自报持久变化）</option>
                </select>
                <p class="mt-1 mb-0 text-[11px] text-base-content/50">
                  开启后请求带工具 schema；接入点不支持时自动降级为纯文本。卡上的
                  <code class="font-mono">tools.deny</code> 可再做减法。
                </p>
              </div>
            </div>
            <div class="flex justify-end gap-2">
              <button class="btn btn-primary btn-sm" type="submit">保存</button>
              <button class="btn btn-ghost btn-sm" type="button" @click="editing = false">取消</button>
            </div>
          </form>
        </div>
      </section>

      <!-- 运行环境：排查「改了没用」的第一眼（进程真正在用的路径与构建时间） -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-3 p-5">
          <div>
            <h2 class="card-title gap-2 text-sm font-medium">
              <Icon name="database" :size="16" class="text-base-content/45" />
              运行环境
            </h2>
            <p class="mt-1 mb-0 text-xs text-base-content/50">
              这是当前进程真正在读的数据目录。若与你编辑的仓库目录不一致，改卡不会生效。
            </p>
          </div>
          <dl class="m-0 flex flex-col gap-1.5 text-xs">
            <div class="flex items-baseline gap-2">
              <dt class="w-20 flex-none text-base-content/45">数据目录</dt>
              <dd class="m-0 min-w-0 flex-1 truncate font-mono text-[11px]" :title="runtime?.dataRoot">
                {{ runtime?.dataRoot ?? "—" }}
              </dd>
            </div>
            <div class="flex items-baseline gap-2">
              <dt class="w-20 flex-none text-base-content/45">构建时间</dt>
              <dd class="m-0 font-mono text-[11px]">{{ buildStamp }}</dd>
            </div>
            <div class="flex items-baseline gap-2">
              <dt class="w-20 flex-none text-base-content/45">已装载</dt>
              <dd class="m-0 text-[11px]">
                {{ runtime?.cardCount ?? 0 }} 张卡 · {{ runtime?.sessionCount ?? 0 }} 场会话
              </dd>
            </div>
          </dl>
          <p v-if="!runtime?.buildTs" class="m-0 text-[11px] text-warning">
            这个构建没有返回构建时间——说明它早于该功能，建议重新构建后再测。
          </p>

          <!-- 诊断：钩子跑没跑、卡从哪来，一眼可见（安装版没有控制台可看） -->
          <div class="flex items-center justify-between gap-2 pt-1">
            <span class="text-xs text-base-content/60">运行时诊断（新的在前）</span>
            <button class="btn btn-ghost btn-xs" @click="refreshDiagnostics">
              <Icon name="refresh" :size="12" />刷新
            </button>
          </div>
          <ul v-if="diagnostics.length" class="m-0 flex max-h-56 list-none flex-col gap-1 overflow-y-auto p-0">
            <li
              v-for="(d, i) in diagnostics"
              :key="`${d.ts}-${i}`"
              class="flex items-start gap-2 rounded-box bg-base-200 px-2.5 py-1.5"
            >
              <span class="badge badge-xs badge-soft flex-none" :class="d.kind === 'error' ? 'badge-error' : 'badge-ghost'">
                {{ d.kind }}
              </span>
              <span class="min-w-0 flex-1 font-mono text-[11px] break-all">{{ d.detail }}</span>
              <span class="flex-none text-[10px] text-base-content/40">{{ fmtClock(d.ts) }}</span>
            </li>
          </ul>
          <p v-else class="m-0 text-[11px] text-base-content/45">
            暂无记录。发一条消息或导入一张卡后再刷新。
          </p>
        </div>
      </section>

      <!-- 出网：代理直连二选一，发不出去时的第一处置点 -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-3 p-5">
          <div>
            <h2 class="card-title gap-2 text-sm font-medium">
              <Icon name="bolt" :size="16" class="text-base-content/45" />
              出网代理
            </h2>
            <p class="mt-1 mb-0 text-xs text-base-content/50">
              留空即自动：环境变量 → Windows 系统代理（需确认在监听）→ 直连。
              直连不上 API（连接超时 / 请求失败）时，在这里填本地代理地址。
            </p>
          </div>
          <div class="join w-full">
            <input
              class="input join-item input-sm flex-1"
              v-model="proxyDraft"
              placeholder="http://127.0.0.1:7890（留空 = 自动）"
              aria-label="出网代理地址"
            />
            <button class="btn join-item btn-sm" :disabled="savingProxy" @click="saveProxy">
              <span v-if="savingProxy" class="loading loading-spinner loading-xs"></span>
              保存
            </button>
          </div>
          <p v-if="proxySaved" class="m-0 text-[11px] text-success">
            已保存。点任意接入点的「测试」确认能否出网。
          </p>
          <p v-if="testResult?.result.proxy.length" class="m-0 font-mono text-[11px] text-base-content/50">
            检测到：{{ testResult.result.proxy.join(" · ") }}
          </p>
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
            <li class="list-row items-center rounded-box bg-base-200 px-4 py-3">
              <div class="flex flex-col gap-0.5">
                <span class="text-sm text-base-content/55">自动接受设定小事实</span>
                <span class="text-xs text-base-content/45">
                  总结管线给既有实体补充的小事实直接写正史（M3.8 · 设计 §6.8-2 分级捕获的中档）；
                  关闭 = 一律进收件箱人工确认。瞬时状态写黑板与全新实体不受此开关影响。
                </span>
              </div>
              <input
                type="checkbox"
                class="toggle toggle-sm"
                :checked="settings.auto_accept_minor_facts ?? false"
                aria-label="自动接受设定小事实"
                :disabled="savingMinor"
                @change="setAutoAcceptMinor(($event.target as HTMLInputElement).checked)"
              />
            </li>
            <li class="list-row items-center rounded-box bg-base-200 px-4 py-3">
              <div class="flex flex-col gap-0.5">
                <span class="text-sm text-base-content/55">每轮工具调用上限</span>
                <span class="text-xs text-base-content/45">
                  接入点开「工具调用」后，主演模型每轮最多提交的次数；超出的全部弃置并记入诊断
                  （增强 A5）。缺省 6。
                </span>
              </div>
              <div class="flex items-center gap-2">
                <input
                  v-model="toolLimitText"
                  class="input input-sm w-16 text-right"
                  type="number"
                  min="1"
                  max="20"
                  aria-label="每轮工具调用上限"
                />
                <button class="btn btn-ghost btn-sm" type="button" @click="saveToolLimit">保存</button>
              </div>
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
