<script setup lang="ts">
import { computed, defineAsyncComponent, onMounted, onUnmounted, ref, watch } from "vue";
import { api } from "./api";
import { assetTab, type AssetTab } from "./assets";
import { CARD_FILE_RE, cardGeneration, importNotice, importOpen, requestImport, startCardWatch, stopCardWatch } from "./cards";
import Icon from "./components/Icon.vue";
import TitleBar from "./components/TitleBar.vue";
import { loadSessions, openNewSession, selectedId, selectSession, sessions } from "./sessions";
import type { SessionMeta } from "./types";
import ImportCardDialog from "./components/ImportCardDialog.vue";
import AssetsView from "./views/AssetsView.vue";
import HomeView from "./views/HomeView.vue";
import IngestView from "./views/IngestView.vue";
import SessionsView from "./views/SessionsView.vue";
import SettingsView from "./views/SettingsView.vue";

// 主题编辑器按需加载：预设主题字符串约 45KB，只在进入该页时拉取
const ThemeView = defineAsyncComponent(() => import("./views/ThemeView.vue"));

// 应用外壳：自定义标题栏（无边框窗口）+ drawer 侧栏 + navbar 顶栏 + 视图区。
// 路由细化（单会话地址等）随 M1.5 评估，此处先分页切换；地址片段可直达。
type ViewId = "home" | "sessions" | "ingest" | "assets" | "theme" | "settings";

const HASH_VIEWS: ViewId[] = ["home", "sessions", "ingest", "assets", "theme", "settings"];
const view = ref<ViewId>(HASH_VIEWS.find((v) => location.hash === `#${v}`) ?? "home");
watch(view, (v) => {
  location.hash = v;
});

// 主导航只放高频入口；设置是低频项，单独落在侧栏底部（本地模式卡之上）
const NAV: ViewId[] = ["home", "sessions", "ingest", "assets", "theme"];
const PAGES: Record<ViewId, { label: string; icon: string; title: string; desc: string }> = {
  home: { label: "概览", icon: "home", title: "概览", desc: "运行时状态与快速入口" },
  sessions: { label: "会话", icon: "chat", title: "会话", desc: "挑一场戏，接着往下演" },
  ingest: { label: "素材导入", icon: "database", title: "素材导入", desc: "wiki 角色页十分钟成卡（素材规格化）" },
  assets: { label: "资产", icon: "book", title: "资产", desc: "角色卡与世界设定集" },
  theme: { label: "主题", icon: "palette", title: "主题", desc: "预设、配色与形状令牌" },
  settings: { label: "设置", icon: "settings", title: "设置", desc: "接入点、人格与全局项" },
};

/** 资产子条目 → 跳资产页并落到对应页签 */
function goAssets(tab: AssetTab) {
  assetTab.value = tab;
  go("assets");
}

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

/** 导入完成：让会话页重扫卡片清单（新建会话的下拉要跟上） */
function onImported() {
  cardGeneration.value += 1;
}

const info = ref<Awaited<ReturnType<typeof api.appInfo>> | null>(null);

/** 构建时间（本地时区，精确到分钟）：真机排查时先看它——旧构建一眼可见 */
const buildLabel = computed(() => {
  const ts = info.value?.buildTs;
  if (!ts) return ""; // 旧构建没有这个字段
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
});
onMounted(async () => {
  try {
    info.value = await api.appInfo();
  } catch {
    info.value = null;
  }
  void loadSessions();
  // M1.7 热加载：改了 DataHub 下的卡片/人格即推事件，视图据此刷新
  void startCardWatch();
  void startDropWatch();
});

// ---------- 拖放导入（M1.8）----------
// WebView 的 File API 拿不到本地路径，拖放的路径只能从 Tauri 的窗口事件取。
const dragging = ref(false);

// E4：拖放监听句柄保存下来，卸载时解除（原来直接丢弃）
let unlistenDrag: (() => void) | null = null;

async function startDropWatch() {
  try {
    const { getCurrentWebview } = await import("@tauri-apps/api/webview");
    unlistenDrag = await getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "enter" || event.payload.type === "over") {
        dragging.value = true;
        return;
      }
      dragging.value = false;
      if (event.payload.type !== "drop") return;
      const paths = event.payload.paths;
      const hit = paths.find((p) => CARD_FILE_RE.test(p));
      if (hit) {
        importNotice.value = "";
        requestImport(hit);
      } else if (paths.length > 0) {
        // 拖了别的文件：说清楚为什么不理会（并留痕，便于事后排查）
        importNotice.value = `只支持 SillyTavern 的 PNG / JSON 角色卡，已忽略：${paths[0]}`;
        void api.recordDiagnostic("import", `拖放被忽略（非 PNG/JSON）：${paths[0]}`);
      }
    });
  } catch {
    /* 浏览器 mock 下没有 Tauri 窗口事件 */
  }
}

// E4：卸载时解除拖放与卡片变更监听（句柄不再丢弃）
onUnmounted(() => {
  unlistenDrag?.();
  stopCardWatch();
});
</script>

