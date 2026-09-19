<script setup lang="ts">
import { computed, defineAsyncComponent, onMounted, ref, watch } from "vue";
import { api } from "./api";
import Icon from "./components/Icon.vue";
import TitleBar from "./components/TitleBar.vue";
import { loadSessions, openNewSession, selectedId, selectSession, sessions } from "./sessions";
import type { SessionMeta } from "./types";
import HomeView from "./views/HomeView.vue";
import SessionsView from "./views/SessionsView.vue";
import SettingsView from "./views/SettingsView.vue";

// 主题编辑器按需加载：预设主题字符串约 45KB，只在进入该页时拉取
const ThemeView = defineAsyncComponent(() => import("./views/ThemeView.vue"));

// 应用外壳：自定义标题栏（无边框窗口）+ drawer 侧栏 + navbar 顶栏 + 视图区。
// 路由细化（单会话地址等）随 M1.5 评估，此处先四页切换；地址片段可直达。
type ViewId = "home" | "sessions" | "theme" | "settings";

const HASH_VIEWS: ViewId[] = ["home", "sessions", "theme", "settings"];
const view = ref<ViewId>(HASH_VIEWS.find((v) => location.hash === `#${v}`) ?? "home");
watch(view, (v) => {
  location.hash = v;
});

const NAV: ViewId[] = ["home", "sessions", "theme", "settings"];
const PAGES: Record<ViewId, { label: string; icon: string; title: string; desc: string }> = {
  home: { label: "概览", icon: "home", title: "概览", desc: "运行时状态与本机数据一览" },
  sessions: { label: "会话", icon: "chat", title: "会话", desc: "挑一场戏，接着往下演" },
  theme: { label: "主题", icon: "palette", title: "主题", desc: "预设、配色与形状令牌" },
  settings: { label: "设置", icon: "settings", title: "设置", desc: "接入点、人格与全局项" },
};

// 侧栏：大屏默认展开，窄屏默认收起（收起时只留图标栏）
const drawerOpen = ref(window.matchMedia("(min-width: 1024px)").matches);

/** 侧栏子菜单里最多列几场会话，其余走「查看全部」 */
const SIDEBAR_LIMIT = 8;
const sidebarSessions = computed(() => sessions.value.slice(0, SIDEBAR_LIMIT));

/** 切页：窄屏下顺手收起侧栏，避免遮住内容 */
function go(id: ViewId) {
  view.value = id;
  if (!window.matchMedia("(min-width: 1024px)").matches) drawerOpen.value = false;
}

/** 侧栏点选一场会话：选中 + 跳到会话页 */
function openSession(s: SessionMeta) {
  selectSession(s.id);
  go("sessions");
}

const info = ref<Awaited<ReturnType<typeof api.appInfo>> | null>(null);
onMounted(async () => {
  try {
    info.value = await api.appInfo();
  } catch {
    info.value = null;
  }
  void loadSessions();
});
</script>

