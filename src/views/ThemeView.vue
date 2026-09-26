<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import Icon from "../components/Icon.vue";
import { PRESETS } from "../theme-presets";
import { api } from "../api";
import {
  ACTIVE_STEM,
  BASE_KEY,
  TOKEN_GROUPS,
  applyVars,
  clearCustom,
  clearVars,
  cssColorToHex,
  getActiveCustom,
  persistCustom,
  toPluginCss,
  type ThemePreset,
} from "../theme";

// 主题编辑器：基础主题（daisyUI 默认 light/dark）+ 33 个预设（来自 docs/theme_test/theme.css）
// + 自定义令牌。自定义令牌写在 :root 内联样式上，实时覆盖当前主题；
// 持久化走 DataHub/themes/（M4.3 · 决断 7）——custom 是活动主题，其余名字是主题库。

// ---------- 基础主题 ----------
type BasePref = "system" | "light" | "dark";

const storedBase = localStorage.getItem(BASE_KEY);
const basePref = ref<BasePref>(storedBase === "light" || storedBase === "dark" ? storedBase : "system");

watch(basePref, (t) => {
  if (t === "system") {
    localStorage.removeItem(BASE_KEY);
    document.documentElement.removeAttribute("data-theme");
  } else {
    localStorage.setItem(BASE_KEY, t);
    document.documentElement.dataset.theme = t;
  }
});

// ---------- 自定义令牌 ----------
const saved = getActiveCustom();
const vars = ref<Record<string, string>>(saved ? { ...saved.vars } : {});
const colorScheme = ref<"light" | "dark">(saved?.colorScheme ?? "light");
const themeName = ref(saved?.preset ?? "");
const hasCustom = computed(() => Object.keys(vars.value).length > 0);

/** 每次改动都实时落到页面上并（防抖）写进 DataHub/themes/custom.json */
watch(
  [vars, colorScheme, themeName],
  () => {
    applyVars(vars.value);
    if (Object.keys(vars.value).length > 0) {
      document.documentElement.style.colorScheme = colorScheme.value;
      persistCustom({ preset: themeName.value, colorScheme: colorScheme.value, vars: vars.value });
    } else {
      clearCustom();
    }
  },
  { deep: true },
);

function setToken(key: string, value: string) {
  const next = { ...vars.value };
  const trimmed = value.trim();
  if (trimmed) next[key] = trimmed;
  else delete next[key];
  vars.value = next;
}

function usePreset(p: ThemePreset) {
  themeName.value = p.name;
  colorScheme.value = p.colorScheme;
  vars.value = { ...p.vars };
}

function resetCustom() {
  themeName.value = "";
  vars.value = {};
  clearVars();
  clearCustom();
}

// ---------- 主题库（DataHub/themes/ 下除活动主题外的 *.json） ----------
const library = ref<string[]>([]);
const libraryMsg = ref("");

async function refreshLibrary() {
  try {
    library.value = (await api.themeList()).filter((n) => n !== ACTIVE_STEM);
  } catch {
    library.value = [];
  }
}

onMounted(refreshLibrary);

async function saveToLibrary() {
  const name = themeName.value.trim();
  if (!name || !hasCustom.value) return;
  try {
    await api.themeSave(name, { preset: name, colorScheme: colorScheme.value, vars: vars.value });
    libraryMsg.value = "";
    await refreshLibrary();
  } catch (e) {
    libraryMsg.value = String(e);
  }
}

/** 从库里点选：载入编辑器（watcher 会实时应用并写回活动主题） */
async function applyFromLibrary(name: string) {
  try {
    const t = await api.themeLoad(name);
    if (!t) return;
    themeName.value = t.preset || name;
    colorScheme.value = t.colorScheme === "dark" ? "dark" : "light";
    vars.value = { ...t.vars };
    libraryMsg.value = "";
  } catch (e) {
    libraryMsg.value = String(e);
  }
}

// ---------- 预设筛选 ----------
const query = ref("");
const visiblePresets = computed(() => {
  const q = query.value.trim().toLowerCase();
  return q ? PRESETS.filter((p) => p.name.toLowerCase().includes(q)) : PRESETS;
});

// ---------- 导入：粘贴 CSS 主题块或选 .json 文件（键校验在后端，坏键拒绝并列出） ----------
const importText = ref("");
const importMsg = ref<{ ok: boolean; text: string } | null>(null);
const importing = ref(false);

