<script setup lang="ts">
import { computed, ref, watch } from "vue";
import Icon from "../components/Icon.vue";
import { PRESETS } from "../theme-presets";
import {
  BASE_KEY,
  TOKEN_GROUPS,
  applyVars,
  clearVars,
  cssColorToHex,
  loadCustom,
  removeCustom,
  saveCustom,
  toPluginCss,
  type ThemePreset,
} from "../theme";

// 主题编辑器：基础主题（daisyUI 默认 light/dark）+ 33 个预设（来自 docs/theme_test/theme.css）
// + 自定义令牌。自定义令牌写在 :root 内联样式上，实时覆盖当前主题。

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
const saved = loadCustom();
const vars = ref<Record<string, string>>(saved ? { ...saved.vars } : {});
const colorScheme = ref<"light" | "dark">(saved?.colorScheme ?? "light");
const themeName = ref(saved?.preset ?? "");
const hasCustom = computed(() => Object.keys(vars.value).length > 0);

/** 每次改动都实时落到页面上并记住 */
watch(
  [vars, colorScheme],
  () => {
    applyVars(vars.value);
    if (Object.keys(vars.value).length > 0) {
      document.documentElement.style.colorScheme = colorScheme.value;
      saveCustom({ preset: themeName.value, colorScheme: colorScheme.value, vars: vars.value });
    } else {
      removeCustom();
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
  removeCustom();
}

// ---------- 预设筛选 ----------
const query = ref("");
const visiblePresets = computed(() => {
  const q = query.value.trim().toLowerCase();
  return q ? PRESETS.filter((p) => p.name.toLowerCase().includes(q)) : PRESETS;
});

// ---------- 导出 ----------
const exportCss = computed(() => toPluginCss(vars.value, themeName.value || "custom", colorScheme.value));
const copied = ref(false);

async function copyCss() {
  try {
    await navigator.clipboard.writeText(exportCss.value);
    copied.value = true;
    window.setTimeout(() => (copied.value = false), 1600);
  } catch {
    copied.value = false;
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
              <button class="btn btn-sm" :disabled="!hasCustom" @click="resetCustom">
                <Icon name="trash" :size="15" />清除
              </button>
            </div>
          </div>

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
                <code class="font-mono">default/prefersdark</code> 两行）。
              </p>
            </div>
            <button class="btn btn-sm" @click="copyCss">
              <Icon :name="copied ? 'check' : 'copy'" :size="15" />{{ copied ? "已复制" : "复制 CSS" }}
            </button>
          </div>
          <textarea class="textarea h-64 w-full font-mono text-xs leading-relaxed" readonly :value="exportCss"></textarea>
        </div>
      </section>
    </div>
  </div>
</template>
