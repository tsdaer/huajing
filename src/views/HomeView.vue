<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api } from "../api";
import type { CardSummary, SessionMeta, Settings } from "../types";
import Icon from "../components/Icon.vue";

// 概览：运行时状态、角色卡卡片墙、下一步入口。数据全部来自本机 DataHub。
const emit = defineEmits<{ go: [view: "sessions" | "settings"] }>();

const info = ref<Awaited<ReturnType<typeof api.appInfo>> | null>(null);
const cards = ref<CardSummary[]>([]);
const sessions = ref<SessionMeta[]>([]);
const settings = ref<Settings | null>(null);
const loading = ref(false);

onMounted(async () => {
  try {
    info.value = await api.appInfo();
  } catch {
    info.value = null;
  }
  await loadCards();
  try {
    sessions.value = await api.listSessions();
  } catch {
    sessions.value = [];
  }
  try {
    settings.value = await api.getSettings();
  } catch {
    settings.value = null;
  }
});

/** 卡目录为空或读取失败时静默 */
async function loadCards() {
  loading.value = true;
  try {
    cards.value = await api.listCards();
  } catch {
    cards.value = [];
  } finally {
    loading.value = false;
  }
}

const stats = computed(() => [
  {
    icon: "users",
    title: "角色卡",
    value: String(cards.value.length),
    badge: cards.value.length > 0 ? "已装载" : "空",
    tone: cards.value.length > 0 ? "badge-primary" : "badge-ghost",
  },
  {
    icon: "chat",
    title: "会话",
    value: String(sessions.value.length),
    badge: sessions.value.length > 0 ? "可继续" : "未开始",
    tone: sessions.value.length > 0 ? "badge-success" : "badge-ghost",
  },
  { icon: "cpu", title: "核心", value: "Tauri 2", badge: "已打通", tone: "badge-info" },
  {
    icon: "sparkle",
    title: "版本",
    value: `v${info.value?.version ?? "0.1.0"}`,
    badge: "M1 施工中",
    tone: "badge-ghost",
  },
]);

const hooks = computed(() => cards.value.filter((c) => c.has_hooks).length);
const degraded = computed(() => cards.value.filter((c) => c.degraded).length);
</script>

