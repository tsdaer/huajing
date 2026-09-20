<script setup lang="ts">
import { computed, nextTick, onMounted, reactive, ref, watch } from "vue";
import { api } from "../api";
import { cardGeneration } from "../cards";
import type {
  Blackboard,
  HookReport,
  InspectorData,
  MemRecord,
  Message,
  PromptAssembly,
  Scene,
  SceneView,
  SessionMeta,
  StreamEvent,
  TimelineEntry,
} from "../types";
import ErrorToast from "../components/ErrorToast.vue";
import Icon from "../components/Icon.vue";
import StatePathPanel from "../components/inspector/StatePathPanel.vue";
import ThreadsPanel from "../components/inspector/ThreadsPanel.vue";
import PsychePanel from "../components/inspector/PsychePanel.vue";
import PalacePanel from "../components/inspector/PalacePanel.vue";
import CodexPanel from "../components/inspector/CodexPanel.vue";
import SummaryPanel from "../components/inspector/SummaryPanel.vue";

// M1.5 聊天界面：气泡流 + 流式打字机 + 停止 + 消息编辑/重roll/删除。
// 黑板与记忆检查器收进右侧抽屉，聊天流为主。界面全部由 daisyUI 组件构成。

const props = defineProps<{ meta: SessionMeta }>();

const error = ref("");
const messages = ref<Message[]>([]);
const blackboard = ref<Blackboard | null>(null);
const assembly = ref<PromptAssembly | null>(null);

/** 预算账目里该层的裁剪说明（M2.7 · 设计 §4.2）；没被动过则 undefined */
function layerTrimmed(id: string, name: string): string | undefined {
  return assembly.value?.budget?.layers.find((u) => u.id === id && u.name === name)?.trimmed;
}
const assemblySource = ref<"last" | "preview">("preview");
const cardName = ref(props.meta.characters[0] ?? "角色");

const panel = ref<"" | "board" | "inspector">("");
const bbForm = reactive({ day: 1, clock: "", place: "", actors: "" });
const savingBb = ref(false);

// ---------- 记忆检查器：页签与数据（M1.6 的四个可观测面 + M2.8 的六个面板） ----------
/** 检查器页签：M1 的四个可观测面 + M2.8 的六个记忆检查器面板 */
const INSP_TABS = [
  { id: "layers", label: "注入层" },
  { id: "statetree", label: "状态路径" },
  { id: "threads", label: "剧情线" },
  { id: "psyche", label: "心理" },
  { id: "palace", label: "宫殿" },
  { id: "codex", label: "设定集" },
  { id: "outbox", label: "摘要·收件箱" },
  { id: "state", label: "卡内状态" },
  { id: "memory", label: "卡内记忆" },
  { id: "events", label: "事件流" },
] as const;

type InspTab = (typeof INSP_TABS)[number]["id"];

/** M2.8 面板组：共享一份 inspectorData（注入层仍走 preview/last_prompt） */
const M2_TABS: InspTab[] = ["statetree", "threads", "psyche", "palace", "codex", "outbox"];

const inspTab = ref<InspTab>("layers");
const inspTabLabel = computed(() => INSP_TABS.find((t) => t.id === inspTab.value)?.label ?? "");

// 切到事件流页签时按需拉一次类型化事件流（M3.0 ④）
watch(inspTab, (tab) => {
  if (tab === "events") ensureTimeline();
});

/** 记忆检查器全量投影（状态树 / 剧情线 / 心理 / 宫殿 / 设定集 / 摘要与收件箱） */
const inspector = ref<InspectorData | null>(null);
const inspLoading = ref(false);
const inspError = ref("");
/** 「立即总结」的进行态与结果（总结可能较慢） */
const summarizing = ref(false);
const summaryResult = ref("");
/** 正在确认/否决的提案 id */
const decidingId = ref("");

/** 拉一次检查器数据；失败只在面板里提示，不打断聊天（浏览器 mock 未覆盖该命令时也走这里） */
async function loadInspector() {
  inspLoading.value = true;
  inspError.value = "";
  try {
    inspector.value = await api.inspectorData(props.meta.id);
  } catch (e) {
    inspError.value = String(e);
  } finally {
    inspLoading.value = false;
  }
}

/** 打开抽屉时拉一次；已经有数据就复用缓存，重拉交给「刷新」 */
function ensureInspector() {
  if (!inspector.value && !inspLoading.value) void loadInspector();
}

/** 抽屉头切到「检查器」页（与工具栏同一条路径：切过去顺手把数据拉上） */
function openInspector() {
  panel.value = "inspector";
  ensureInspector();
}

/** 收件箱：确认 / 否决一条提案，动作进事件流，成功后刷新全量视图 */
async function decideProposal(id: string, accept: boolean) {
  decidingId.value = id;
  try {
    await api.decideProposal(props.meta.id, id, accept);
    await loadInspector();
  } catch (e) {
    error.value = String(e);
  } finally {
    decidingId.value = "";
  }
}