async function doImport() {
  importing.value = true;
  try {
    const parsed = await api.themeParseImport(importText.value, TOKEN_GROUPS.flatMap((g) => g.tokens.map((t) => t.key)));
    themeName.value = parsed.name;
    colorScheme.value = parsed.colorScheme === "dark" ? "dark" : "light";
    vars.value = { ...parsed.vars };
    importMsg.value = { ok: true, text: `已导入「${parsed.name}」（${Object.keys(parsed.vars).length} 个令牌），正在生效` };
    importText.value = "";
  } catch (e) {
    importMsg.value = { ok: false, text: String(e) };
  } finally {
    importing.value = false;
  }
}

async function onImportFile(ev: Event) {
  const file = (ev.target as HTMLInputElement).files?.[0];
  if (!file) return;
  importText.value = await file.text();
  await doImport();
}

// ---------- 导出：复制到剪贴板 / 保存文件 双出口 ----------
const exportCss = computed(() => toPluginCss(vars.value, themeName.value || "custom", colorScheme.value));
const copied = ref(false);
const savedPath = ref("");

async function copyCss() {
  try {
    await navigator.clipboard.writeText(exportCss.value);
    copied.value = true;
    window.setTimeout(() => (copied.value = false), 1600);
  } catch {
    copied.value = false;
  }
}

async function saveCssFile() {
  try {
    savedPath.value = await api.themeExportFile(themeName.value.trim() || "custom", exportCss.value);
  } catch {
    savedPath.value = "";
  }
}
</script>