<template>
  <div class="h-full overflow-y-auto p-4 lg:p-6">
    <div class="mx-auto flex w-full max-w-[1200px] flex-col gap-6">
      <!-- 统计卡 -->
      <div class="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <div v-for="s in stats" :key="s.title" class="card card-border bg-base-100">
          <div class="card-body gap-3 p-5">
            <div class="flex items-center gap-3">
              <span class="flex size-10 items-center justify-center rounded-box bg-primary/15 text-primary">
                <Icon :name="s.icon" :size="18" />
              </span>
              <span class="text-xs tracking-widest text-base-content/45">{{ s.title }}</span>
            </div>
            <div class="flex items-end justify-between gap-2">
              <span class="text-2xl leading-none font-semibold">{{ s.value }}</span>
              <span class="badge badge-sm badge-soft" :class="s.tone">{{ s.badge }}</span>
            </div>
          </div>
        </div>
      </div>

      <!-- 角色卡卡片墙 -->
      <section class="flex flex-col gap-4">
        <div class="flex items-center justify-between gap-3">
          <h2 class="m-0 flex items-center gap-2 text-sm font-medium">
            <Icon name="users" :size="16" class="text-base-content/45" />
            角色卡
            <span class="badge badge-sm badge-ghost">{{ cards.length }}</span>
            <span v-if="hooks > 0" class="badge badge-sm badge-soft badge-primary">{{ hooks }} 张带 hooks</span>
            <span v-if="degraded > 0" class="badge badge-sm badge-soft badge-error">{{ degraded }} 张降级</span>
          </h2>
          <button class="btn btn-ghost btn-sm" :disabled="loading" @click="loadCards">
            <span v-if="loading" class="loading loading-spinner loading-xs"></span>
            <Icon v-else name="refresh" :size="15" />刷新
          </button>
        </div>

        <div v-if="cards.length > 0" class="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4">
          <article v-for="c in cards" :key="c.dir_name" class="card card-border bg-base-100">
            <div class="card-body gap-3 p-5">
              <div class="flex items-start gap-3">
                <span
                  class="flex size-10 shrink-0 items-center justify-center rounded-box bg-primary/15 text-base font-medium text-primary"
                >
                  {{ c.name.slice(0, 1) }}
                </span>
                <div class="min-w-0 flex-1">
                  <h3 class="truncate text-sm font-semibold">{{ c.name }}</h3>
                  <p class="truncate font-mono text-[11px] text-base-content/45">{{ c.dir_name }}</p>
                </div>
                <details class="dropdown dropdown-end">
                  <summary class="btn btn-square btn-ghost btn-xs" aria-label="卡片操作">
                    <Icon name="dots" :size="15" />
                  </summary>
                  <ul class="dropdown-content menu z-10 w-40 rounded-box bg-base-200 p-2 shadow-sm">
                    <li>
                      <button @click="emit('go', 'sessions')">
                        <Icon name="chat" :size="14" />用它开一场
                      </button>
                    </li>
                    <li>
                      <button @click="loadCards">
                        <Icon name="refresh" :size="14" />刷新列表
                      </button>
                    </li>
                  </ul>
                </details>
              </div>

              <p class="m-0 text-xs text-base-content/50">
                {{ c.creator ? `作者 ${c.creator}` : "未署名卡片" }}
              </p>

              <div class="flex flex-wrap gap-1">
                <span v-if="c.degraded" class="badge badge-sm badge-soft badge-error">降级</span>
                <span v-if="c.has_hooks" class="badge badge-sm badge-soft badge-primary">hooks</span>
                <span v-for="t in c.tags" :key="t" class="badge badge-sm badge-ghost">{{ t }}</span>
                <span
                  v-if="!c.degraded && !c.has_hooks && c.tags.length === 0"
                  class="text-[11px] text-base-content/40"
                >
                  静态卡
                </span>
              </div>

              <div class="mt-auto flex items-center justify-between gap-2 pt-1">
                <span class="text-[11px] text-base-content/40">DataHub/characters</span>
                <button class="btn btn-sm" @click="emit('go', 'sessions')">
                  <Icon name="chat" :size="15" />开一场
                </button>
              </div>
            </div>
          </article>
        </div>

        <div
          v-else
          class="card card-dash flex flex-col items-center justify-center gap-2 bg-base-100/50 p-10 text-center"
        >
          <Icon name="users" :size="24" class="text-base-content/25" />
          <p class="m-0 text-sm text-base-content/50">还没有角色卡。</p>
          <p class="m-0 text-xs text-base-content/40">
            把卡片目录放进 <code class="font-mono">DataHub/characters/</code> 后点刷新。
          </p>
        </div>
      </section>

      <div class="grid grid-cols-1 gap-4 xl:grid-cols-3">
        <!-- 运行时 -->
        <section class="card card-border bg-base-100 xl:col-span-2">
          <div class="card-body gap-3 p-5">
            <h2 class="card-title gap-2 text-sm font-medium">
              <Icon name="cpu" :size="16" class="text-base-content/45" />
              运行时
            </h2>
            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <div class="flex items-center justify-between gap-3 rounded-box bg-base-200 px-4 py-3">
                <span class="flex items-center gap-2 text-sm text-base-content/55">
                  <Icon name="bolt" :size="15" />通道
                </span>
                <span class="badge badge-sm badge-soft badge-success">已打通</span>
              </div>
              <div class="flex items-center justify-between gap-3 rounded-box bg-base-200 px-4 py-3">
                <span class="flex items-center gap-2 text-sm text-base-content/55">
                  <Icon name="chat" :size="15" />叙事模式
                </span>
                <span class="text-sm font-medium">{{ settings?.narrative_mode ?? "—" }}</span>
              </div>
              <div class="flex items-center justify-between gap-3 rounded-box bg-base-200 px-4 py-3">
                <span class="flex items-center gap-2 text-sm text-base-content/55">
                  <Icon name="sparkle" :size="15" />界面主题
                </span>
                <span class="text-sm font-medium">{{ settings?.theme ?? "—" }}</span>
              </div>
              <div class="flex items-center justify-between gap-3 rounded-box bg-base-200 px-4 py-3">
                <span class="flex items-center gap-2 text-sm text-base-content/55">
                  <Icon name="clock" :size="15" />语言
                </span>
                <span class="text-sm font-medium">{{ settings?.locale ?? "—" }}</span>
              </div>
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
            <div class="flex flex-col gap-3">
              <div class="flex flex-col gap-2 rounded-box bg-base-200 p-4">
                <p class="m-0 text-sm font-medium">配好接入点</p>
                <p class="m-0 text-xs text-base-content/50">
                  填一个 OpenAI 兼容接入点（DeepSeek / GLM / Ollama 均可）。
                </p>
                <button class="btn btn-sm self-start" @click="emit('go', 'settings')">
                  <Icon name="settings" :size="15" />去设置
                </button>
              </div>
              <div class="flex flex-col gap-2 rounded-box bg-base-200 p-4">
                <p class="m-0 text-sm font-medium">开一场会话</p>
                <p class="m-0 text-xs text-base-content/50">
                  选角色卡与用户人格，设定时间地点，把这场戏开起来。
                </p>
                <button class="btn btn-sm self-start" @click="emit('go', 'sessions')">
                  <Icon name="chat" :size="15" />去会话
                </button>
              </div>
            </div>
          </div>
        </section>
      </div>
    </div>
  </div>
</template>