<template>
  <div class="flex h-full flex-col">
    <TitleBar />

    <div class="drawer min-h-0 flex-1 lg:drawer-open">
      <input id="shell-drawer" v-model="drawerOpen" type="checkbox" class="drawer-toggle" />

      <div class="drawer-content flex h-full min-h-0 flex-col bg-base-200">
        <!-- 顶栏：navbar（左：折叠按钮 + 页面标题；右：面包屑 + 状态） -->
        <header class="navbar flex-none gap-2 border-b border-base-300 bg-base-100 px-3 lg:px-6">
          <div class="navbar-start gap-2">
            <label for="shell-drawer" class="btn btn-square btn-ghost btn-sm" aria-label="收起或展开侧栏">
              <Icon name="menu" :size="18" />
            </label>
            <div class="min-w-0">
              <h1 class="truncate text-base font-semibold">{{ PAGES[view].title }}</h1>
              <p class="truncate text-xs text-base-content/50">{{ PAGES[view].desc }}</p>
            </div>
            <!-- 新建会话：随「会话」标题一起出现在顶栏 -->
            <button
              v-if="view === 'sessions'"
              class="btn btn-primary btn-sm ml-1 flex-none"
              @click="openNewSession"
            >
              <Icon name="plus" :size="15" />新建会话
            </button>
          </div>
          <div class="navbar-end gap-3">
            <div class="breadcrumbs hidden text-xs text-base-content/45 sm:block">
              <ul>
                <li>化境</li>
                <li class="text-base-content/70">{{ PAGES[view].label }}</li>
              </ul>
            </div>
            <div class="indicator">
              <span class="indicator-item badge badge-xs badge-soft badge-success"></span>
              <div class="avatar avatar-placeholder">
                <div class="w-8 rounded-full bg-primary text-primary-content">
                  <span class="text-xs">境</span>
                </div>
              </div>
            </div>
          </div>
        </header>

        <main class="min-h-0 flex-1">
          <HomeView v-if="view === 'home'" @go="view = $event" />
          <SessionsView v-else-if="view === 'sessions'" />
          <ThemeView v-else-if="view === 'theme'" />
          <SettingsView v-else />
        </main>
      </div>

      <!-- 侧栏：可收成图标栏（daisyUI is-drawer-close / is-drawer-open 变体） -->
      <!-- daisyUI 的 .drawer-side 固定为 100dvh，而自定义标题栏已占掉一条高度，
           这里用 ! 覆盖成父容器高度，避免页面被撑出 36px 的滚动条 -->
      <div class="drawer-side z-30 h-full! is-drawer-close:overflow-visible">
        <label for="shell-drawer" aria-label="关闭侧栏" class="drawer-overlay"></label>
        <aside
          class="flex h-full flex-col border-r border-base-300 bg-base-100 is-drawer-close:w-16 is-drawer-open:w-64"
        >
          <!-- 收起时不做滚动容器：否则菜单项的 tooltip 伪元素会被算作横向溢出并撑出滚动条 -->
          <ul
            class="menu w-full flex-1 flex-nowrap gap-0.5 px-2 pt-3"
            :class="drawerOpen ? 'overflow-y-auto' : 'overflow-visible'"
          >
            <li class="menu-title text-[11px] tracking-widest text-base-content/40 is-drawer-close:hidden">
              导航
            </li>

            <!-- 会话项是「可折叠子菜单」，其余是普通入口 -->
            <template v-for="id in NAV" :key="id">
              <li v-if="id === 'sessions' && drawerOpen">
                <details :open="view === 'sessions'">
                  <summary :class="{ 'menu-active': view === 'sessions' }" @click="go('sessions')">
                    <Icon name="chat" :size="17" />
                    <span class="flex-1">{{ PAGES.sessions.label }}</span>
                    <span class="badge badge-xs badge-ghost">{{ sessions.length }}</span>
                  </summary>
                  <ul>
                    <li v-for="s in sidebarSessions" :key="s.id">
                      <button
                        :class="{ 'menu-active': selectedId === s.id }"
                        :title="s.characters.join(' × ')"
                        @click="openSession(s)"
                      >
                        <span class="truncate">{{ s.characters.join(" × ") }}</span>
                      </button>
                    </li>
                    <li v-if="sessions.length === 0" class="menu-disabled">
                      <span>暂无会话</span>
                    </li>
                    <li v-else-if="sessions.length > SIDEBAR_LIMIT">
                      <button @click="go('sessions')">查看全部 {{ sessions.length }} 场</button>
                    </li>
                  </ul>
                </details>
              </li>

              <li v-else>
                <button
                  class="is-drawer-close:justify-center is-drawer-close:tooltip is-drawer-close:tooltip-right"
                  :class="{ 'menu-active': view === id }"
                  :data-tip="PAGES[id].label"
                  @click="go(id)"
                >
                  <Icon :name="PAGES[id].icon" :size="17" />
                  <span class="is-drawer-close:hidden">{{ PAGES[id].label }}</span>
                </button>
              </li>
            </template>
          </ul>

          <div class="border-t border-base-300 p-2">
            <div
              class="flex items-center gap-3 rounded-box bg-base-200 p-2 is-drawer-close:tooltip is-drawer-close:tooltip-right"
              :data-tip="`v${info?.version ?? '0.1.0'}`"
            >
              <span
                class="flex size-9 shrink-0 items-center justify-center rounded-full bg-neutral text-xs text-neutral-content"
              >
                境
              </span>
              <div class="min-w-0 flex-1 is-drawer-close:hidden">
                <p class="truncate text-sm font-medium">本地模式</p>
                <p class="truncate text-[11px] text-base-content/45">
                  v{{ info?.version ?? "0.1.0" }} · 明文数据
                </p>
              </div>
              <span class="status status-sm status-success is-drawer-close:hidden"></span>
            </div>
          </div>
        </aside>
      </div>
    </div>
  </div>
</template>
