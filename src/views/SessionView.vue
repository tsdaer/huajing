<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { api } from "../api";
import { cardGeneration } from "../cards";
import type {
  Blackboard,
  Message,
  PromptAssembly,
  Scene,
  SceneView,
  SessionMeta,
} from "../types";
import ErrorToast from "../components/ErrorToast.vue";
import Icon from "../components/Icon.vue";
import SceneDialog from "../components/SceneDialog.vue";
import SceneBar from "../components/SceneBar.vue";
import InspectorDrawer from "../components/InspectorDrawer.vue";
import { kindLabel } from "../components/inspector/kinds";
import type { InspTab } from "../components/inspector/tabs";
import type { SceneSubmit } from "../types";
import { useChatStream } from "../composables/useChatStream";
import { useTheater } from "../composables/useTheater";

// M1.5 聊天界面：气泡流 + 流式打字机 + 停止 + 消息编辑/重roll/删除。
// 黑板与记忆检查器收进右侧抽屉，聊天流为主。界面全部由 daisyUI 组件构成。
// E1 拆分：生成/流式收尾在 useChatStream，剧场自动轮次在 useTheater，
// 场景条是 SceneBar，检查器抽屉是 InspectorDrawer（卡内三页签在其内的 CardStatePanel）。

const props = defineProps<{ meta: SessionMeta }>();

// B6：组件卸载即断链——剧场自动轮次的 promise 链不再驱动下一轮，并中断可能在途的
// 生成（旧实例的流回调由 B5 的世代守卫丢弃）。开着剧场切走页面不能再烧预算。
let disposed = false;
onUnmounted(() => {
  disposed = true;
  void api.stopGeneration(props.meta.id).catch(() => {});
});

const error = ref("");
const messages = ref<Message[]>([]);
const blackboard = ref<Blackboard | null>(null);
const assembly = ref<PromptAssembly | null>(null);

const assemblySource = ref<"last" | "preview">("preview");
const cardName = ref(props.meta.characters[0] ?? "角色");

const panel = ref<"" | "board" | "inspector">("");
const bbForm = reactive({ day: 1, clock: "", place: "", actors: "" });
const savingBb = ref(false);
/** 检查器视角（M3.11）：以谁的视角看（角色目录名；"" = 主角色）。抽屉与卡内状态共用 */
const inspView = ref("");
/** 检查器页签（抽屉关闭再开不丢） */
const inspTab = ref<InspTab>("layers");
/** 检查器抽屉实例：打开时拉数据、换会话作废缓存、轮末重拉 */
const inspectorDrawer = ref<InstanceType<typeof InspectorDrawer> | null>(null);

const draft = ref("");
const generating = ref(false);
const editingIndex = ref(-1);
const editDraft = ref("");
const streamEl = ref<HTMLElement | null>(null);
const composerEl = ref<HTMLTextAreaElement | null>(null);

// ---------- M3.4 群聊：发言权（导演调度 / 点名 · 设计 §10.5） ----------
const speakers = computed(() => props.meta.characters);
/** "" = 导演调度（多角色缺省：发言权打分选出 1–N 位按序接话）；否则为点名角色的目录名 */
const speaker = ref("");
watch(
  speakers,
  (list) => {
    // 1v1 无导演；点名对象不在阵容里时回到导演调度
    if (!speaker.value && list.length > 1) return;
    if (!list.includes(speaker.value)) speaker.value = "";
  },
  { immediate: true },
);
/** 每轮发言数上限（导演调度的限流旋钮；0 = 恢复缺省 2） */
const maxSpeakers = ref(props.meta.max_speakers ?? 2);
watch(
  () => props.meta.max_speakers,
  (n) => {
    maxSpeakers.value = n ?? 2;
  },
);
async function setMaxSpeakers(n: number) {
  maxSpeakers.value = n;
  try {
    await api.setMaxSpeakers(props.meta.id, n);
  } catch (e) {
    error.value = String(e);
  }
}

/** 即兴模式开关（M3.8 · 设计 §6.8-4，默认关） */
const improv = ref(props.meta.improv ?? false);
watch(
  () => props.meta.improv,
  (n) => {
    improv.value = n ?? false;
  },
);
async function setImprov(on: boolean) {
  improv.value = on;
  try {
    await api.setImprov(props.meta.id, on);
  } catch (e) {
    error.value = String(e);
  }
}

// ---------- M3.2 场景与多线（设计 §10.3）：场景条 + 消息按场景分段 ----------
const scenes = ref<Scene[]>([]);
const activeScene = ref("");
const sceneBusy = ref(false);

// ---------- 剧场模式（M3.6 · 设计 §10.5）：自动轮次 + 导演树进度 ----------
// autoAdvance 要调 chat.sendText、finishGeneration 要调 refreshTheater/autoAdvance——
// 双向接环用闭包解：theater 拿 () => chat.sendText，chat 拿 theaterApi。
const theaterCtl = useTheater({
  error,
  generating,
  isDisposed: () => disposed,
  loadScenes: () => loadScenes(),
  sendText: (content) => chat.sendText(content),
});
/** 剧场视图（模板与收尾共用；useTheater 内部状态的顶层绑定） */
const theater = theaterCtl.theater;