/** 手动触发一次总结（正常路径是消息滑出窗口后自动触发） */
async function summarizeNow() {
  if (summarizing.value) return;
  summarizing.value = true;
  summaryResult.value = "";
  error.value = "";
  try {
    summaryResult.value = await api.summarizeNow(props.meta.id);
    await loadInspector();
    void refreshInspector();
  } catch (e) {
    error.value = String(e);
  } finally {
    summarizing.value = false;
  }
}

// ---------- M3.0 ①：剧情线手动开/收线（设计 §8.3，后端命令的 UI 入口） ----------
const threadBusy = ref(false);

async function openThread(draft: { title: string; cause: string; actors: string[]; importance: number }) {
  if (threadBusy.value) return;
  threadBusy.value = true;
  error.value = "";
  try {
    await api.openThread(props.meta.id, draft.title, draft.cause, draft.actors, draft.importance);
    await loadInspector();
    void refreshInspector(); // 线影响 B1/C1 组装
  } catch (e) {
    error.value = String(e);
  } finally {
    threadBusy.value = false;
  }
}

async function resolveThreadCmd(id: string, outcome: string) {
  if (threadBusy.value) return;
  threadBusy.value = true;
  error.value = "";
  try {
    await api.resolveThread(props.meta.id, id, outcome);
    await loadInspector();
    void refreshInspector(); // 收线三件事都会改变组装（B1「了结未远」/ C1 清空/转移）
  } catch (e) {
    error.value = String(e);
  } finally {
    threadBusy.value = false;
  }
}

// ---------- M3.0 ②：宫殿记忆溯源跳转（跳回产生这条记忆的原文轮次） ----------
async function jumpToTurn(turn: number) {
  // 场景视图里找（分页基于视图）；找不到（可能在别的场景）就不跳
  const pos = sceneMessagesView.value.findIndex(({ m }) => m.turn === turn);
  if (pos < 0) return;
  // 跳过去并把抽屉收起来，让聊天流正对那一轮
  panel.value = "";
  await goPage(Math.floor(pos / PAGE_SIZE) + 1);
}

// ---------- M3.0 ④：类型化事件流视图（session_timeline） ----------
const timeline = ref<TimelineEntry[] | null>(null);
const timelineLoading = ref(false);
const timelineError = ref("");

async function loadTimeline() {
  timelineLoading.value = true;
  timelineError.value = "";
  try {
    timeline.value = await api.sessionTimeline(props.meta.id);
  } catch (e) {
    timelineError.value = String(e);
  } finally {
    timelineLoading.value = false;
  }
}

/** 事件流页签展开时拉一次（聊天推进后想看新的，点「刷新」） */
function ensureTimeline() {
  if (timeline.value === null && !timelineLoading.value) void loadTimeline();
}

// ---------- M1.6：卡内状态 / 记忆 / 界面事件 ----------
const cardState = ref<Record<string, unknown>>({});
const memory = ref<MemRecord[]>([]);
/** 卡内界面事件（api.ui.emit）：最近 50 条，最新的在前 */
const hookEvents = ref<{ kind: string; value: string; turn: number }[]>([]);
/** 本轮生成期间收到的钩子报告（done 事件另带一份） */
const lastReport = ref<HookReport | null>(null);
/** 卡内错误日志（沙箱错误边界捕获；只在面板里显示） */
const hookLogs = ref<string[]>([]);

const KIND_LABEL: Record<string, string> = {
  emotion: "表情",
  bgm: "音效",
  sprite: "立绘",
  effect: "特效",
};

function kindLabel(kind: string): string {
  return KIND_LABEL[kind] ?? kind;
}

/** 最近一次表情事件：会话头部的徽标（表情位的 M1 占位显示） */
const mood = computed(() => {
  const hit = hookEvents.value.find((e) => e.kind === "emotion");
  return hit ? hit.value : "";
});

/** 卡内状态行（顶层键值；嵌套对象折叠成 JSON 单行） */
const stateRows = computed(() =>
  Object.entries(cardState.value).map(([k, v]) => ({
    key: k,
    value: typeof v === "object" && v !== null ? JSON.stringify(v) : String(v),
  })),
);

/** 记忆流按时间倒序（最新在前） */
const memoryRows = computed(() => [...memory.value].reverse());