<template>
  <div class="h-full overflow-y-auto p-4 lg:p-6">
    <div class="mx-auto flex w-full max-w-[1000px] flex-col gap-4">
      <!-- 基础主题 -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-4 p-5">
          <div class="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h2 class="card-title gap-2 text-sm font-medium">
                <Icon name="sun" :size="16" class="text-base-content/45" />
                基础主题
              </h2>
              <p class="mt-1 mb-0 text-xs text-base-content/50">
                daisyUI 自带主题：light（默认）与 dark（跟随系统）。下面的自定义令牌会覆盖它。
              </p>
            </div>
            <div class="join">
              <button
                class="btn join-item btn-sm"
                :class="{ 'btn-active': basePref === 'system' }"
                @click="basePref = 'system'"
              >
                <Icon name="cpu" :size="15" />跟随系统
              </button>
              <button
                class="btn join-item btn-sm"
                :class="{ 'btn-active': basePref === 'light' }"
                @click="basePref = 'light'"
              >
                <Icon name="sun" :size="15" />浅色
              </button>
              <button
                class="btn join-item btn-sm"
                :class="{ 'btn-active': basePref === 'dark' }"
                @click="basePref = 'dark'"
              >
                <Icon name="moon" :size="15" />深色
              </button>
            </div>
          </div>
        </div>
      </section>

      <!-- 预设 -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-4 p-5">
          <div class="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h2 class="card-title gap-2 text-sm font-medium">
                <Icon name="palette" :size="16" class="text-base-content/45" />
                预设主题
                <span class="badge badge-sm badge-ghost">{{ PRESETS.length }}</span>
              </h2>
              <p class="mt-1 mb-0 text-xs text-base-content/50">
                来自 <code class="font-mono">docs/theme_test/theme.css</code>；点一张即套用，并可继续微调。
              </p>
            </div>
            <label class="input input-sm w-full sm:w-56">
              <Icon name="search" :size="15" class="text-base-content/40" />
              <input v-model="query" type="search" placeholder="筛选预设名" />
            </label>
          </div>

          <div class="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4">
            <button
              v-for="p in visiblePresets"
              :key="p.name"
              class="card card-border cursor-pointer gap-0 bg-base-200 p-3 text-left transition-colors"
              :class="themeName === p.name ? 'border-primary' : 'hover:border-base-content/25'"
              @click="usePreset(p)"
            >
              <span class="flex items-center gap-1.5">
                <span
                  class="size-6 rounded-selector border border-base-content/10"
                  :style="{ background: p.vars['--color-base-100'] }"
                ></span>
                <span class="size-6 rounded-selector" :style="{ background: p.vars['--color-primary'] }"></span>
                <span class="size-6 rounded-selector" :style="{ background: p.vars['--color-secondary'] }"></span>
                <span class="size-6 rounded-selector" :style="{ background: p.vars['--color-accent'] }"></span>
                <Icon v-if="themeName === p.name" name="check" :size="14" class="ml-auto text-primary" />
              </span>
              <span class="mt-2 flex items-center gap-1.5 text-xs font-medium">
                {{ p.name }}
                <span class="badge badge-xs badge-ghost">{{ p.colorScheme === "dark" ? "暗" : "亮" }}</span>
              </span>
            </button>
          </div>
        </div>
      </section>

      <!-- 自定义令牌 -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-4 p-5">
          <div class="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h2 class="card-title gap-2 text-sm font-medium">
                <Icon name="settings" :size="16" class="text-base-content/45" />
                自定义令牌
                <span v-if="hasCustom" class="badge badge-sm badge-soft badge-primary">已启用</span>
              </h2>
              <p class="mt-1 mb-0 text-xs text-base-content/50">
                留空即继承当前主题；支持 <code class="font-mono">oklch(...)</code>、hex 等任意 CSS 颜色写法。
              </p>
            </div>
            <div class="flex flex-wrap items-center gap-2">
              <input v-model="themeName" class="input input-sm w-36 font-mono" placeholder="主题名" />
              <select v-model="colorScheme" class="select select-sm w-24">
                <option value="light">浅色</option>
                <option value="dark">深色</option>
              </select>
              <button
                class="btn btn-sm"
                :disabled="!hasCustom || !themeName.trim()"
                :title="!themeName.trim() ? '先给主题起个名字' : '存一份进主题库（DataHub/themes/）'"
                @click="saveToLibrary"
              >
                <Icon name="copy" :size="15" />存入库
              </button>
              <button class="btn btn-sm" :disabled="!hasCustom" @click="resetCustom">
                <Icon name="trash" :size="15" />清除
              </button>
            </div>
          </div>
          <p v-if="libraryMsg" role="alert" class="m-0 text-xs text-error">{{ libraryMsg }}</p>

          <div v-for="group in TOKEN_GROUPS" :key="group.title" class="flex flex-col gap-2">
            <div class="flex flex-wrap items-baseline gap-2">
              <h3 class="m-0 text-xs font-medium">{{ group.title }}</h3>
              <span class="text-[11px] text-base-content/45">{{ group.hint }}</span>
            </div>
            <div class="flex flex-col gap-2">
              <div
                v-for="t in group.tokens"
                :key="t.key"
                class="flex items-center gap-2 rounded-box bg-base-200 px-3 py-2"
              >
                <span
                  class="size-6 shrink-0 rounded-selector border border-base-content/10"
                  :style="{ background: `var(${t.key})` }"
                ></span>
                <span class="w-36 shrink-0 truncate font-mono text-[11px] text-base-content/60" :title="t.key">
                  {{ t.label }}
                </span>
                <input
                  class="input input-xs flex-1 font-mono"
                  :value="vars[t.key] ?? ''"
                  :placeholder="t.hint ?? '继承当前主题'"
                  @input="setToken(t.key, ($event.target as HTMLInputElement).value)"
                />
                <input
                  v-if="t.kind === 'color'"
                  type="color"
                  class="h-6 w-9 shrink-0 cursor-pointer rounded-selector border border-base-300 bg-transparent"
                  :value="cssColorToHex(vars[t.key])"
                  @input="setToken(t.key, ($event.target as HTMLInputElement).value)"
                />
              </div>
            </div>
          </div>
        </div>
      </section>

      <!-- 主题库：DataHub/themes/ 下保存的主题（拷走 DataHub 即跟走） -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-4 p-5">
          <div>
            <h2 class="card-title gap-2 text-sm font-medium">
              <Icon name="layers" :size="16" class="text-base-content/45" />
              主题库
              <span class="badge badge-sm badge-ghost">{{ library.length }}</span>
            </h2>
            <p class="mt-1 mb-0 text-xs text-base-content/50">
              存进 <code class="font-mono">DataHub/themes/</code> 的主题——拷走 DataHub
              即跟走，换设备放回同目录即可点选应用。
            </p>
          </div>
          <p v-if="library.length === 0" class="m-0 text-xs text-base-content/45">
            库里还没有主题：给自定义令牌起个名，点「存入库」。
          </p>
          <div v-else class="flex flex-wrap gap-2">
            <div
              v-for="name in library"
              :key="name"
              class="flex items-center gap-2 rounded-box bg-base-200 py-1.5 pl-3 pr-1.5 text-xs"
            >
              <span class="font-mono">{{ name }}</span>
              <button class="btn btn-ghost btn-xs" @click="applyFromLibrary(name)">应用</button>
            </div>
          </div>
        </div>
      </section>

      <!-- 预览 -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-4 p-5">
          <h2 class="card-title gap-2 text-sm font-medium">
            <Icon name="layers" :size="16" class="text-base-content/45" />
            预览
          </h2>

          <div class="flex flex-wrap items-center gap-2">
            <button class="btn btn-primary btn-sm">主要动作</button>
            <button class="btn btn-sm">默认</button>
            <button class="btn btn-outline btn-sm">描边</button>
            <button class="btn btn-ghost btn-sm">幽灵</button>
            <span class="badge badge-primary">徽标</span>
            <span class="badge badge-soft badge-success">成功</span>
            <span class="badge badge-soft badge-warning">警告</span>
            <span class="badge badge-soft badge-error">错误</span>
          </div>

          <div class="flex flex-wrap items-center gap-3">
            <input class="input input-sm w-44" placeholder="输入框" />
            <select class="select select-sm w-28">
              <option>下拉</option>
            </select>
            <input type="checkbox" class="checkbox checkbox-sm checkbox-primary" checked />
            <input type="range" class="range range-xs w-28" />
            <span class="loading loading-spinner loading-sm text-primary"></span>
            <span class="status status-success"></span>
          </div>

          <div class="flex flex-col gap-1">
            <div class="chat chat-start">
              <div class="chat-bubble text-sm">对面说的话</div>
            </div>
            <div class="chat chat-end">
              <div class="chat-bubble chat-bubble-primary text-sm">自己说的话</div>
            </div>
          </div>

          <div role="alert" class="alert alert-soft text-sm">提示条：alert-soft</div>

          <div class="flex flex-wrap gap-2">
            <div class="rounded-box bg-base-100 px-3 py-2 text-xs">base-100</div>
            <div class="rounded-box bg-base-200 px-3 py-2 text-xs">base-200</div>
            <div class="rounded-box bg-base-300 px-3 py-2 text-xs">base-300</div>
            <div class="rounded-box bg-primary px-3 py-2 text-xs text-primary-content">primary</div>
            <div class="rounded-box bg-secondary px-3 py-2 text-xs text-secondary-content">secondary</div>
            <div class="rounded-box bg-accent px-3 py-2 text-xs text-accent-content">accent</div>
            <div class="rounded-box bg-neutral px-3 py-2 text-xs text-neutral-content">neutral</div>
          </div>
        </div>
      </section>

      <!-- 导入：粘贴 CSS 主题块或选 .json 文件；键校验在后端，坏键拒绝并列出 -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-4 p-5">
          <div class="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h2 class="card-title gap-2 text-sm font-medium">
                <Icon name="download" :size="16" class="text-base-content/45" />
                导入
              </h2>
              <p class="mt-1 mb-0 text-xs text-base-content/50">
                粘贴本页导出的 CSS 主题块，或选一个主题 JSON。只认编辑器认识的令牌键，
                键名不对会整包拒绝并列出非法键。
              </p>
            </div>
            <label class="btn btn-sm">
              <Icon name="download" :size="15" />选择文件…
              <input type="file" accept=".json,.css,.txt" class="hidden" @change="onImportFile" />
            </label>
          </div>
          <textarea
            v-model="importText"
            class="textarea h-32 w-full font-mono text-xs leading-relaxed"
            placeholder='@plugin "daisyui/theme" { name: "…"; color-scheme: dark; --color-primary: …; }'
          ></textarea>
          <div class="flex items-center justify-end gap-2">
            <p v-if="importMsg" class="m-0 flex-1 text-xs" :class="importMsg.ok ? 'text-success' : 'text-error'">
              {{ importMsg.text }}
            </p>
            <button class="btn btn-primary btn-sm" :disabled="!importText.trim() || importing" @click="doImport">
              <span v-if="importing" class="loading loading-spinner loading-xs"></span>
              <Icon v-else name="check" :size="15" />
              导入并应用
            </button>
          </div>
        </div>
      </section>

      <!-- 导出 -->
      <section class="card card-border bg-base-100">
        <div class="card-body gap-4 p-5">
          <div class="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h2 class="card-title gap-2 text-sm font-medium">
                <Icon name="copy" :size="16" class="text-base-content/45" />
                导出
              </h2>
              <p class="mt-1 mb-0 text-xs text-base-content/50">
                粘进 <code class="font-mono">src/style.css</code> 即可成为编译期主题（可去掉
                <code class="font-mono">default/prefersdark</code> 两行）；「保存文件」落到
                <code class="font-mono">DataHub/exports/</code>。
              </p>
            </div>
            <div class="flex items-center gap-2">
              <p v-if="savedPath" class="m-0 max-w-52 truncate text-[11px] text-base-content/55" :title="savedPath">
                已存 {{ savedPath }}
              </p>
              <button class="btn btn-sm" @click="saveCssFile">
                <Icon name="download" :size="15" />保存文件
              </button>
              <button class="btn btn-sm" @click="copyCss">
                <Icon :name="copied ? 'check' : 'copy'" :size="15" />{{ copied ? "已复制" : "复制 CSS" }}
              </button>
            </div>
          </div>
          <textarea class="textarea h-64 w-full font-mono text-xs leading-relaxed" readonly :value="exportCss"></textarea>
        </div>
      </section>
    </div>
  </div>
</template>