const chat = useChatStream({
  sessionId: () => props.meta.id,
  error,
  generating,
  messages,
  blackboard,
  bbForm,
  editingIndex,
  activeScene, // 场景状态在下方声明（const 提升前的引用只发生在收尾回调里）
  speaker,
  inspView,
  cardName,
  streamEl,
  composerEl,
  jumpToLastPage,
  refreshInspector,
  isDisposed: () => disposed,
  theaterApi: {
    theater: theaterCtl.theater,
    refreshTheater: theaterCtl.refreshTheater,
    autoAdvance: theaterCtl.autoAdvance,
  },
  onRoundEnd: () => {
    // 刚聊完一轮：抽屉开着就顺手重拉（设计 §8/§9 的「面板可查」）
    if (panel.value === "inspector") inspectorDrawer.value?.refreshAfterRound();
  },
});
const {
  streams,
  scheduleNote,
  cardState,
  memory,
  hookEvents,
  hookLogs,
  lastReport,
  sendText,
  reroll,
  stop,
  refreshCard,
  clearHookEvents,
  scrollToBottom,
} = chat;

/** 输入框占位（随发言权模式变化） */
const placeholder = computed(() => {
  if (speakers.value.length <= 1) return "说点什么…";
  return speaker.value ? `点名${speaker.value}：对她说点什么…` : "说点什么，导演安排谁接话…";
});

/** 最近一次表情事件：会话头部的徽标（表情位的 M1 占位显示） */
const mood = computed(() => {
  const hit = hookEvents.value.find((e) => e.kind === "emotion");
  return hit ? hit.value : "";
});

const lastIndex = computed(() => messages.value.length - 1);
/** 视图末条消息的全量下标：重roll 只对整条流的末尾有效（后端语义如此），
 *  所以只有「当前场景正对着流的末尾」时才亮重roll */
const viewLastFullIndex = computed(
  () => sceneMessagesView.value[sceneMessagesView.value.length - 1]?.index ?? -1,
);

// ---------- 分页：多轮对话按页翻，每页 20 条消息 ----------
const PAGE_SIZE = 20;
const page = ref(1);

// ---------- M3.2 场景与多线（设计 §10.3）：场景条 + 消息按场景分段（状态声明在发送接线之前） ----------

/** 消息的场景视图：只显示聚焦场景的分段（index 保留在**全量**消息流里的下标，
 *  编辑/删除仍按全量下标发给后端）。过渡插页（system）永远显示。 */
const sceneMessagesView = computed(() =>
  messages.value
    .map((m, index) => ({ m, index }))
    .filter(
      ({ m }) =>
        m.role === "system" ||
        !scenes.value.length ||
        (m.scene_id ?? "scene.main") === (activeScene.value || "scene.main"),
    ),
);

const pageCount = computed(() =>
  Math.max(1, Math.ceil(sceneMessagesView.value.length / PAGE_SIZE)),
);
const isLastPage = computed(() => page.value >= pageCount.value);
const pagedMessages = computed(() => {
  const start = (page.value - 1) * PAGE_SIZE;
  return sceneMessagesView.value.slice(start, start + PAGE_SIZE);
});
/** 页码窗口：最多 5 个 */
const pageNumbers = computed(() => {
  const total = pageCount.value;
  const span = 5;
  const start = Math.max(1, Math.min(page.value - 2, total - span + 1));
  const end = Math.min(total, start + span - 1);
  return Array.from({ length: Math.max(0, end - start + 1) }, (_, i) => start + i);
});

async function loadScenes() {
  const id = props.meta.id;
  try {
    const view: SceneView = await api.listScenes(id);
    if (id !== props.meta.id) return; // B4：快速切换会话时，迟到响应不得覆盖新会话
    scenes.value = view.scenes;
    if (view.active && view.active !== activeScene.value) {
      activeScene.value = view.active;
    } else if (!view.active) {
      activeScene.value = view.scenes[0]?.id ?? "";
    }
  } catch {
    /* 场景读取失败不阻塞聊天（旧后端/浏览器 mock 未覆盖时单场景照常用） */
  }
}

/** 切场：后端冻结原场景并插入过渡插页 */
async function switchTo(sc: Scene) {
  if (sc.id === activeScene.value || sceneBusy.value || sc.status === "merged") return;
  sceneBusy.value = true;
  error.value = "";
  try {
    const view = await api.switchScene(props.meta.id, sc.id);
    scenes.value = view.scenes;
    activeScene.value = view.active ?? sc.id;
    messages.value = await api.readMessages(props.meta.id);
    editingIndex.value = -1; // B3：messages 被外部替换，编辑框的旧下标不再可信
    void jumpToLastPage();
  } catch (e) {
    error.value = String(e);
  } finally {
    sceneBusy.value = false;
  }
}