function fmtTs(ts: number): string {
  if (!ts) return "";
  const d = new Date(ts * 1000);
  return `${d.getMonth() + 1}/${d.getDate()} ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

function fmtValue(v: unknown): string {
  if (v === null || v === undefined) return "nil";
  return typeof v === "object" ? JSON.stringify(v) : String(v);
}

const draft = ref("");
const generating = ref(false);
const streamText = ref("");
const editingIndex = ref(-1);
const editDraft = ref("");

// ---------- M3.1 群聊：发言人选择（多角色时对谁说话；1v1 恒主角色） ----------
const speakers = computed(() => props.meta.characters);
const speaker = ref("");
watch(
  speakers,
  (list) => {
    if (!list.includes(speaker.value)) speaker.value = list[0] ?? "";
  },
  { immediate: true },
);

const streamEl = ref<HTMLElement | null>(null);
const composerEl = ref<HTMLTextAreaElement | null>(null);

const lastIndex = computed(() => messages.value.length - 1);
/** 视图末条消息的全量下标：重roll 只对整条流的末尾有效（后端语义如此），
 *  所以只有「当前场景正对着流的末尾」时才亮重roll */
const viewLastFullIndex = computed(
  () => sceneMessagesView.value[sceneMessagesView.value.length - 1]?.index ?? -1,
);

// ---------- 分页：多轮对话按页翻，每页 20 条消息 ----------
const PAGE_SIZE = 20;
const page = ref(1);

// ---------- M3.2 场景与多线（设计 §10.3）：场景条 + 消息按场景分段 ----------
const scenes = ref<Scene[]>([]);
const activeScene = ref("");
const sceneBusy = ref(false);

/** 当前场景的信息（冻结标识、在场者） */
const activeSceneInfo = computed(() => scenes.value.find((sc) => sc.id === activeScene.value));

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
  try {
    const view: SceneView = await api.listScenes(props.meta.id);
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

/** 场景条上的一条的提示文字（时间线 + 状态） */
function sceneTip(sc: Scene): string {
  const time = sc.clock ? `第${sc.day}天 ${sc.clock}` : `第${sc.day}天`;
  const status = sc.status === "frozen" ? "（已冻结，切回原地继续）" : sc.status === "merged" ? "（已并入他场）" : "";
  return `${time} · ${sc.place || "地点未定"} · 在场 ${sc.actors.length} 人${status}`;
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
    void jumpToLastPage();
  } catch (e) {
    error.value = String(e);
  } finally {
    sceneBusy.value = false;
  }
}

/** 新建场景（prompt 三连：标题/地点/在场者——v0 用系统对话框，M3.10 再换成向导） */
async function createSceneCmd() {
  const title = window.prompt("新场景的标题（如：坡下的旧书店）");
  if (!title?.trim()) return;
  const place = window.prompt("新场景的地点") ?? "";
  const castHint = props.meta.characters.join(", ");
  const actorsRaw = window.prompt(`在场角色（逗号分隔；可选：${castHint}）`, castHint) ?? "";
  sceneBusy.value = true;
  error.value = "";
  try {
    const view = await api.createScene(
      props.meta.id,
      title.trim(),
      place.trim(),
      actorsRaw
        .split(/[,，]/)
        .map((a) => a.trim())
        .filter(Boolean),
    );
    scenes.value = view.scenes;
    activeScene.value = view.active ?? "";
    messages.value = await api.readMessages(props.meta.id);
    void jumpToLastPage();
  } catch (e) {
    error.value = String(e);
  } finally {
    sceneBusy.value = false;
  }
}

/** 分场：挑人离场另立场景（视角跟到新场景） */
async function splitSceneCmd() {
  const here = activeSceneInfo.value;
  const hint = here?.actors.join(", ") ?? props.meta.characters.join(", ");
  const movingRaw = window.prompt(`分场：哪些角色离场？（逗号分隔；此刻在场：${hint}）`);
  if (!movingRaw?.trim()) return;
  const moving = movingRaw
    .split(/[,，]/)
    .map((a) => a.trim())
    .filter(Boolean);
  const title = window.prompt("新场景的标题（如：天台）");
  if (!title?.trim()) return;
  const place = window.prompt("新场景的地点") ?? "";
  sceneBusy.value = true;
  error.value = "";
  try {
    const view = await api.splitScene(props.meta.id, title.trim(), place.trim(), moving);
    scenes.value = view.scenes;
    activeScene.value = view.active ?? "";
    messages.value = await api.readMessages(props.meta.id);
    void jumpToLastPage();
  } catch (e) {
    error.value = String(e);
  } finally {
    sceneBusy.value = false;
  }
}

/** 合场：把另一路场景并进当前场景（对齐需确认——在场者并集、时间取较晚） */
async function mergeSceneCmd() {
  const candidates = scenes.value.filter(
    (sc) => sc.id !== activeScene.value && sc.status !== "merged",
  );
  if (!candidates.length) {
    error.value = "没有可以并进来的场景";
    return;
  }
  const listed = candidates.map((sc, i) => `${i + 1}. ${sc.title}（${sc.place}）`).join("\n");
  const pick = window.prompt(
    `合场：把哪一路并进「${activeSceneInfo.value?.title ?? "当前场景"}」？\n${listed}\n\n输入序号（合场后各角色记忆不合并，只并舞台）：`,
  );
  if (!pick?.trim()) return;
  const chosen = candidates[Number(pick.trim()) - 1];
  if (!chosen) return;
  const ok = window.confirm(
    `确认合场？\n\n「${chosen.title}」的在场者将并入当前场景，故事时间取较晚的一路（${chosen.day}天 ${chosen.clock || "?"}）。被并入的场景归档留档。`,
  );
  if (!ok) return;
  sceneBusy.value = true;
  error.value = "";
  try {
    const view = await api.mergeScenes(props.meta.id, [chosen.id]);
    scenes.value = view.scenes;
    activeScene.value = view.active ?? activeScene.value;
    messages.value = await api.readMessages(props.meta.id);
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
  if (panel.value === "inspector") ensureInspector(); // M2.8：打开抽屉时拉一次检查器数据
}

async function scrollToBottom() {
  await nextTick();
  const el = streamEl.value;
  if (el) el.scrollTop = el.scrollHeight;
}

async function loadAll() {
  error.value = "";
  generating.value = false;
  streamText.value = "";
  editingIndex.value = -1;
  draft.value = "";
  page.value = 1;
  // 换会话：检查器数据作废（下次打开抽屉再拉）；事件流视图同理
  inspector.value = null;
  inspError.value = "";
  summaryResult.value = "";
  timeline.value = null;
  timelineError.value = "";
  const id = props.meta.id;
  try {
    const [bb, msgs] = await Promise.all([api.getBlackboard(id), api.readMessages(id)]);
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

/** 检查器：优先"最近一次发送"，无记录时干跑预览 */
async function refreshInspector() {
  try {
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
    assembly.value = await api.previewPrompt(props.meta.id);
    assemblySource.value = "preview";
  } catch (e) {
    error.value = String(e);
  }
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

// ---------- 发送 / 重roll / 停止 ----------

async function send() {
  const content = draft.value.trim();
  if (!content || generating.value) return;
  error.value = "";
  draft.value = "";
  resetComposerHeight();
  generating.value = true;
  streamText.value = "";
  // 乐观上屏；终态后以磁盘为准重读（带场景归属，切着场景时不出戏）
  messages.value = [
    ...messages.value,
    { turn: -1, role: "user", content, ts: 0, scene_id: activeScene.value || undefined },
  ];
  void jumpToLastPage();
  let final: StreamEvent;
  try {
    final = await api.sendMessage(props.meta.id, content, onDelta, speaker.value || undefined);
  } catch (e) {
    final = { event: "error", message: String(e) };
  }
  await finishGeneration(final);
}

async function reroll() {
  if (generating.value) return;
  error.value = "";
  generating.value = true;
  streamText.value = "";
  void jumpToLastPage();
  let final: StreamEvent;
  try {
    final = await api.regenerate(props.meta.id, onDelta);
  } catch (e) {
    final = { event: "error", message: String(e) };
  }
  await finishGeneration(final);
}

function onDelta(e: StreamEvent) {
  if (e.event === "delta") {
    streamText.value += e.text;
    void scrollToBottom();
  } else if (e.event === "hook_event") {
    // turn 由后端带上（产生该事件的钩子轮次）；没有就回退到本地推算
    pushHookEvent(e.kind, e.value, e.turn ?? -1);
  }
}

/** 界面事件进列表；轮次缺省时从消息流推算（乐观消息 turn=-1 不算数，M3.0 ③） */
function pushHookEvent(kind: string, value: string, turn = -1) {
  const hint = turn >= 0 ? turn : currentTurnHint();
  hookEvents.value = [{ kind, value, turn: hint }, ...hookEvents.value].slice(0, 50);
}

/** 推算「此刻在第几轮」：跳过乐观上屏的 turn=-1，取最后一条真实消息的轮次 */
function currentTurnHint(): number {
  for (let i = messages.value.length - 1; i >= 0; i--) {
    const t = messages.value[i].turn;
    if (t >= 0) return t;
  }
  return 0;
}

/** 钩子报告落地：卡内状态、记忆增量、日志 */
function applyReport(report: HookReport) {
  lastReport.value = report;
  if (report.card_state) cardState.value = report.card_state;
  hookLogs.value = report.logs ?? [];
  for (const e of report.ui_events) pushHookEvent(e.kind, e.value, report.turn);
}

/** 读卡内状态与记忆流（面板数据源） */
async function refreshCard() {
  try {
    const [state, mem] = await Promise.all([
      api.getCardState(props.meta.id),
      api.listCardMemory(props.meta.id),
    ]);
    cardState.value = state;
    memory.value = mem;
  } catch {
    /* 卡内数据读取失败不阻塞聊天 */
  }
}

/** 生成收尾：错误上报、消息与黑板重读（时钟已步进）、检查器刷新 */
async function finishGeneration(final: StreamEvent) {
  if (final.event === "error") error.value = final.message;
  if (final.event === "done" && final.report) applyReport(final.report);
  try {
    const [msgs, bb] = await Promise.all([
      api.readMessages(props.meta.id),
      api.getBlackboard(props.meta.id),
    ]);
    messages.value = [...msgs];
    blackboard.value = bb;
    Object.assign(bbForm, {
      day: bb.day,
      clock: bb.clock,
      place: bb.place,
      actors: bb.actors.join(", "),
    });
  } catch (e) {
    error.value = String(e);
  }
  generating.value = false;
  streamText.value = "";
  void refreshInspector();
  void refreshCard();
  // 刚聊完一轮：状态树/剧情线/心理/宫殿多半都变了，抽屉开着就顺手重拉（设计 §8/§9 的「面板可查」）
  if (panel.value === "inspector") {
    void loadInspector();
    if (inspTab.value === "events") void loadTimeline(); // 事件流页签：新事件即时可见
  }
  void scrollToBottom();
  composerEl.value?.focus();
}

async function stop() {
  await api.stopGeneration(props.meta.id);
}

// ---------- 消息编辑 / 删除 ----------

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

async function removeMsg(i: number) {
  if (!window.confirm("删除这条消息？")) return;
  try {
    messages.value = await api.deleteMessage(props.meta.id, i);
    if (editingIndex.value === i) editingIndex.value = -1;
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
    <div v-if="scenes.length > 0" class="card card-border flex-none bg-base-100">
      <div class="flex items-center gap-1 overflow-x-auto p-2">
        <button
          v-for="sc in scenes"
          :key="sc.id"
          class="btn btn-sm h-auto min-h-0 flex-col items-start gap-0 px-3 py-1.5 text-left"
          :class="[
            sc.id === activeScene ? 'btn-primary' : 'btn-ghost',
            sc.status === 'merged' ? 'btn-disabled opacity-50' : '',
          ]"
          :disabled="sc.status === 'merged' || sceneBusy"
          :data-tip="sceneTip(sc)"
          @click="switchTo(sc)"
        >
          <span class="flex items-center gap-1 text-xs font-semibold">
            {{ sc.title || sc.place || sc.id }}
            <span v-if="sc.status === 'frozen'" class="badge badge-xs badge-ghost">冻结</span>
            <span v-else-if="sc.status === 'merged'" class="badge badge-xs badge-ghost">已并入</span>
          </span>
          <span class="text-[10px] font-normal opacity-70">
            {{ sc.clock ? `第${sc.day}天 ${sc.clock}` : `第${sc.day}天` }} · {{ sc.place || "地点未定" }}
          </span>
        </button>
        <div class="ml-auto flex flex-none items-center gap-1 pl-2">
          <button
            class="btn btn-square btn-sm btn-ghost tooltip tooltip-bottom"
            data-tip="新场景：另起一个舞台（视角随即切过去）"
            :disabled="sceneBusy"
            @click="createSceneCmd"
          >
            <Icon name="plus" :size="15" />
          </button>
          <button
            class="btn btn-square btn-sm btn-ghost tooltip tooltip-bottom"
            data-tip="分场：挑人离场另立场景（「与此同时」）"
            :disabled="sceneBusy"
            @click="splitSceneCmd"
          >
            <Icon name="split" :size="15" />
          </button>
          <button
            class="btn btn-square btn-sm btn-ghost tooltip tooltip-bottom"
            data-tip="合场：把另一路场景并进当前场景（需确认）"
            :disabled="sceneBusy"
            @click="mergeSceneCmd"
          >
            <Icon name="merge" :size="15" />
          </button>
        </div>
      </div>
    </div>

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
              <div class="w-9 rounded-full" :class="avatarClass(m)">
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

          <!-- 流式中的角色回复（只在最后一页显示） -->
          <li v-if="generating && isLastPage" class="chat chat-start">
            <div class="chat-image avatar avatar-placeholder">
              <div class="w-9 rounded-full bg-neutral text-neutral-content">
                <span class="text-xs">{{ initial(cardName) }}</span>
              </div>
            </div>
            <div class="chat-header mb-0.5 text-xs font-medium text-base-content/60">{{ cardName }}</div>
            <div
              class="chat-bubble max-w-[min(76%,72ch)] border border-primary/40 bg-base-300 leading-relaxed break-words whitespace-pre-wrap text-base-content"
            >
              {{ streamText }}<span class="ml-0.5 inline-block h-[1em] w-0.5 animate-pulse bg-primary align-[-0.15em]" aria-hidden="true"></span>
            </div>
            <div class="chat-footer mt-1 flex items-center gap-2 text-[11px] text-base-content/45">
              <span class="loading loading-dots loading-xs text-primary"></span>
              正在写…
            </div>
          </li>

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
            第 {{ page }}/{{ pageCount }} 页 · 共 {{ messages.length }} 条 · 每页 {{ PAGE_SIZE }} 条
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

        <!-- 输入区：daisyUI textarea + 圆形发送键；多角色时带发言人选择（M3.1 隔离模式） -->
        <form class="flex-none border-t border-base-300 p-3" @submit.prevent="send">
          <div v-if="speakers.length > 1" class="mb-2 flex flex-wrap items-center gap-1.5">
            <span class="text-[11px] text-base-content/45">对谁说</span>
            <div role="tablist" class="tabs tabs-box tabs-xs">
              <button
                v-for="dir in speakers"
                :key="dir"
                role="tab"
                class="tab"
                :class="{ 'tab-active': speaker === dir }"
                @click="speaker = dir"
              >
                {{ dir }}
              </button>
            </div>
          </div>
          <div class="flex items-end gap-2">
            <textarea
              ref="composerEl"
              class="textarea max-h-40 w-full flex-1 resize-none leading-relaxed"
              v-model="draft"
              rows="1"
              :placeholder="speakers.length > 1 ? `对${speaker || '谁'}说点什么…` : '说点什么…'"
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

        <div v-else class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4">
          <!-- 检查器页签：M1.6 的四个可观测面 + M2.8 的六个记忆检查器面板 -->
          <div role="tablist" class="tabs tabs-border tabs-xs flex-none">
            <button
              v-for="t in INSP_TABS"
              :key="t.id"
              role="tab"
              class="tab"
              :class="{ 'tab-active': inspTab === t.id }"
              @click="inspTab = t.id"
            >
              {{ t.label }}
            </button>
          </div>

          <template v-if="inspTab === 'layers'">
          <div class="flex items-center justify-between gap-2">
            <p v-if="assembly" class="m-0 text-xs text-base-content/50">
              {{ assemblySource === "last" ? "最近一次发送" : "预览 · 干跑" }}
            </p>
            <button class="btn btn-ghost btn-xs flex-none" @click="preview">
              <Icon name="refresh" :size="13" />刷新
            </button>
          </div>

          <template v-if="assembly">
            <!-- 预算总账（M2.7 · 设计 §4.2）：输入预算 / 实际占用 / 逐层用量 -->
            <div v-if="assembly.budget" class="rounded-box bg-base-200 px-3 py-2.5">
              <div class="flex items-center justify-between gap-2">
                <span class="text-[11px] text-base-content/45">输入预算（上下文 × 75%）</span>
                <span class="font-mono text-[11px] text-base-content/60">
                  {{ assembly.budget.used_tokens }} / {{ assembly.budget.input_tokens }}
                </span>
              </div>
              <progress
                class="progress progress-primary mt-1.5 h-1.5 w-full"
                :value="assembly.budget.used_tokens"
                :max="assembly.budget.input_tokens || 1"
              ></progress>
              <div class="mt-2.5 flex flex-col gap-2">
                <div v-for="u in assembly.budget.layers" :key="u.id + u.name" class="flex flex-col gap-1">
                  <div class="flex items-center gap-2 text-[11px]">
                    <span
                      class="badge badge-xs badge-soft font-mono"
                      :class="u.trimmed ? 'badge-warning' : 'badge-primary'"
                      >{{ u.id }}</span
                    >
                    <span class="flex-1 truncate text-base-content/60">{{ u.name }}</span>
                    <span v-if="u.trimmed" class="badge badge-xs badge-soft badge-warning">被裁</span>
                    <span class="font-mono text-base-content/45">{{ u.tokens }} / {{ u.limit }}</span>
                  </div>
                  <progress
                    class="progress h-1 w-full"
                    :class="u.trimmed ? 'progress-warning' : 'progress-primary'"
                    :value="u.tokens"
                    :max="u.limit || 1"
                  ></progress>
                  <p v-if="u.trimmed" class="m-0 text-[11px] text-warning">{{ u.trimmed }}</p>
                </div>
              </div>
            </div>

            <div class="grid grid-cols-3 gap-2">
              <div class="rounded-box bg-base-200 px-3 py-2">
                <p class="m-0 text-[11px] text-base-content/45">注入层</p>
                <p class="m-0 text-lg leading-tight font-semibold">{{ assembly.layers.length }}</p>
              </div>
              <div class="rounded-box bg-base-200 px-3 py-2">
                <p class="m-0 text-[11px] text-base-content/45">估算 token</p>
                <p class="m-0 text-lg leading-tight font-semibold">{{ assembly.total_tokens }}</p>
              </div>
              <div class="rounded-box bg-base-200 px-3 py-2">
                <p class="m-0 text-[11px] text-base-content/45">消息数</p>
                <p class="m-0 text-lg leading-tight font-semibold">{{ assembly.messages.length }}</p>
              </div>
            </div>

            <div
              v-for="l in assembly.layers"
              :key="l.id + l.name"
              class="collapse collapse-arrow rounded-box bg-base-200"
            >
              <input type="checkbox" />
              <div class="collapse-title flex min-h-0 items-center gap-2.5 px-3 py-2 text-[13px]">
                <span class="badge badge-xs badge-soft badge-primary font-mono">{{ l.id }}</span>
                <span class="flex-1 truncate">{{ l.name }}</span>
                <!-- 被预算裁/截断过的层标出来（M2.7；说明在预算总账里） -->
                <span
                  v-if="layerTrimmed(l.id, l.name)"
                  class="badge badge-xs badge-soft badge-warning"
                  :title="layerTrimmed(l.id, l.name)"
                  >被裁</span
                >
                <span class="font-mono text-[11px] text-base-content/45">≈{{ l.tokens }}</span>
              </div>
              <div class="collapse-content px-3">
                <pre class="m-0 rounded-box border border-base-300 bg-base-100 p-2.5 text-xs leading-relaxed break-words whitespace-pre-wrap">{{ l.content }}</pre>
                <!-- 逐卡激活原因（设计 §6.11：每张注入卡都说得清「它为什么在这里」） -->
                <ul v-if="l.sources?.length" class="m-0 mt-1.5 flex list-none flex-col gap-1 p-0">
                  <li
                    v-for="s in l.sources"
                    :key="s"
                    class="rounded-box bg-base-200 px-2 py-1 font-mono text-[11px] text-base-content/60"
                  >
                    {{ s }}
                  </li>
                </ul>
              </div>
            </div>
          </template>
          <p v-else class="m-0 text-xs text-base-content/50">尚无组装数据。</p>
          </template>

          <!-- M2.8 记忆检查器面板组：一份 inspectorData 喂六个页签（设计 §14） -->
          <template v-else-if="M2_TABS.includes(inspTab)">
            <div class="flex items-center justify-between gap-2">
              <p class="m-0 min-w-0 truncate text-xs text-base-content/50">
                {{ inspTabLabel }} · {{ inspector?.character || cardName }}
              </p>
              <button class="btn btn-ghost btn-xs flex-none" :disabled="inspLoading" @click="loadInspector">
                <span v-if="inspLoading" class="loading loading-spinner loading-xs"></span>
                <Icon v-else name="refresh" :size="13" />刷新
              </button>
            </div>

            <!-- 读失败只在这里提示：不打断聊天（浏览器 mock 未覆盖该命令时也走这里） -->
            <div v-if="inspError" role="alert" class="alert alert-error alert-soft py-2 text-xs break-words">
              {{ inspError }}
            </div>
            <p v-else-if="!inspector" class="m-0 flex items-center gap-2 text-xs text-base-content/50">
              <span v-if="inspLoading" class="loading loading-spinner loading-xs"></span>
              {{ inspLoading ? "正在读记忆检查器…" : "还没有检查器数据，点「刷新」重拉。" }}
            </p>

            <template v-else>
              <StatePathPanel
                v-if="inspTab === 'statetree'"
                :tree="inspector.stateTree"
                :transitions="inspector.transitions"
              />
              <ThreadsPanel
                v-else-if="inspTab === 'threads'"
                :threads="inspector.threads"
                :busy="threadBusy"
                @open="openThread"
                @resolve="resolveThreadCmd"
              />
              <PsychePanel v-else-if="inspTab === 'psyche'" :psyche="inspector.psyche" />
              <PalacePanel v-else-if="inspTab === 'palace'" :palace="inspector.palace" @jump="jumpToTurn" />
              <CodexPanel
                v-else-if="inspTab === 'codex'"
                :codex="inspector.codex"
                :known="inspector.known"
                :active-entities="inspector.activeEntities"
              />
              <SummaryPanel
                v-else
                :summary="inspector.summary"
                :proposals="inspector.proposals"
                :summarizing="summarizing"
                :result="summaryResult"
                :deciding="decidingId"
                @summarize="summarizeNow"
                @decide="decideProposal"
              />
            </template>
          </template>

          <!-- 卡内状态：角色私有 state（state.json，hook 每轮维护） -->
          <template v-else-if="inspTab === 'state'">
            <div class="flex items-center justify-between gap-2">
              <p class="m-0 text-xs text-base-content/50">
                卡片私有状态（state.json）· 重启不丢
              </p>
              <button class="btn btn-ghost btn-xs flex-none" @click="refreshCard">
                <Icon name="refresh" :size="13" />刷新
              </button>
            </div>
            <dl v-if="stateRows.length" class="m-0 flex flex-col gap-2">
              <div
                v-for="row in stateRows"
                :key="row.key"
                class="rounded-box flex items-center justify-between gap-3 bg-base-200 px-3 py-2"
              >
                <dt class="font-mono text-xs text-base-content/60">{{ row.key }}</dt>
                <dd class="m-0 truncate text-sm font-medium" :title="row.value">{{ row.value }}</dd>
              </div>
            </dl>
            <p v-else class="m-0 text-xs text-base-content/50">这张卡没有 state（静态卡）。</p>
            <p v-if="lastReport && !lastReport.ran" class="m-0 text-[11px] text-base-content/40">
              卡片未定义 on_message 钩子，状态不随对话变化。
            </p>
          </template>

          <!-- 卡内记忆：api.memory.set 的写入流（palace.jsonl） -->
          <template v-else-if="inspTab === 'memory'">
            <div class="flex items-center justify-between gap-2">
              <p class="m-0 text-xs text-base-content/50">
                卡内长期记忆写入（palace.jsonl）· 召回在 M2 接记忆宫殿
              </p>
              <button class="btn btn-ghost btn-xs flex-none" @click="refreshCard">
                <Icon name="refresh" :size="13" />刷新
              </button>
            </div>
            <ul v-if="memoryRows.length" class="m-0 flex list-none flex-col gap-2 p-0">
              <li
                v-for="(rec, i) in memoryRows"
                :key="`${rec.ts}-${rec.key}-${i}`"
                class="rounded-box flex items-center justify-between gap-3 bg-base-200 px-3 py-2"
              >
                <div class="min-w-0">
                  <p class="m-0 truncate font-mono text-xs">{{ rec.key }}</p>
                  <p class="m-0 text-[11px] text-base-content/45">
                    第 {{ rec.turn }} 轮 · {{ rec.source }} · {{ fmtTs(rec.ts) }}
                  </p>
                </div>
                <span class="badge badge-sm badge-soft flex-none">{{ fmtValue(rec.value) }}</span>
              </li>
            </ul>
            <p v-else class="m-0 text-xs text-base-content/50">还没有记忆写入。</p>
          </template>

          <!-- 事件流：类型化事件流（session_timeline）+ 界面事件（api.ui.emit）+ 卡内错误 -->
          <template v-else>
            <div class="flex items-center justify-between gap-2">
              <p class="m-0 text-xs text-base-content/50">
                类型化事件流（messages.jsonl · 最新在前）
              </p>
              <button class="btn btn-ghost btn-xs flex-none" :disabled="timelineLoading" @click="loadTimeline">
                <span v-if="timelineLoading" class="loading loading-spinner loading-xs"></span>
                <Icon v-else name="refresh" :size="13" />刷新
              </button>
            </div>
            <div
              v-if="timelineError"
              role="alert"
              class="alert alert-error alert-soft py-2 text-xs break-words"
            >
              {{ timelineError }}
            </div>
            <p v-else-if="!timeline" class="m-0 text-xs text-base-content/50">读取中…</p>
            <ul v-else-if="timeline.length" class="m-0 flex list-none flex-col gap-1 p-0">
              <li
                v-for="e in timeline"
                :key="e.seq"
                class="rounded-box flex items-center gap-2 bg-base-200 px-2.5 py-1.5 text-xs"
              >
                <span class="badge badge-xs badge-soft flex-none font-mono">{{ e.kind }}</span>
                <span class="min-w-0 flex-1 break-words">{{ e.brief }}</span>
                <span class="ml-auto flex-none font-mono text-[10px] text-base-content/35">
                  #{{ e.seq }} · 第 {{ e.turn }} 轮
                </span>
              </li>
            </ul>
            <p v-else class="m-0 text-xs text-base-content/50">事件流还是空的。</p>

            <div class="collapse collapse-arrow rounded-box bg-base-200">
              <input type="checkbox" />
              <div class="collapse-title min-h-0 px-3 py-2 text-xs">
                界面事件（api.ui.emit · {{ hookEvents.length }}）
              </div>
              <div class="collapse-content px-3">
                <div class="mb-1 flex justify-end">
                  <button class="btn btn-ghost btn-xs" @click="hookEvents = []">清空</button>
                </div>
                <ul v-if="hookEvents.length" class="m-0 flex list-none flex-col gap-1.5 p-0">
                  <li
                    v-for="(ev, i) in hookEvents"
                    :key="`${ev.turn}-${ev.kind}-${ev.value}-${i}`"
                    class="rounded-box flex items-center gap-2 bg-base-100 px-2.5 py-1.5 text-xs"
                  >
                    <span class="badge badge-xs badge-soft badge-primary">{{ kindLabel(ev.kind) }}</span>
                    <span class="truncate">{{ ev.value }}</span>
                    <span class="ml-auto flex-none text-[11px] text-base-content/40">第 {{ ev.turn }} 轮</span>
                  </li>
                </ul>
                <p v-else class="m-0 text-xs text-base-content/50">还没有界面事件。</p>
              </div>
            </div>

            <template v-if="hookLogs.length">
              <p class="m-0 mt-1 text-xs text-base-content/50">卡内错误（不打断对话）</p>
              <div
                v-for="(log, i) in hookLogs"
                :key="i"
                class="alert alert-error alert-soft py-2 text-xs whitespace-pre-wrap"
              >
                {{ log }}
              </div>
            </template>
          </template>
        </div>
      </aside>
    </div>
  </div>
</template>
