<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { api } from "../api";
import { cardGeneration } from "../cards";
import { loadSessions, openNewSession, selectSession, sessions } from "../sessions";
import type { CardSummary, Settings } from "../types";
import Icon from "../components/Icon.vue";

// 概览（精简版）：继续开演 + 紧凑统计 + 运行时与下一步。
// 角色卡墙与设定集浏览已迁往「资产」页，这里只留快速入口。

const emit = defineEmits<{ go: [view: "sessions" | "assets" | "settings"] }>();

const info = ref<Awaited<ReturnType<typeof api.appInfo>> | null>(null);
const cards = ref<CardSummary[]>([]);
const settings = ref<Settings | null>(null);

const recent = computed(() => sessions.value[0] ?? null);

function continueRecent() {
  if (!recent.value) return;
  selectSession(recent.value.id);
  emit("go", "sessions");
}

/** 新建会话：先立标记再跳页——弹窗由会话页承载，落地即开 */
function newSession() {
  openNewSession();
  emit("go", "sessions");
}

onMounted(async () => {
  try {
    info.value = await api.appInfo();
  } catch {
    info.value = null;
  }
  void loadSessions();
  try {
    cards.value = await api.listCards();
  } catch {
    cards.value = [];
  }
  try {
    settings.value = await api.getSettings();
  } catch {
    settings.value = null;
  }
});

// 卡片热加载（M1.7）：只影响统计砖的计数
watch(cardGeneration, async () => {
  try {
    cards.value = await api.listCards();
  } catch {
    cards.value = [];
  }
});
</script>

<template>
  <div class="h-full overflow-y-auto p-4 lg:p-6">
    <div class="mx-auto flex w-full max-w-[900px] flex-col gap-4">
      <!-- 继续开演：最近一场 + 新建 -->
      <section class="card card-border bg-base-100">
        <div class="card-body flex-row items-center gap-4 p-5">
          <span class="flex size-12 shrink-0 items-center justify-center rounded-box bg-primary/15 text-primary">
            <Icon name="play" :size="20" />
          </span>
          <div class="min-w-0 flex-1">
            <p class="m-0 text-[11px] tracking-widest text-base-content/45">继续开演</p>
            <h2 class="m-0 truncate text-base font-semibold">
              {{ recent ? recent.characters.join(" × ") : "还没有会话" }}
            </h2>
            <p class="m-0 truncate text-xs text-base-content/50">
              {{ recent ? recent.premise || "接着上一场，把故事往下推。" : "选一张角色卡，把这场戏开起来。" }}
            </p>
          </div>
          <div class="flex flex-none items-center gap-2">
            <button v-if="recent" class="btn btn-primary btn-sm" @click="continueRecent">
              <Icon name="play" :size="15" />继续
            </button>
            <button class="btn btn-sm" :class="recent ? '' : 'btn-primary'" @click="newSession">
              <Icon name="plus" :size="15" />新建会话
            </button>
          </div>
        </div>
      </section>

      <!-- 紧凑统计：点击直达对应页 -->
      <div class="grid grid-cols-2 gap-3">
        <button
          class="card card-border bg-base-100 px-4 py-3 text-left transition-colors hover:border-primary/40"
          @click="emit('go', 'assets')"
        >
          <span class="flex items-center gap-2 text-xs text-base-content/50">
            <Icon name="users" :size="14" />角色卡
          </span>
          <p class="m-0 mt-1.5 text-2xl leading-none font-semibold tabular-nums">{{ cards.length }}</p>
        </button>
        <button
          class="card card-border bg-base-100 px-4 py-3 text-left transition-colors hover:border-primary/40"
          @click="emit('go', 'sessions')"
        >
          <span class="flex items-center gap-2 text-xs text-base-content/50">
            <Icon name="chat" :size="14" />会话
          </span>
          <p class="m-0 mt-1.5 text-2xl leading-none font-semibold tabular-nums">{{ sessions.length }}</p>
        </button>
      </div>

      <div class="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <!-- 运行时 -->
        <section class="card card-border bg-base-100">
          <div class="card-body gap-3 p-5">
            <h2 class="card-title gap-2 text-sm font-medium">
              <Icon name="cpu" :size="16" class="text-base-content/45" />
              运行时
            </h2>
            <div class="flex items-center justify-between gap-3 rounded-box bg-base-200 px-4 py-2.5">
              <span class="flex items-center gap-2 text-sm text-base-content/55">
                <Icon name="bolt" :size="15" />通道
              </span>
              <span class="badge badge-sm badge-soft badge-success">已打通</span>
            </div>
            <div class="flex items-center justify-between gap-3 rounded-box bg-base-200 px-4 py-2.5">
              <span class="flex items-center gap-2 text-sm text-base-content/55">
                <Icon name="chat" :size="15" />叙事模式
              </span>
              <span class="text-sm font-medium">{{ settings?.narrative_mode ?? "—" }}</span>
            </div>
            <div class="flex items-center justify-between gap-3 rounded-box bg-base-200 px-4 py-2.5">
              <span class="flex items-center gap-2 text-sm text-base-content/55">
                <Icon name="clock" :size="15" />语言
              </span>
              <span class="text-sm font-medium">{{ settings?.locale ?? "—" }}</span>
            </div>
            <div>
              <p class="mb-1.5 flex items-center gap-2 text-xs text-base-content/45">
                <Icon name="database" :size="14" />数据根目录
              </p>
              <p class="m-0 rounded-box bg-base-200 px-3 py-2 font-mono text-[11px] break-all text-base-content/70">
                {{ info?.dataRoot ?? "—" }}
              </p>
            </div>
          </div>
        </section>

        <!-- 下一步 -->
        <section class="card card-border bg-base-100">
          <div class="card-body gap-3 p-5">
            <h2 class="card-title gap-2 text-sm font-medium">
              <Icon name="layers" :size="16" class="text-base-content/45" />
              下一步
            </h2>
            <button
              class="group flex items-center gap-3 rounded-box bg-base-200 px-4 py-3 text-left transition-colors hover:bg-base-300"
              @click="emit('go', 'settings')"
            >
              <span class="flex size-9 shrink-0 items-center justify-center rounded-box bg-primary/15 text-primary">
                <Icon name="settings" :size="16" />
              </span>
              <span class="min-w-0 flex-1">
                <span class="block text-sm font-medium">配好接入点</span>
                <span class="block text-xs text-base-content/50">OpenAI 兼容接入点（DeepSeek / GLM / Ollama 均可）</span>
              </span>
              <Icon name="arrow" :size="15" class="flex-none text-base-content/35 transition-transform group-hover:translate-x-0.5" />
            </button>
            <button
              class="group flex items-center gap-3 rounded-box bg-base-200 px-4 py-3 text-left transition-colors hover:bg-base-300"
              @click="emit('go', 'assets')"
            >
              <span class="flex size-9 shrink-0 items-center justify-center rounded-box bg-primary/15 text-primary">
                <Icon name="users" :size="16" />
              </span>
              <span class="min-w-0 flex-1">
                <span class="block text-sm font-medium">备好角色</span>
                <span class="block text-xs text-base-content/50">导入 SillyTavern 角色卡，或浏览世界设定集</span>
              </span>
              <Icon name="arrow" :size="15" class="flex-none text-base-content/35 transition-transform group-hover:translate-x-0.5" />
            </button>
          </div>
        </section>
      </div>
    </div>
  </div>
</template>