/** M3.11 场景操作向导：新建/分场/合场/编辑走应用内对话框（替换 window.prompt/confirm） */
const sceneDlgOpen = ref(false);
const sceneDlgKind = ref<"create" | "split" | "merge" | "edit">("create");
function openSceneDialog(kind: "create" | "split" | "merge" | "edit") {
  if (sceneBusy.value) return;
  if (kind === "merge" && !scenes.value.some((sc) => sc.id !== activeScene.value && sc.status !== "merged")) {
    error.value = "没有可以并进来的场景";
    return;
  }
  sceneDlgKind.value = kind;
  sceneDlgOpen.value = true;
}

/** 场景操作提交：一个出口分派四类动作，成功后统一刷新（场景条 + 消息流 + 页脚对齐） */
async function onSceneSubmit(p: SceneSubmit) {
  sceneBusy.value = true;
  error.value = "";
  try {
    let view: SceneView;
    if (p.kind === "create") {
      view = await api.createScene(props.meta.id, p.title, p.place, p.actors);
    } else if (p.kind === "split") {
      view = await api.splitScene(props.meta.id, p.title, p.place, p.moving);
    } else if (p.kind === "merge") {
      view = await api.mergeScenes(props.meta.id, [p.from]);
    } else {
      view = await api.updateScene(props.meta.id, p.sceneId, {
        title: p.title,
        place: p.place,
        actors: p.actors,
        day: p.day,
        clock: p.clock,
      });
    }
    scenes.value = view.scenes;
    // merge 不换聚焦；create/split 后端会把聚焦切到新场景
    activeScene.value = view.active ?? activeScene.value;
    messages.value = await api.readMessages(props.meta.id);
    editingIndex.value = -1; // B3：messages 被外部替换，编辑框的旧下标不再可信
    void jumpToLastPage();
  } catch (e) {
    error.value = String(e);
  } finally {
    sceneBusy.value = false;
  }
}

/** 翻页：回到最后一页时贴底，往回翻时从头看 */
async function goPage(target: number) {
  page.value = Math.min(Math.max(1, target), pageCount.value);
  await nextTick();
  const el = streamEl.value;
  if (el) el.scrollTop = isLastPage.value ? el.scrollHeight : 0;
}

/** 有新消息时贴到最后一页 */
async function jumpToLastPage() {
  await nextTick();
  page.value = pageCount.value;
  await scrollToBottom();
}

watch(pageCount, (n) => {
  if (page.value > n) page.value = n;
});

function whoFor(m: Message): string {
  if (m.role === "user") return props.meta.persona || "我";
  if (m.role === "char") return m.name || cardName.value;
  return "系统";
}

function initial(name: string): string {
  return name.trim().slice(0, 1) || "?";
}

function avatarClass(m: Message): string {
  return m.role === "user" ? "bg-primary text-primary-content" : "bg-neutral text-neutral-content";
}

/** 角色气泡的稳定颜色（M3.4 群聊：按署名取色，同一角色恒同色——多人同台一眼可分）。
 *  色相由名字哈希决定，底色/字色与主题令牌 color-mix——明暗与 33 预设下都协调，不裸写白字 */
function avatarStyle(name: string): string {
  let h = 0;
  for (const ch of name) h = (h * 31 + (ch.codePointAt(0) ?? 0)) % 360;
  const hue = `hsl(${h} 70% 55%)`;
  return (
    `background: color-mix(in oklab, ${hue} 26%, var(--color-base-200));` +
    ` color: color-mix(in oklab, ${hue} 58%, var(--color-base-content))`
  );
}