<template>
  <div class="flex h-full flex-col">
    <TitleBar />

    <!-- 导入向导常驻应用级：拖放/顶栏/会话页三处入口共用，且在任何页面都能被拖放唤起 -->
    <ImportCardDialog v-model="importOpen" @imported="onImported" />

    <!-- 拖入被忽略时的说明 -->
    <div
      v-if="importNotice"
      class="fixed inset-x-0 bottom-4 z-50 mx-auto w-fit max-w-[92vw]"
      role="status"
    >
      <div class="alert alert-warning shadow-lg">
        <span class="text-xs">{{ importNotice }}</span>
        <button class="btn btn-ghost btn-xs" @click="importNotice = ''">知道了</button>
      </div>
    </div>

    <!-- 拖入卡文件的提示（M1.8）：松手即打开导入向导 -->
    <Transition name="fade">
      <div
        v-if="dragging"
        class="pointer-events-none fixed inset-0 z-50 flex items-center justify-center bg-base-300/70 backdrop-blur-xs"
      >
        <div class="pop-in rounded-box border-2 border-dashed border-primary bg-base-100 px-8 py-6 text-center">
          <Icon name="sparkle" :size="24" class="mx-auto text-primary" />
          <p class="mt-2 mb-0 text-sm font-medium">松手导入角色卡</p>
          <p class="mb-0 text-xs text-base-content/50">SillyTavern 的 PNG 或 JSON</p>
        </div>
      </div>
    </Transition>

    <div class="drawer min-h-0 flex-1 lg:drawer-open">
      <input id="shell-drawer" v-model="drawerOpen" type="checkbox" class="drawer-toggle" />

      <!-- overflow-x-clip：右缘按钮的 tooltip 伪元素会伸出视口，外壳层禁止横向滚动（tooltip 越界部分裁掉） -->
      <div class="drawer-content flex h-full min-h-0 flex-col overflow-x-clip bg-base-200">
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
          <!-- 页面切换淡入淡出（out-in：旧页先走，不叠影） -->
          <Transition name="fade" mode="out-in">
            <HomeView v-if="view === 'home'" @go="view = $event" />
            <SessionsView v-else-if="view === 'sessions'" />
            <IngestView v-else-if="view === 'ingest'" />
            <AssetsView v-else-if="view === 'assets'" @go="view = $event" />
            <ThemeView v-else-if="view === 'theme'" />
            <SettingsView v-else />
          </Transition>
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

            <!-- 会话与资产是「可折叠子菜单」，其余是普通入口 -->
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

              <li v-else-if="id === 'assets' && drawerOpen">
                <details :open="view === 'assets'">
                  <summary :class="{ 'menu-active': view === 'assets' }" @click="go('assets')">
                    <Icon name="book" :size="17" />
                    <span class="flex-1">{{ PAGES.assets.label }}</span>
                  </summary>
                  <ul>
                    <li>
                      <button
                        :class="{ 'menu-active': view === 'assets' && assetTab === 'characters' }"
                        @click="goAssets('characters')"
                      >
                        <Icon name="users" :size="15" />
                        <span class="truncate">角色</span>
                      </button>
                    </li>
                    <li>
                      <button
                        :class="{ 'menu-active': view === 'assets' && assetTab === 'codex' }"
                        @click="goAssets('codex')"
                      >
                        <Icon name="globe" :size="15" />
                        <span class="truncate">设定集</span>
                      </button>
                    </li>
                  </ul>
                </details>
              </li>

              <li v-else>
                <button
                  class="is-drawer-close:justify-center is-drawer-close:tooltip is-drawer-close:tooltip-right transition-colors duration-150"
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

          <!-- 底部区：低频入口与身份信息（设置 + 本地模式卡） -->
          <div class="flex flex-none flex-col gap-1 border-t border-base-300 p-2">
            <ul class="menu w-full gap-0.5 px-0">
              <li>
                <button
                  class="is-drawer-close:justify-center is-drawer-close:tooltip is-drawer-close:tooltip-right transition-colors duration-150"
                  :class="{ 'menu-active': view === 'settings' }"
                  data-tip="设置"
                  @click="go('settings')"
                >
                  <Icon name="settings" :size="17" />
                  <span class="is-drawer-close:hidden">设置</span>
                </button>
              </li>
            </ul>

            <div
              class="flex items-center gap-3 rounded-box bg-base-200 p-2 is-drawer-close:tooltip is-drawer-close:tooltip-right"
              :data-tip="`v${info?.version ?? '0.1.0'}${buildLabel ? ' · 构建 ' + buildLabel : ''}`"
            >
              <span
                class="flex size-9 shrink-0 items-center justify-center rounded-full bg-neutral text-xs text-neutral-content"
              >
                境
              </span>
              <div class="min-w-0 flex-1 is-drawer-close:hidden">
                <p class="truncate text-sm font-medium">本地模式</p>
                <p class="truncate text-[11px] text-base-content/45">
                  v{{ info?.version ?? "0.1.0" }}
                  <template v-if="buildLabel"> · 构建 {{ buildLabel }}</template>
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