/** 消息时间戳（秒）→ HH:MM；乐观上屏的消息没有时间戳，返回空串 */
function fmtTime(ts: number): string {
  if (!ts) return "";
  const d = new Date(ts * 1000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/** 可重新生成本轮的情形：末尾角色回复（重roll）、末尾用户消息（上次生成失败后重试）；
 *  且当前场景视图必须正对着流的末尾（重roll 的目标是整条流的最后一轮） */
function canReroll(i: number, m: Message): boolean {
  return (
    !generating.value &&
    i === lastIndex.value &&
    viewLastFullIndex.value === lastIndex.value &&
    (m.role === "char" || m.role === "user")
  );
}

function togglePanel(p: "board" | "inspector") {
  panel.value = panel.value === p ? "" : p;
  if (panel.value === "inspector") void openInspector();
}

/** 抽屉头切到「检查器」页（与工具栏同一条路径：切过去顺手把数据拉上） */
async function openInspector() {
  panel.value = "inspector";
  await nextTick(); // 抽屉此刻才挂载，模板引用要等一拍
  inspectorDrawer.value?.ensureInspector();
}

// ---------- 会话装载与刷新 ----------

// ---------- M3.0 ②：宫殿记忆溯源跳转（跳回产生这条记忆的原文轮次） ----------
async function jumpToTurn(turn: number) {
  // 场景视图里找（分页基于视图）；找不到（可能在别的场景）就不跳
  const pos = sceneMessagesView.value.findIndex(({ m }) => m.turn === turn);
  if (pos < 0) return;
  // 跳过去并把抽屉收起来，让聊天流正对那一轮
  panel.value = "";
  await goPage(Math.floor(pos / PAGE_SIZE) + 1);
}

async function loadAll() {
  error.value = "";
  generating.value = false;
  streams.value = [];
  scheduleNote.value = "";
  editingIndex.value = -1;
  draft.value = "";
  page.value = 1;
  // 换会话：检查器视角复位、检查器数据作废（下次打开抽屉再拉）
  inspView.value = "";
  inspectorDrawer.value?.resetForSession();
  const id = props.meta.id;
  try {
    const [bb, msgs] = await Promise.all([api.getBlackboard(id), api.readMessages(id)]);
    if (id !== props.meta.id) return; // B4：快速切换会话时，迟到响应不得覆盖新会话
    blackboard.value = bb;
    Object.assign(bbForm, {
      day: bb.day,
      clock: bb.clock,
      place: bb.place,
      actors: bb.actors.join(", "),
    });
    messages.value = [...msgs];
    scenes.value = [];
    activeScene.value = "";
    await loadScenes();
    page.value = pageCount.value; // 打开会话时停在最新一页
    void refreshInspector();
    void refreshCard();
    void theaterCtl.refreshTheater(props.meta.id);
    void scrollToBottom();
    // 气泡署名用卡片显示名；读取失败退回目录名
    api
      .getCard(props.meta.characters[0])
      .then((d) => {
        if (d.card.name) cardName.value = d.card.name;
      })
      .catch(() => {});
  } catch (e) {
    error.value = String(e);
  }
}

/** 检查器：优先"最近一次发送"，无记录时干跑预览。
 *  选了非主角色视角时一律预览——「最近一次发送」的组装是发言人那位的 */
async function refreshInspector() {
  try {
    if (inspView.value) {
      assembly.value = await api.previewPrompt(props.meta.id, inspView.value);
      assemblySource.value = "preview";
      return;
    }
    const last = await api.lastPrompt(props.meta.id);
    if (last) {
      assembly.value = last;
      assemblySource.value = "last";
    } else {
      assembly.value = await api.previewPrompt(props.meta.id);
      assemblySource.value = "preview";
    }
  } catch {
    /* 检查器读取失败不阻塞聊天 */
  }
}

async function preview() {
  try {
    assembly.value = await api.previewPrompt(props.meta.id, inspView.value || undefined);
    assemblySource.value = "preview";
  } catch (e) {
    error.value = String(e);
  }
}

/** 抽屉内命令失败上报全局错误条（ErrorToast 与聊天共用一条） */
function onDrawerError(message: string) {
  error.value = message;
}

async function saveBlackboard() {
  savingBb.value = true;
  try {
    const bb = await api.updateBlackboard(props.meta.id, {
      day: bbForm.day || 1,
      clock: bbForm.clock.trim(),
      place: bbForm.place.trim(),
      actors: bbForm.actors
        .split(/[,，]/)
        .map((a) => a.trim())
        .filter(Boolean),
    });
    blackboard.value = bb;
    await refreshInspector(); // 场景快照随黑板变化
  } catch (e) {
    error.value = String(e);
  } finally {
    savingBb.value = false;
  }
}

// ---------- 发送 / 消息编辑 / 删除 ----------

async function send() {
  const content = draft.value.trim();
  if (!content || generating.value) return;
  draft.value = "";
  resetComposerHeight();
  await sendText(content);
}

function startEdit(i: number) {
  editingIndex.value = i;
  editDraft.value = messages.value[i].content;
}

async function saveEdit() {
  const i = editingIndex.value;
  if (i < 0 || !editDraft.value.trim()) return;
  try {
    messages.value = await api.editMessage(props.meta.id, i, editDraft.value.trim());
    editingIndex.value = -1;
    void refreshInspector(); // 历史变了，预览随之更新
  } catch (e) {
    error.value = String(e);
  }
}

// E4：删除确认收进应用内对话框（替换 window.confirm，与 M3.11 场景向导同一惯用式）
const pendingDelete = ref<number | null>(null);

function removeMsg(i: number) {
  pendingDelete.value = i;
}

async function confirmRemove() {
  const i = pendingDelete.value;
  pendingDelete.value = null;
  if (i === null) return;
  try {
    messages.value = await api.deleteMessage(props.meta.id, i);
    editingIndex.value = -1; // B3：删除使其后所有下标前移，编辑框一律关闭
    void refreshInspector();
  } catch (e) {
    error.value = String(e);
  }
}

// ---------- 输入框 ----------

/** Enter 发送、Shift+Enter 换行；IME 组词中的 Enter 不发送（中文输入关键路径） */
function onComposerKeydown(e: KeyboardEvent) {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    void send();
  }
}

function onEditKeydown(e: KeyboardEvent) {
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && !e.isComposing) {
    e.preventDefault();
    void saveEdit();
  }
  if (e.key === "Escape") editingIndex.value = -1;
}

function autoGrow() {
  const el = composerEl.value;
  if (!el) return;
  el.style.height = "auto";
  el.style.height = `${Math.min(el.scrollHeight, 140)}px`;
}

function resetComposerHeight() {
  if (composerEl.value) composerEl.value.style.height = "auto";
}

onMounted(loadAll);
watch(() => props.meta.id, loadAll);
// 检查器视角变了：卡内状态跟着换人重读；组装按视角重读（检查器重拉在抽屉内）
watch(inspView, () => {
  void refreshCard();
  void refreshInspector();
});
// 卡片热加载（M1.7）：改了 card.lua 立即重读卡内状态与署名，不必重启
watch(cardGeneration, () => {
  if (generating.value) return; // 生成中不动面板，避免读到半截状态
  void refreshCard();
  api
    .getCard(props.meta.characters[0])
    .then((d) => {
      if (d.card.name) cardName.value = d.card.name;
    })
    .catch(() => {});
});
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-4">
    <ErrorToast :message="error" @dismiss="error = ''" />

    <!-- 会话头：角色 + 状态 + 面板开关 -->
    <header class="card card-border flex-none bg-base-100">
      <div class="card-body flex-row items-center gap-3 p-3">
        <div class="avatar avatar-placeholder">
          <div class="w-10 rounded-full bg-primary/15 text-primary">
            <span class="text-sm">{{ initial(cardName) }}</span>
          </div>
        </div>

        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <h2 class="truncate text-base font-semibold">{{ cardName }}</h2>
            <!-- 群聊阵容（M3.1）：主角色之外还有谁同台 -->
            <span
              v-if="speakers.length > 1"
              class="badge badge-xs badge-soft tooltip tooltip-bottom"
              :data-tip="`多角色会话：按角色隔离组装（设计 §10.2），输入框上方选择对谁说话`"
            >
              +{{ speakers.length - 1 }} 同台
            </span>
            <span class="status status-xs status-success"></span>
            <span class="text-xs text-base-content/50">{{ generating ? "生成中" : "在场" }}</span>
            <!-- 表情位占位（M1.6）：卡片 api.ui.emit("emotion", …) 的结果 -->
            <span
              v-if="mood"
              class="badge badge-xs badge-soft badge-secondary tooltip tooltip-bottom"
              data-tip="角色表情（卡内 ui.emit，立绘差分留待资产规范落地）"
            >
              {{ kindLabel("emotion") }} · {{ mood }}
            </span>
          </div>
          <p class="mt-0.5 mb-0 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-base-content/50">
            <span class="flex items-center gap-1">
              <Icon name="clock" :size="13" />
              第 {{ blackboard?.day ?? 1 }} 天 · {{ blackboard?.clock || "时间未定" }}
            </span>
            <span class="flex items-center gap-1">
              <Icon name="pin" :size="13" />{{ blackboard?.place || "地点未定" }}
            </span>
            <span v-if="blackboard && blackboard.actors.length > 0" class="flex items-center gap-1">
              <Icon name="users" :size="13" />{{ blackboard.actors.join(" · ") }}
            </span>
          </p>
        </div>

        <div class="flex flex-none items-center gap-1">
          <button
            class="btn btn-square btn-sm btn-ghost tooltip tooltip-bottom"
            data-tip="黑板"
            :class="{ 'btn-active': panel === 'board' }"
            @click="togglePanel('board')"
          >
            <Icon name="layers" :size="16" />
          </button>
          <button
            class="btn btn-square btn-sm btn-ghost tooltip tooltip-bottom"
            data-tip="记忆检查器"
            :class="{ 'btn-active': panel === 'inspector' }"
            @click="togglePanel('inspector')"
          >
            <Icon name="cpu" :size="16" />
          </button>
        </div>
      </div>
    </header>

    <!-- 场景条（M3.2 · 设计 §10.3）：多场景会话的「与此同时」切换 -->
    <SceneBar
      v-if="scenes.length > 0"
      :scenes="scenes"
      :active-scene="activeScene"
      :busy="sceneBusy"
      @switch="switchTo"
      @edit="openSceneDialog('edit')"
      @create="openSceneDialog('create')"
      @split="openSceneDialog('split')"
      @merge="openSceneDialog('merge')"
    />

    <div class="relative flex min-h-0 flex-1 gap-4">
      <!-- 聊天流 + 输入区 -->
      <section class="card card-border min-w-0 flex-1 overflow-hidden bg-base-100">
        <ul ref="streamEl" class="m-0 flex min-h-0 flex-1 list-none flex-col gap-5 overflow-y-auto p-5">
          <template v-for="{ m, index: i } in pagedMessages" :key="i">
            <!-- 过渡插页（切场/分场/合场的叙事接缝）：居中一条，不占气泡 -->
            <li v-if="m.role === 'system'" class="mx-auto my-1 flex max-w-full list-none items-center gap-3">
              <span class="h-px flex-1 bg-base-content/15"></span>
              <span class="flex-none font-serif text-sm italic tracking-wide text-base-content/50">
                {{ m.content }}
              </span>
              <span class="h-px flex-1 bg-base-content/15"></span>
            </li>
            <li v-else class="chat group" :class="m.role === 'user' ? 'chat-end' : 'chat-start'">
            <div class="chat-image avatar avatar-placeholder">
              <div
                class="w-9 rounded-full"
                :class="avatarClass(m)"
                :style="m.role === 'char' ? avatarStyle(whoFor(m)) : undefined"
              >
                <span class="text-xs">{{ initial(whoFor(m)) }}</span>
              </div>
            </div>
            <div class="chat-header mb-0.5 text-xs font-medium text-base-content/60">{{ whoFor(m) }}</div>

            <template v-if="editingIndex === i">
              <div class="chat-bubble w-full max-w-[min(76%,72ch)] bg-base-200 p-2">
                <textarea
                  class="textarea textarea-sm textarea-ghost w-full leading-relaxed"
                  v-model="editDraft"
                  rows="3"
                  @keydown="onEditKeydown"
                ></textarea>
              </div>
              <div class="chat-footer mt-1 flex flex-wrap items-center gap-2">
                <button class="btn btn-ghost btn-xs text-primary" @click="saveEdit">保存</button>
                <button class="btn btn-ghost btn-xs" @click="editingIndex = -1">取消</button>
                <span class="flex items-center gap-1 text-[11px] text-base-content/40">
                  <kbd class="kbd kbd-xs">Ctrl</kbd>+<kbd class="kbd kbd-xs">Enter</kbd> 保存 ·
                  <kbd class="kbd kbd-xs">Esc</kbd> 取消
                </span>
              </div>
            </template>

            <template v-else>
              <div
                class="chat-bubble max-w-[min(76%,72ch)] leading-relaxed break-words whitespace-pre-wrap"
                :class="m.role === 'user' ? 'chat-bubble-primary' : 'bg-base-300 text-base-content'"
              >
                {{ m.content }}
              </div>
              <div class="chat-footer mt-1 flex items-center gap-2 text-[11px] text-base-content/40">
                <span v-if="fmtTime(m.ts)">{{ fmtTime(m.ts) }}</span>
                <span
                  v-if="!generating"
                  class="flex items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100 max-lg:opacity-70"
                >
                  <button class="btn btn-ghost btn-xs" @click="startEdit(i)">
                    <Icon name="edit" :size="13" />编辑
                  </button>
                  <button v-if="canReroll(i, m)" class="btn btn-ghost btn-xs" @click="reroll">
                    <Icon name="refresh" :size="13" />{{ m.role === "char" ? "重roll" : "重试生成" }}
                  </button>
                  <button class="btn btn-ghost btn-xs text-error" @click="removeMsg(i)">
                    <Icon name="trash" :size="13" />删除
                  </button>
                </span>
              </div>
            </template>
            </li>
          </template>

          <!-- 流式区（只在最后一页显示）：导演调度指示 + 各位发言人的流式气泡 -->
          <template v-if="generating && isLastPage">
            <li v-if="scheduleNote" class="mx-auto my-1 flex max-w-full list-none items-center gap-3">
              <span class="h-px flex-1 bg-base-content/15"></span>
              <span class="flex-none text-xs tracking-wide text-base-content/50">{{ scheduleNote }}</span>
              <span class="h-px flex-1 bg-base-content/15"></span>
            </li>
            <li v-for="(s, si) in streams" :key="`stream-${si}-${s.name}`" class="chat chat-start">
              <div class="chat-image avatar avatar-placeholder">
                <div class="w-9 rounded-full" :style="avatarStyle(s.name)">
                  <span class="text-xs">{{ initial(s.name) }}</span>
                </div>
              </div>
              <div class="chat-header mb-0.5 text-xs font-medium text-base-content/60">{{ s.name }}</div>
              <div
                class="chat-bubble max-w-[min(76%,72ch)] border border-primary/40 bg-base-300 leading-relaxed break-words whitespace-pre-wrap text-base-content"
              >
                {{ s.text }}<span class="ml-0.5 inline-block h-[1em] w-0.5 animate-pulse bg-primary align-[-0.15em]" aria-hidden="true"></span>
              </div>
              <div class="chat-footer mt-1 flex items-center gap-2 text-[11px] text-base-content/45">
                <span class="loading loading-dots loading-xs text-primary"></span>
                正在写…
              </div>
            </li>
            <!-- 第一位还没开口：先给一条生成中提示，气泡等第一个增量到了再出现 -->
            <li v-if="streams.length === 0" class="chat chat-start">
              <div class="chat-image avatar avatar-placeholder">
                <div class="w-9 rounded-full bg-neutral text-neutral-content">
                  <span class="text-xs">{{ initial(speaker || cardName) }}</span>
                </div>
              </div>
              <div class="chat-bubble border border-primary/40 bg-base-300">
                <span class="loading loading-dots loading-sm text-primary"></span>
              </div>
            </li>
          </template>

          <li
            v-if="sceneMessagesView.length === 0 && !generating"
            class="mt-10 self-center text-sm text-base-content/45"
          >
            还没有消息。说点什么，把这场戏开起来。
          </li>
        </ul>

        <!-- 分页：多轮对话按页翻 -->
        <div
          v-if="messages.length > 0"
          class="flex flex-none flex-wrap items-center justify-between gap-2 border-t border-base-300 px-3 py-2"
        >
          <span class="text-[11px] text-base-content/45">
            第 {{ page }}/{{ pageCount }} 页 · 共 {{ sceneMessagesView.length }} 条 · 每页 {{ PAGE_SIZE }} 条
          </span>
          <div class="join">
            <button class="btn join-item btn-xs" :disabled="page <= 1" @click="goPage(page - 1)">
              <Icon name="chevron" :size="13" class="rotate-180" />上一页
            </button>
            <button
              v-for="n in pageNumbers"
              :key="n"
              class="btn join-item btn-xs"
              :class="{ 'btn-active': n === page }"
              :aria-current="n === page ? 'page' : undefined"
              @click="goPage(n)"
            >
              {{ n }}
            </button>
            <button class="btn join-item btn-xs" :disabled="page >= pageCount" @click="goPage(page + 1)">
              下一页<Icon name="chevron" :size="13" />
            </button>
          </div>
        </div>

        <!-- 剧场模式条（M3.6 · 设计 §10.5）：导演树阶段 + 轮数预算进度 -->
        <div
          v-if="theater?.on"
          class="flex flex-none flex-wrap items-center gap-2 border-t border-base-300 px-3 py-1.5 text-xs"
        >
          <span class="flex-none font-medium tracking-wide text-primary">剧场</span>
          <span class="flex-none text-base-content/80">「{{ theater?.path[theater?.path.length - 1] || "…" }}」</span>
          <span class="min-w-0 flex-1 truncate text-base-content/45" :title="theater?.stage_directive">
            {{ theater?.stage_directive }}
          </span>
          <progress
            class="progress progress-primary w-24 flex-none"
            :value="theater?.used"
            :max="theater?.budget || 1"
            aria-label="剧场进度"
          ></progress>
          <span class="flex-none text-base-content/45">{{ theater?.used }}/{{ theater?.budget }} 轮</span>
          <button
            class="btn btn-ghost btn-xs flex-none"
            :disabled="generating"
            data-tip="停掉自动轮次（已走到的阶段保留）"
            @click="theaterCtl.setTheater(meta.id, false)"
          >
            收棚
          </button>
        </div>

        <!-- 输入区：daisyUI textarea + 圆形发送键；多角色时带发言权选择（M3.4 · 设计 §10.5） -->
        <form class="flex-none border-t border-base-300 p-3" @submit.prevent="send">
          <div v-if="speakers.length > 1" class="mb-2 flex flex-wrap items-center gap-1.5">
            <span class="text-[11px] text-base-content/45">发言权</span>
            <div role="tablist" class="tabs tabs-box tabs-xs">
              <button
                role="tab"
                class="tab tooltip tooltip-bottom"
                :class="{ 'tab-active': !speaker }"
                data-tip="导演按相关性打分选人接话（最近提及 · 场景 · 剧情线 · 想说话）"
                @click="speaker = ''"
              >
                导演调度
              </button>
              <button
                v-for="dir in speakers"
                :key="dir"
                role="tab"
                class="tab tooltip tooltip-bottom"
                :class="{ 'tab-active': speaker === dir }"
                :data-tip="`点名${dir}：只让她接话，不经调度`"
                @click="speaker = dir"
              >
                点名 {{ dir }}
              </button>
            </div>
            <label
              class="flex items-center gap-1 text-[11px] text-base-content/45"
              data-tip="即兴模式（默认关）：被提及的实体太薄时，便宜模型现场补一条「设定·暂定」；会话结束进收件箱确认"
            >
              <input
                type="checkbox"
                class="toggle toggle-xs"
                :checked="improv"
                aria-label="即兴模式"
                @change="setImprov(($event.target as HTMLInputElement).checked)"
              />
              即兴
            </label>
            <label v-if="!speaker" class="ml-auto flex items-center gap-1 text-[11px] text-base-content/45">
              每轮至多
              <select
                class="select select-bordered select-xs"
                :value="maxSpeakers"
                aria-label="每轮发言数上限"
                @change="setMaxSpeakers(Number(($event.target as HTMLSelectElement).value))"
              >
                <option :value="1">1 人</option>
                <option :value="2">2 人</option>
                <option :value="3">3 人</option>
              </select>
            </label>
          </div>
          <div class="flex items-end gap-2">
            <textarea
              ref="composerEl"
              class="textarea max-h-40 w-full flex-1 resize-none leading-relaxed"
              v-model="draft"
              rows="1"
              :placeholder="placeholder"
              aria-label="消息输入框"
              @keydown="onComposerKeydown"
              @input="autoGrow"
            ></textarea>
            <button
              v-if="!generating"
              class="btn btn-circle btn-sm btn-primary"
              type="submit"
              :disabled="!draft.trim()"
              aria-label="发送"
            >
              <Icon name="send" :size="16" />
            </button>
            <button
              v-else
              class="btn btn-circle btn-sm btn-error"
              type="button"
              aria-label="停止生成"
              @click="stop"
            >
              <span class="size-3 rounded-xs bg-current"></span>
            </button>
          </div>
          <p class="mt-2 mb-0 flex flex-wrap items-center gap-1.5 text-[11px] text-base-content/45">
            <kbd class="kbd kbd-xs">Enter</kbd> 发送 ·
            <kbd class="kbd kbd-xs">Shift</kbd>+<kbd class="kbd kbd-xs">Enter</kbd> 换行
            <button
              v-if="!theater?.on"
              class="btn btn-ghost btn-xs ml-auto"
              :disabled="generating"
              data-tip="剧场模式：导演树起承转合自动跑一轮数预算（缺省 20 轮），多场景时交叉剪辑"
              @click="theaterCtl.setTheater(meta.id, true)"
            >
              <Icon name="play" :size="13" />剧场
            </button>
          </p>
        </form>
      </section>

      <!-- 右侧抽屉：黑板 / 记忆检查器 -->
      <aside
        v-if="panel"
        class="card card-border flex w-80 flex-none flex-col overflow-hidden bg-base-100 max-[900px]:absolute max-[900px]:inset-y-0 max-[900px]:right-0 max-[900px]:z-10 max-[900px]:w-[min(340px,92vw)]"
      >
        <header class="flex flex-none items-center justify-between gap-2 border-b border-base-300 px-3 py-2">
          <div role="tablist" class="tabs tabs-box tabs-xs">
            <button role="tab" class="tab" :class="{ 'tab-active': panel === 'board' }" @click="panel = 'board'">
              黑板
            </button>
            <button
              role="tab"
              class="tab"
              :class="{ 'tab-active': panel === 'inspector' }"
              @click="openInspector()"
            >
              检查器
            </button>
          </div>
          <button class="btn btn-square btn-ghost btn-xs" aria-label="收起面板" @click="panel = ''">
            <Icon name="close" :size="15" />
          </button>
        </header>

        <div v-if="panel === 'board'" class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4">
          <p class="m-0 text-xs text-base-content/50">保存后下一轮组装生效；每轮回复后时钟 +10 分钟。</p>
          <div class="grid grid-cols-2 gap-3">
            <div>
              <label class="label" for="bb-day">第几天</label>
              <input id="bb-day" class="input input-sm w-full" v-model.number="bbForm.day" type="number" min="1" />
            </div>
            <div>
              <label class="label" for="bb-clock">时间</label>
              <input id="bb-clock" class="input input-sm w-full" v-model="bbForm.clock" placeholder="21:30" />
            </div>
          </div>
          <div>
            <label class="label" for="bb-place">地点</label>
            <input id="bb-place" class="input input-sm w-full" v-model="bbForm.place" placeholder="图书馆自习区" />
          </div>
          <div>
            <label class="label" for="bb-actors">在场（逗号分隔）</label>
            <input id="bb-actors" class="input input-sm w-full" v-model="bbForm.actors" placeholder="小雨, 玩家" />
          </div>
          <div class="flex justify-end">
            <button class="btn btn-primary btn-sm" :disabled="savingBb" @click="saveBlackboard">
              <span v-if="savingBb" class="loading loading-spinner loading-xs"></span>
              {{ savingBb ? "保存中…" : "保存黑板" }}
            </button>
          </div>
        </div>

        <!-- 检查器抽屉（KeepAlive：黑板↔检查器来回切不丢检查器状态） -->
        <KeepAlive>
          <InspectorDrawer
            v-if="panel === 'inspector'"
            ref="inspectorDrawer"
            :session-id="meta.id"
            :world="meta.world"
            :speakers="speakers"
            :card-name="cardName"
            :assembly="assembly"
            :assembly-source="assemblySource"
            v-model:view="inspView"
            v-model:tab="inspTab"
            :card-state="cardState"
            :memory="memory"
            :last-report="lastReport"
            :hook-events="hookEvents"
            :hook-logs="hookLogs"
            @jump="jumpToTurn"
            @preview="preview"
            @refresh-assembly="refreshInspector"
            @error="onDrawerError"
            @refresh-card="refreshCard"
            @clear-events="clearHookEvents"
          />
        </KeepAlive>
      </aside>
    </div>

    <!-- M3.11 场景操作向导：新建 / 分场 / 合场 / 编辑 -->
    <!-- E4：删除消息的应用内确认 -->
    <div v-if="pendingDelete !== null" class="modal modal-open">
      <div class="modal-box max-w-sm text-sm">
        <p class="m-0">删除这条消息？它之后轮次的派生效果（好感度、状态树、时钟）会一并重算。</p>
        <div class="modal-action">
          <button class="btn btn-ghost btn-sm" @click="pendingDelete = null">取消</button>
          <button class="btn btn-error btn-sm" @click="confirmRemove">删除</button>
        </div>
      </div>
      <div class="modal-backdrop" @click="pendingDelete = null"></div>
    </div>

    <SceneDialog
      v-model="sceneDlgOpen"
      :kind="sceneDlgKind"
      :scenes="scenes"
      :active-scene-id="activeScene"
      :cast="speakers"
      :busy="sceneBusy"
      @submit="onSceneSubmit"
    />
  </div>
</template>
